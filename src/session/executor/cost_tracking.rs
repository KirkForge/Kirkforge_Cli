//! Cost, usage, and session-learning tracker.
//!
//! Groups the fields that track token costs, prompt-cache stem stability,
//! doom-loop detection, and cross-session carryover into a single
//! sub-struct owned by [`super::Executor`].

use crate::session::carryover::CarryoverProfile;
use crate::session::executor::loop_::DoomLoopTracker;
use crate::session::executor::types::TurnEvent;
use crate::session::prompt::cache_stem::CacheStemTracker;
use crate::shared::metrics::{record, MetricEvent};
use crate::shared::ToolOutcome;
use std::sync::Arc;
use tokio::sync::mpsc;

use super::helpers::tool_outcome_success;

/// Signal returned when the doom-loop circuit breaker fires.
/// The caller should auto-switch to plan mode (if not already in it)
/// or halt the turn (if already in plan mode).
#[allow(dead_code)] // fields read by callers through TurnEvent construction
pub(crate) struct DoomLoopRemediation {
    pub action: String,
    pub hits: usize,
}

pub(crate) struct CostTracker {
    pub(crate) usage: crate::shared::CostTracking,
    pub(crate) doom_loop_tracker: DoomLoopTracker,
    pub(crate) doom_loop_hits: usize,
    pub(crate) cache_stem: CacheStemTracker,
    pub(crate) carryover: CarryoverProfile,
    pub(crate) carryover_enabled: bool,
    pub(crate) carryover_target: Option<Arc<std::sync::Mutex<CarryoverProfile>>>,
}

impl CostTracker {
    pub(crate) fn new(carryover_enabled: bool) -> Self {
        let carryover = if carryover_enabled {
            crate::session::carryover::load_carryover()
        } else {
            CarryoverProfile::default()
        };
        Self {
            usage: crate::shared::CostTracking::default(),
            doom_loop_tracker: DoomLoopTracker::new(),
            doom_loop_hits: 0,
            cache_stem: CacheStemTracker::new(),
            carryover,
            carryover_enabled,
            carryover_target: None,
        }
    }

    /// Feed a tool outcome to the doom-loop detector. If the threshold is
    /// crossed, emit a `TurnEvent::DoomLoopDetected` on `event_tx` and
    /// a `MetricEvent::DoomLoop` to the metrics log. Returns `Some(hint)`
    /// to inject into the conversation so the model changes strategy.
    /// If `doom_loop_max_hits > 0` and cumulative hits have reached the
    /// limit, also emits `TurnEvent::DoomLoopRemediation` and returns
    /// a `DoomLoopRemediation` so the caller can auto-switch to plan mode
    /// or halt the turn.
    pub(crate) fn observe_tool_outcome(
        &mut self,
        tool: &str,
        outcome: &ToolOutcome,
        event_tx: &mpsc::Sender<TurnEvent>,
        doom_loop_max_hits: usize,
    ) -> (Option<String>, Option<DoomLoopRemediation>) {
        let is_error = !tool_outcome_success(outcome);
        let error_text = if is_error {
            let mut s = String::new();
            match outcome {
                ToolOutcome::Error { message } => {
                    s.push_str(message);
                }
                ToolOutcome::Failure(err) => {
                    s.push_str(&err.to_user_message());
                }
                _ => {
                    self.doom_loop_tracker.reset();
                    return (None, None);
                }
            }
            s
        } else {
            self.doom_loop_tracker.reset();
            return (None, None);
        };

        if let Some(hit) = self.doom_loop_tracker.observe(tool, &error_text) {
            self.doom_loop_hits += 1;
            record(MetricEvent::DoomLoop {
                count: hit.count,
                tool: hit.tool.clone(),
                last_error: hit.last_error.clone(),
            });
            if let Err(e) = event_tx.try_send(TurnEvent::DoomLoopDetected {
                count: hit.count,
                tool: hit.tool.clone(),
                last_error: hit.last_error.clone(),
            }) {
                tracing::warn!(error = %e, "failed to send DoomLoopDetected to TUI");
            }
            let hint = Some(format!(
                "[System: tool '{}' has failed {} times with the same error. Try a different approach or ask the user for help.]",
                hit.tool, hit.count
            ));

            // Circuit breaker: if cumulative hits reach the configured max,
            // fire a remediation event. The caller (turn executor) will
            // auto-switch to plan mode or halt the turn.
            if doom_loop_max_hits > 0 && self.doom_loop_hits >= doom_loop_max_hits {
                let action = "auto_plan_mode";
                tracing::warn!(
                    hits = self.doom_loop_hits,
                    max = doom_loop_max_hits,
                    "doom-loop circuit breaker firing"
                );
                if let Err(e) = event_tx.try_send(TurnEvent::DoomLoopRemediation {
                    action: action.to_string(),
                    hits: self.doom_loop_hits,
                }) {
                    tracing::warn!(error = %e, "failed to send DoomLoopRemediation to TUI");
                }
                return (
                    hint,
                    Some(DoomLoopRemediation {
                        action: action.to_string(),
                        hits: self.doom_loop_hits,
                    }),
                );
            }

            (hint, None)
        } else {
            (None, None)
        }
    }

    /// Flush the carryover profile to disk (if enabled) and push it to
    /// the shared target so the next session can pick it up.
    pub(crate) fn flush_carryover(&mut self) {
        if self.carryover_enabled {
            self.carryover.session_count += 1;
            self.carryover.last_session_time =
                chrono::Local::now().format("%Y-%m-%d %H:%M").to_string();
            self.carryover.refresh_patterns();
            if let Some(ref target) = self.carryover_target {
                if let Ok(mut guard) = target.lock() {
                    *guard = self.carryover.clone();
                }
            }
        }
    }

    /// Record a tool call and verifier corrections in the carryover profile.
    pub(crate) fn collect_carryover(
        &mut self,
        tc: &crate::shared::ToolInvocation,
        crs: &[crate::session::verifier::CorrectionResult],
    ) {
        if !self.carryover_enabled {
            return;
        }
        self.carryover.record_tool_call(&tc.name);

        if let Some(path) = tc.arguments.get("path").and_then(|v| v.as_str()) {
            if !path.is_empty() {
                self.carryover.record_path(path);
            }
        }

        if tc.name == "bash" {
            if let Some(cmd) = tc.arguments.get("command").and_then(|v| v.as_str()) {
                if cmd.contains("cargo test")
                    || cmd.contains("cargo check")
                    || cmd.contains("go test")
                    || cmd.contains("npm test")
                    || cmd.contains("pytest")
                    || cmd.contains("make test")
                {
                    self.carryover.record_test_after_change();
                }
            }
        }

        for cr in crs {
            self.carryover.record_verifier_warning(&cr.message);
        }
    }
}
