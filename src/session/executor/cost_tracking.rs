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

/// Configurable doom-loop remediation action.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DoomLoopAction {
    AutoPlan,
    Halt,
    WarnOnly,
}

impl std::str::FromStr for DoomLoopAction {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "auto_plan" => Ok(Self::AutoPlan),
            "halt" => Ok(Self::Halt),
            "warn_only" => Ok(Self::WarnOnly),
            _ => Err(format!(
                "unknown doom_loop_action '{s}': expected 'auto_plan', 'halt', or 'warn_only'"
            )),
        }
    }
}

/// Outcome returned when a doom loop is detected.
pub struct DoomLoopOutcome {
    pub hint: String,
    pub action: DoomLoopAction,
    pub count: usize,
    pub tool: String,
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
    /// a `MetricEvent::DoomLoop` to the metrics log. Returns
    /// `Some(DoomLoopOutcome)` with the hint and requested action when
    /// the circuit breaker fires (cumulative hits >= doom_loop_max_hits).
    pub(crate) fn observe_tool_outcome(
        &mut self,
        tool: &str,
        outcome: &ToolOutcome,
        event_tx: &mpsc::Sender<TurnEvent>,
        doom_loop_max_hits: usize,
        doom_action: DoomLoopAction,
    ) -> Option<DoomLoopOutcome> {
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
                    return None;
                }
            }
            s
        } else {
            self.doom_loop_tracker.reset();
            return None;
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

            // Circuit breaker: if cumulative hits reach the configured max,
            // emit remediation event and return DoomLoopOutcome.
            if doom_loop_max_hits > 0 && self.doom_loop_hits >= doom_loop_max_hits {
                let hint = format!(
                    "[System: tool '{}' has failed {} times with the same error. Try a different approach or ask the user for help.]",
                    hit.tool, hit.count
                );
                tracing::warn!(
                    hits = self.doom_loop_hits,
                    max = doom_loop_max_hits,
                    action = ?doom_action,
                    "doom-loop circuit breaker firing"
                );
                if let Err(e) = event_tx.try_send(TurnEvent::DoomLoopRemediation {
                    action: format!("{:?}", doom_action).to_lowercase(),
                    hits: self.doom_loop_hits,
                }) {
                    tracing::warn!(error = %e, "failed to send DoomLoopRemediation to TUI");
                }
                return Some(DoomLoopOutcome {
                    hint,
                    action: doom_action,
                    count: self.doom_loop_hits,
                    tool: hit.tool,
                });
            }

            None
        } else {
            None
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn doom_loop_action_from_str_valid() {
        assert_eq!("auto_plan".parse::<DoomLoopAction>().unwrap(), DoomLoopAction::AutoPlan);
        assert_eq!("halt".parse::<DoomLoopAction>().unwrap(), DoomLoopAction::Halt);
        assert_eq!("warn_only".parse::<DoomLoopAction>().unwrap(), DoomLoopAction::WarnOnly);
    }

    #[test]
    fn doom_loop_action_from_str_invalid() {
        assert!("banish".parse::<DoomLoopAction>().is_err());
    }
}
