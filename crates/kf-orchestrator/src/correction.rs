//! Correction loop (R4). Port of `orchestrator/src/orchestrator-correction.ts`.
//!
//! The impure counterpart to `kf_routing::correction::decide_correction`:
//! runs delegate → validator → decide across `0..=max_corrections` turns,
//! accumulates cost, and writes a memory observation via the orchestrator's
//! `MemoryStore`. The reducer (TS `StateReducer`) is NOT ported here — the
//! loop reads `delegate_result.packet` (a default `ReducedStatePacket` until
//! the reducer ports) and the external validator outcome.
//!
//! DEFERRED (per WO 29.7): shell/structured validator execution; the loop
//! only consults `task.task_pass` for now. Wiring `ValidatorConfig` to a
//! `tokio::process::Command` is the follow-up.

use anyhow::Result;
use tracing::warn;

use kf_memory_store::MemoryStore;
use kf_routing::correction::{
    compute_final_verdict, decide_correction, CorrectionAction, FinalAction, FinalVerdict,
    SourceOfTruth, TruthInput, TruthProfile, ValidationStatus,
};
use kf_routing::cost::{estimate_simple_cost, resolve_cost_provider_key};

use crate::correction_loop_helpers::task_outcome_from_validation;
use crate::model::ModelClient;
use crate::types::{CorrectionLoopConfig, CorrectionLoopOutcome, TaskInput, TaskValidationResult};

/// Callback shape for the loop's per-turn delegate call. Mirrors
/// `decompose::DelegateFn` but kept separate because the loop runs in a
/// tight retry cycle that benefits from a distinct name.
#[async_trait::async_trait]
pub trait LoopDelegate: Send + Sync {
    /// Run one delegation turn. `task` may have been mutated by the loop
    /// (description extended with the correction prompt, task_id bumped).
    async fn delegate_turn(&self, task: TaskInput) -> Result<crate::types::DelegationResult>;
}

/// Run the correction loop. Each turn calls `delegate.delegate_turn(task)`,
/// optionally consults the external validator (`task.task_pass`), then feeds
/// the resulting state into `decide_correction`. Stops on accept, escalate,
/// or `max_corrections`.
///
/// The model seam (`client`) is only used when a correction prompt needs to
/// be generated and there is no other source; currently the loop just
/// appends `decision.correction_prompt` to the task description. The
/// `client` arg is kept for parity with TS so the future reducer/prompt
/// builder can use it without an API change.
pub async fn run_correction_loop(
    delegate: &dyn LoopDelegate,
    _client: Option<&dyn ModelClient>,
    memory: Option<&MemoryStore>,
    mut task: TaskInput,
    config: &CorrectionLoopConfig,
) -> Result<CorrectionLoopOutcome> {
    let original_description = task.description.clone();
    let base_id = task
        .task_id
        .clone()
        .unwrap_or_else(|| format!("task-{}", now_millis()));
    let mut task_id = base_id.clone();
    let original_profile = kf_routing::detect_task_profile(&original_description);

    let mut session_tokens: i64 = 0;
    let mut session_cost: f64 = 0.0;
    let mut last_provider: Option<String> = None;
    let mut last_packet: Option<kf_routing::correction::ReducedStatePacket> = None;
    let mut last_emission_format: String = "unknown".into();
    let mut last_emission_model: String = "unknown".into();
    let loop_started = now_millis();
    let mut task_validation = TaskValidationResult::skipped("none", "no task validator configured");

    let mut final_action = FinalAction::Escalate;

    for turn in 0..=config.max_corrections {
        let result = match delegate.delegate_turn(task.clone()).await {
            Ok(r) => r,
            Err(e) => {
                warn!("delegation failed on turn {turn}: {e}");
                // Match TS: any delegation failure forces an escalate result.
                break;
            }
        };

        // Track provider for cost lookup; default to local-ollama when unknown.
        let provider_key = result
            .provider_resolved
            .clone()
            .unwrap_or_else(|| "local-ollama".into());
        last_provider = Some(provider_key.clone());
        let emission = &result.emission;
        let worker_tokens = emission.total_tokens;
        session_tokens += worker_tokens;
        last_emission_format = emission.format.clone();
        last_emission_model = emission.model.clone();
        let cost_key = resolve_cost_provider_key(&provider_key);
        session_cost +=
            estimate_simple_cost(cost_key, emission.prompt_tokens, emission.completion_tokens);
        last_packet = result.packet.clone();

        // External validator hook: DEFERRED. For now, the only signal is the
        // caller's `task.task_pass` flag (set by the harness or external
        // caller). If `ValidatorConfig` is configured, the future impl would
        // run it here and overwrite task_validation.
        if let Some(cfg) = &config.validator {
            // ponytail: deferred — ValidatorConfig parsing is not yet wired.
            // Mark task_validation as skipped-with-validator-pending so the
            // loop still falls through to decide_correction.
            let _ = cfg;
            task_validation = TaskValidationResult {
                status: "skipped".into(),
                validator: "pending-wo-29.7-followup".into(),
                reason: Some("validator configured but execution is deferred".into()),
                duration_ms: 0,
            };
        }

        let task_pass = match task_validation.status.as_str() {
            "pass" => Some(true),
            "fail" => Some(false),
            _ => task.task_pass,
        };

        let decision = decide_correction(
            last_packet.as_ref().unwrap_or(&Default::default()),
            turn,
            config.max_corrections,
            worker_tokens,
            session_tokens,
            session_cost,
            config.max_cost,
            task_pass,
        );

        match decision.action {
            CorrectionAction::Accept => {
                final_action = FinalAction::Accept;
                break;
            }
            CorrectionAction::Escalate => {
                final_action = FinalAction::Escalate;
                break;
            }
            CorrectionAction::Correct => {
                let next_id = format!("{base_id}-c{}", turn + 1);
                let correction_prompt = decision.correction_prompt.clone();
                let feedback = if task_validation.status == "fail" {
                    format!(
                        "\n\nExternal task validator ({}) {}: {}",
                        task_validation.validator,
                        task_validation.status,
                        task_validation.reason.as_deref().unwrap_or("no reason")
                    )
                } else {
                    String::new()
                };
                let correction = correction_prompt.unwrap_or_default();
                task.description = format!("{}\n\n{}{}", task.description, correction, feedback);
                task.task_id = Some(next_id.clone());
                task_id = next_id;
            }
        }
    }

    // Validator takes precedence over verifier for final action.
    if config.validator.is_some()
        && task_validation.status != "pass"
        && final_action == FinalAction::Accept
    {
        final_action = FinalAction::Escalate;
    }

    // Truth-model precedence (single source of truth).
    let truth = compute_final_verdict(&TruthInput {
        task_validation: kf_routing_validation(&task_validation),
        has_validator: config.validator.is_some(),
        final_action,
        packet: last_packet.as_ref(),
        profile: TruthProfile {
            language: original_profile.language.as_str().to_string(),
            validator_required: original_profile.validator_required,
        },
        actual_mode: last_emission_format.clone(),
        protocol_broken: false,
    });

    // Persist the observation (best-effort).
    if let Some(store) = memory {
        let _ = store.write_task_observation(&kf_memory_store::types::TaskObservationInput {
            task_id: task_id.clone(),
            description: original_description.clone(),
            language: original_profile.language.as_str().into(),
            mode: last_emission_format.clone(),
            model: last_emission_model.clone(),
            provider_key: last_provider.clone(),
            final_action: Some(final_action_str(final_action).into()),
            task_pass: match task_validation.status.as_str() {
                "pass" => Some(true),
                "fail" => Some(false),
                _ => None,
            },
            outcome: Some(truth.final_verdict_str().into()),
            tokens: session_tokens,
            duration_ms: now_millis() - loop_started,
            turns: Some(config.max_corrections + 1),
            final_verdict: Some(truth.final_verdict_str().into()),
            source_of_truth: Some(source_of_truth_str(truth.source_of_truth).into()),
            ..Default::default()
        });
    }

    Ok(CorrectionLoopOutcome {
        final_action: final_action_str(final_action).into(),
        final_verdict: truth.final_verdict_str().into(),
        source_of_truth: source_of_truth_str(truth.source_of_truth).into(),
        task_outcome: task_outcome_from_validation(&task_validation).into(),
        session_tokens,
        session_cost,
        validator_duration_ms: task_validation.duration_ms,
        task_validation,
    })
}

fn final_action_str(a: FinalAction) -> &'static str {
    match a {
        FinalAction::Accept => "accept",
        FinalAction::Escalate => "escalate",
    }
}

fn source_of_truth_str(s: SourceOfTruth) -> &'static str {
    match s {
        SourceOfTruth::TaskValidator => "task-validator",
        SourceOfTruth::Verifier => "verifier",
    }
}

fn kf_routing_validation(v: &TaskValidationResult) -> kf_routing::correction::TaskValidationResult {
    let status = match v.status.as_str() {
        "pass" => ValidationStatus::Pass,
        "fail" => ValidationStatus::Fail,
        "error" => ValidationStatus::Error,
        "skipped" => ValidationStatus::Skipped,
        _ => ValidationStatus::Other,
    };
    kf_routing::correction::TaskValidationResult { status }
}

fn now_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

trait FinalVerdictExt {
    fn final_verdict_str(&self) -> &'static str;
}

impl FinalVerdictExt for kf_routing::correction::TruthOutput {
    fn final_verdict_str(&self) -> &'static str {
        match self.final_verdict {
            FinalVerdict::Pass => "pass",
            FinalVerdict::Fail => "fail",
            FinalVerdict::Error => "error",
            FinalVerdict::Unknown => "unknown",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{DelegationResult, Emission};
    use async_trait::async_trait;
    use std::sync::Mutex;

    fn make_result(pass: bool, total_tokens: i64) -> DelegationResult {
        let mut packet = kf_routing::correction::ReducedStatePacket::default();
        packet.verification.overall = if pass {
            kf_routing::correction::OverallVerdict::Pass
        } else {
            kf_routing::correction::OverallVerdict::Fail
        };
        DelegationResult {
            decision: crate::types::DelegationDecisionInfo {
                mode: "hard-prompt".into(),
                reason: "test".into(),
                auto_routed: true,
            },
            emission: Emission {
                agent_id: "agent".into(),
                content: "```python\nprint(1)\n```".into(),
                model: "test-model".into(),
                format: "hard-prompt".into(),
                total_tokens,
                ..Default::default()
            },
            signals: vec![],
            packet: Some(packet),
            provider_resolved: Some("local-ollama".into()),
            skills_loaded: None,
        }
    }

    struct AlwaysPassDelegate;
    #[async_trait]
    impl LoopDelegate for AlwaysPassDelegate {
        async fn delegate_turn(&self, _task: TaskInput) -> Result<DelegationResult> {
            Ok(make_result(true, 100))
        }
    }

    struct AlwaysFailThenPass {
        count: Mutex<i64>,
        fail_count: i64,
    }
    #[async_trait]
    impl LoopDelegate for AlwaysFailThenPass {
        async fn delegate_turn(&self, _task: TaskInput) -> Result<DelegationResult> {
            let mut c = self.count.lock().unwrap();
            *c += 1;
            let pass = *c > self.fail_count;
            Ok(make_result(pass, 100))
        }
    }

    struct ErrDelegate;
    #[async_trait]
    impl LoopDelegate for ErrDelegate {
        async fn delegate_turn(&self, _task: TaskInput) -> Result<DelegationResult> {
            anyhow::bail!("network down")
        }
    }

    #[tokio::test]
    async fn pass_on_turn_zero_accepts() {
        let cfg = CorrectionLoopConfig {
            max_corrections: 3,
            ..Default::default()
        };
        // Description must trigger a non-Text profile so validator_required=false
        // (otherwise the truth model returns Unknown via precedence 3).
        let out = run_correction_loop(
            &AlwaysPassDelegate,
            None,
            None,
            TaskInput {
                description: "fix the typescript lint errors in the auth module".into(),
                ..Default::default()
            },
            &cfg,
        )
        .await
        .unwrap();
        assert_eq!(out.final_action, "accept");
        assert_eq!(out.final_verdict, "pass");
        assert_eq!(out.source_of_truth, "verifier");
        assert_eq!(out.session_tokens, 100);
    }

    #[tokio::test]
    async fn fails_then_passes_within_budget() {
        let cfg = CorrectionLoopConfig {
            max_corrections: 3,
            ..Default::default()
        };
        let d = AlwaysFailThenPass {
            count: Mutex::new(0),
            fail_count: 1,
        };
        let out = run_correction_loop(
            &d,
            None,
            None,
            TaskInput {
                description: "fix the typescript lint errors".into(),
                ..Default::default()
            },
            &cfg,
        )
        .await
        .unwrap();
        assert_eq!(out.final_action, "accept");
        // two turns: 100 + 100 = 200 tokens
        assert_eq!(out.session_tokens, 200);
    }

    #[tokio::test]
    async fn delegation_failure_escalates() {
        let cfg = CorrectionLoopConfig {
            max_corrections: 2,
            ..Default::default()
        };
        let out = run_correction_loop(
            &ErrDelegate,
            None,
            None,
            TaskInput {
                description: "fix the typescript lint errors".into(),
                ..Default::default()
            },
            &cfg,
        )
        .await
        .unwrap();
        assert_eq!(out.final_action, "escalate");
    }

    #[tokio::test]
    async fn exhausted_corrections_escalates() {
        let cfg = CorrectionLoopConfig {
            max_corrections: 1,
            ..Default::default()
        };
        // Always fails verifier; never passes.
        let d = AlwaysFailThenPass {
            count: Mutex::new(0),
            fail_count: 99,
        };
        let out = run_correction_loop(
            &d,
            None,
            None,
            TaskInput {
                description: "fix the typescript lint errors".into(),
                ..Default::default()
            },
            &cfg,
        )
        .await
        .unwrap();
        assert_eq!(out.final_action, "escalate");
    }

    #[tokio::test]
    async fn external_task_pass_short_circuits() {
        // Caller sets task_pass=true → loop should accept regardless of verifier.
        let cfg = CorrectionLoopConfig {
            max_corrections: 2,
            ..Default::default()
        };
        let d = AlwaysFailThenPass {
            count: Mutex::new(0),
            fail_count: 99,
        };
        let mut task = TaskInput {
            description: "do thing".into(),
            ..Default::default()
        };
        task.task_pass = Some(true);
        let out = run_correction_loop(&d, None, None, task, &cfg)
            .await
            .unwrap();
        assert_eq!(out.final_action, "accept");
    }
}
