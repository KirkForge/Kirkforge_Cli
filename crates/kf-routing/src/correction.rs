//! R3 — port of `orchestrator/src/correction-loop.ts` + `truth-model.ts`.
//!
//! Pure decision logic for the orchestrator's correction loop:
//! - `decide_correction`: what action (accept / correct / escalate) to take.
//! - `compute_final_verdict`: single-precedence truth table for the final
//!   verdict of a task run.
//!
//! The impure parts (running the correction, model calls) are WO 29.7.

use serde::{Deserialize, Serialize};

// ── Reduced state packet ────────────────────────────────────────────────────

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Changes {
    #[serde(default)]
    pub files_changed: i64,
    #[serde(default)]
    pub paths: Vec<String>,
    #[serde(default)]
    pub insertions: i64,
    #[serde(default)]
    pub deletions: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GraphState {
    #[serde(default)]
    pub edge_count: i64,
    #[serde(default)]
    pub new_edges: i64,
    #[serde(default)]
    pub broken_edges: i64,
    #[serde(default)]
    pub cycles: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LintState {
    #[serde(default)]
    pub errors: i64,
    #[serde(default)]
    pub warnings: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TypesState {
    #[serde(default)]
    pub errors: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SecurityState {
    #[serde(default)]
    pub findings: i64,
    #[serde(default)]
    pub critical: i64,
    #[serde(default)]
    pub high: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[derive(Default)]
pub enum OverallVerdict {
    Pass,
    Fail,
    Warn,
    #[default]
    Unknown,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Verification {
    #[serde(default)]
    pub lint: LintState,
    #[serde(default)]
    pub types: TypesState,
    #[serde(default)]
    pub security: SecurityState,
    #[serde(default)]
    pub overall: OverallVerdict,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct VerifierPolicy {
    #[serde(default)]
    pub required: Vec<String>,
    #[serde(default)]
    pub advisory: Vec<String>,
    #[serde(default)]
    pub missing_required: Vec<String>,
    #[serde(default)]
    pub skipped_required: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ReducedStatePacket {
    #[serde(default)]
    pub task_id: String,
    #[serde(default)]
    pub turn: i64,
    #[serde(default)]
    pub ts: String,
    #[serde(default)]
    pub changes: Changes,
    #[serde(default)]
    pub graph: GraphState,
    #[serde(default)]
    pub verification: Verification,
    #[serde(default)]
    pub verifier_policy: Option<VerifierPolicy>,
}

// ── decide_correction ───────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub enum CorrectionAction {
    Accept,
    Escalate,
    Correct,
}

#[derive(Debug, Clone)]
pub struct CorrectionDecision {
    pub action: CorrectionAction,
    pub rationale: String,
    pub correction_prompt: Option<String>,
    pub correction_count: i64,
    pub worker_tokens: i64,
    pub session_tokens: i64,
}

#[derive(Clone, Copy, PartialEq)]
enum SlotPolicy {
    Required,
    Advisory,
    Absent,
}

fn slot_policy(policy: Option<&VerifierPolicy>, slot: &str) -> SlotPolicy {
    match policy {
        None => SlotPolicy::Required,
        Some(p) if p.required.iter().any(|s| s == slot) => SlotPolicy::Required,
        Some(p) if p.advisory.iter().any(|s| s == slot) => SlotPolicy::Advisory,
        _ => SlotPolicy::Absent,
    }
}

// ponytail: placeholder prompt, not the real `buildCorrectionPrompt` template.
// The real template lives in `correction-core` (TS, not ported — porting it
// requires the orchestrator's model-call layer, which is WO 29.7 and hasn't
// shipped). The pure decision function (`decide_correction`) only needs to
// signal that a correction is required; tests assert `correction_prompt` is
// truthy, which this non-empty string satisfies.
// DEFERRED to WO 32.11: port the `correction-core` template into
// `kf-orchestrator` and wire it into the correction loop's prompt emission.
// ceiling: placeholder string carries no per-failure guidance; the model gets
// a generic prompt instead of one tailored to the specific verifier failure.
fn correction_prompt(packet: &ReducedStatePacket) -> String {
    format!(
        "targeted correction: overall={:?}, broken_edges={}, lint_errors={}",
        packet.verification.overall, packet.graph.broken_edges, packet.verification.lint.errors
    )
}

/// Pure decision: given a packet and run state, what action should the
/// orchestrator take?
#[allow(clippy::too_many_arguments)]
pub fn decide_correction(
    packet: &ReducedStatePacket,
    correction_count: i64,
    max_corrections: i64,
    worker_tokens: i64,
    session_tokens: i64,
    session_cost: f64,
    max_cost: Option<f64>,
    task_pass: Option<bool>,
) -> CorrectionDecision {
    let echo = |action, rationale, correction_prompt: Option<String>| CorrectionDecision {
        action,
        rationale,
        correction_prompt,
        correction_count,
        worker_tokens,
        session_tokens,
    };

    match task_pass {
        Some(true) => {
            return echo(
                CorrectionAction::Accept,
                "taskPass: true (external validator passed)".to_string(),
                None,
            );
        }
        Some(false) => {
            if correction_count >= max_corrections {
                return echo(
                    CorrectionAction::Escalate,
                    "taskPass: false (external validator failed); exceeded corrections".to_string(),
                    None,
                );
            }
            if let Some(max) = max_cost {
                if session_cost >= max {
                    return echo(
                        CorrectionAction::Escalate,
                        format!(
                            "taskPass: false (external validator failed); session cost ${session_cost:.4} exceeds budget ${max:.4}"
                        ),
                        None,
                    );
                }
            }
            return echo(
                CorrectionAction::Correct,
                "taskPass: false (external validator failed); targeted correction".to_string(),
                Some(correction_prompt(packet)),
            );
        }
        None => {}
    }

    // Security: escalate only when a critical finding exists AND the security
    // slot is Required (which is also the default when no policy is set).
    if packet.verification.security.critical > 0
        && slot_policy(packet.verifier_policy.as_ref(), "security") == SlotPolicy::Required
    {
        return echo(
            CorrectionAction::Escalate,
            "critical security finding".to_string(),
            None,
        );
    }

    if packet.graph.broken_edges > 0
        && slot_policy(packet.verifier_policy.as_ref(), "graph") == SlotPolicy::Required
    {
        return echo(
            CorrectionAction::Escalate,
            format!("{} broken import edges", packet.graph.broken_edges),
            None,
        );
    }

    if correction_count >= max_corrections {
        return echo(
            CorrectionAction::Escalate,
            format!("exceeded {max_corrections} corrections"),
            None,
        );
    }

    if let Some(max) = max_cost {
        if session_cost >= max {
            return echo(
                CorrectionAction::Escalate,
                format!("session cost ${session_cost:.4} exceeds budget ${max:.4}"),
                None,
            );
        }
    }

    if packet.verification.overall == OverallVerdict::Pass {
        return echo(
            CorrectionAction::Accept,
            "verification passed".to_string(),
            None,
        );
    }

    echo(
        CorrectionAction::Correct,
        format!(
            "verification {:?}; targeted correction",
            packet.verification.overall
        ),
        Some(correction_prompt(packet)),
    )
}

// ── Truth model (truth-model.ts) ────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FinalVerdict {
    Pass,
    Fail,
    Error,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum SourceOfTruth {
    #[serde(rename = "task-validator")]
    TaskValidator,
    Verifier,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[derive(Default)]
pub enum ValidationStatus {
    Pass,
    Fail,
    Error,
    Skipped,
    #[default]
    Other,
}

#[derive(Debug, Clone, Default)]
pub struct TaskValidationResult {
    pub status: ValidationStatus,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum FinalAction {
    Accept,
    Escalate,
}

#[derive(Debug, Clone)]
pub struct TruthProfile {
    pub language: String,
    pub validator_required: bool,
}

#[derive(Debug, Clone)]
pub struct TruthInput<'a> {
    pub task_validation: TaskValidationResult,
    pub has_validator: bool,
    pub final_action: FinalAction,
    pub packet: Option<&'a ReducedStatePacket>,
    pub profile: TruthProfile,
    pub actual_mode: String,
    pub protocol_broken: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TruthOutput {
    pub final_verdict: FinalVerdict,
    pub source_of_truth: SourceOfTruth,
    pub reason: String,
}

fn final_verdict_from_validation(v: &TaskValidationResult) -> FinalVerdict {
    match v.status {
        ValidationStatus::Pass => FinalVerdict::Pass,
        ValidationStatus::Fail => FinalVerdict::Fail,
        ValidationStatus::Error => FinalVerdict::Unknown,
        _ => FinalVerdict::Unknown,
    }
}

fn final_verdict_from_verifier(
    final_action: FinalAction,
    packet: Option<&ReducedStatePacket>,
) -> FinalVerdict {
    match (final_action, packet) {
        (FinalAction::Accept, Some(p)) if p.verification.overall == OverallVerdict::Pass => {
            FinalVerdict::Pass
        }
        (FinalAction::Escalate, opt) => match opt {
            Some(p) if p.verification.overall == OverallVerdict::Fail => FinalVerdict::Fail,
            _ => FinalVerdict::Unknown,
        },
        _ => FinalVerdict::Fail,
    }
}

/// Single precedence table for final-verdict computation. Every code path
/// that decides what happened must go through this function.
pub fn compute_final_verdict(input: &TruthInput<'_>) -> TruthOutput {
    let effective_source = if input.has_validator {
        SourceOfTruth::TaskValidator
    } else {
        SourceOfTruth::Verifier
    };

    // Precedence 1: protocol integrity break.
    if input.protocol_broken {
        return TruthOutput {
            final_verdict: FinalVerdict::Fail,
            source_of_truth: effective_source,
            reason: "protocol integrity broken (unterminated markers or truncated model output) — all artifact writes blocked".to_string(),
        };
    }

    // Precedence 2: validator result.
    if input.has_validator {
        return TruthOutput {
            final_verdict: final_verdict_from_validation(&input.task_validation),
            source_of_truth: SourceOfTruth::TaskValidator,
            reason: format!(
                "validator result: {}",
                status_str(&input.task_validation.status)
            ),
        };
    }

    // Precedence 3: validator recommended but missing.
    if input.profile.validator_required {
        return TruthOutput {
            final_verdict: FinalVerdict::Unknown,
            source_of_truth: SourceOfTruth::Verifier,
            reason: format!(
                "validator required for {} profile but not configured — verifier pass is advisory only",
                input.profile.language
            ),
        };
    }

    // Precedence 4: schema-contract mode clarification.
    if input.actual_mode == "schema-contract" {
        if let Some(p) = input.packet {
            if p.verification.overall == OverallVerdict::Pass {
                return TruthOutput {
                    final_verdict: FinalVerdict::Unknown,
                    source_of_truth: SourceOfTruth::Verifier,
                    reason: "schema-contract validates structured output but does not persist files — pass cannot confirm code emission".to_string(),
                };
            }
        }
        let overall = input
            .packet
            .map(|p| format!("{:?}", p.verification.overall).to_lowercase())
            .unwrap_or_else(|| "no packet".to_string());
        return TruthOutput {
            final_verdict: final_verdict_from_verifier(input.final_action, input.packet),
            source_of_truth: SourceOfTruth::Verifier,
            reason: format!("schema-contract verifier outcome: {overall}"),
        };
    }

    // Precedence 5: verifier result.
    let verdict = final_verdict_from_verifier(input.final_action, input.packet);
    let overall = input
        .packet
        .map(|p| format!("{:?}", p.verification.overall).to_lowercase())
        .unwrap_or_else(|| "none".to_string());
    TruthOutput {
        final_verdict: verdict,
        source_of_truth: SourceOfTruth::Verifier,
        reason: format!(
            "verifier result: overall={}, action={}",
            overall,
            match input.final_action {
                FinalAction::Accept => "accept",
                FinalAction::Escalate => "escalate",
            }
        ),
    }
}

fn status_str(s: &ValidationStatus) -> &'static str {
    match s {
        ValidationStatus::Pass => "pass",
        ValidationStatus::Fail => "fail",
        ValidationStatus::Error => "error",
        ValidationStatus::Skipped => "skipped",
        ValidationStatus::Other => "other",
    }
}

/// Maps validation status to a memory-friendly outcome string.
pub fn validation_outcome_for_memory(v: &TaskValidationResult) -> &'static str {
    match v.status {
        ValidationStatus::Pass => "pass",
        ValidationStatus::Fail => "fail",
        _ => "error",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pass_packet() -> ReducedStatePacket {
        ReducedStatePacket {
            task_id: "t1".into(),
            verification: Verification {
                overall: OverallVerdict::Pass,
                ..Default::default()
            },
            ..Default::default()
        }
    }

    fn decide(
        packet: &ReducedStatePacket,
        count: i64,
        max: i64,
        cost: f64,
        max_cost: Option<f64>,
        task_pass: Option<bool>,
    ) -> CorrectionAction {
        decide_correction(packet, count, max, 100, 100, cost, max_cost, task_pass).action
    }

    #[test]
    fn accepts_clean_packet() {
        let p = pass_packet();
        assert_eq!(decide(&p, 0, 3, 0.0, None, None), CorrectionAction::Accept);
    }

    #[test]
    fn escalates_critical_security_no_policy_backward_compat() {
        let mut p = pass_packet();
        p.verification.security.critical = 1;
        assert_eq!(
            decide(&p, 0, 3, 0.0, None, None),
            CorrectionAction::Escalate
        );
    }

    #[test]
    fn escalates_critical_security_when_required() {
        let mut p = pass_packet();
        p.verification.security.critical = 1;
        p.verifier_policy = Some(VerifierPolicy {
            required: vec!["lint".into(), "types".into(), "security".into()],
            ..Default::default()
        });
        assert_eq!(
            decide(&p, 0, 3, 0.0, None, None),
            CorrectionAction::Escalate
        );
    }

    #[test]
    fn corrects_critical_security_when_advisory() {
        let mut p = pass_packet();
        p.verification.security.critical = 1;
        p.verification.overall = OverallVerdict::Fail;
        p.verifier_policy = Some(VerifierPolicy {
            required: vec!["lint".into(), "types".into()],
            advisory: vec!["security".into()],
            ..Default::default()
        });
        let d = decide_correction(&p, 0, 3, 100, 100, 0.0, None, None);
        assert_eq!(d.action, CorrectionAction::Correct);
        assert!(d.correction_prompt.is_some());
    }

    #[test]
    fn corrects_critical_security_when_absent_from_policy() {
        let mut p = pass_packet();
        p.verification.security.critical = 1;
        p.verification.overall = OverallVerdict::Fail;
        p.verifier_policy = Some(VerifierPolicy {
            required: vec!["lint".into(), "types".into()],
            advisory: vec!["graph".into()],
            ..Default::default()
        });
        let d = decide_correction(&p, 0, 3, 100, 100, 0.0, None, None);
        assert_eq!(d.action, CorrectionAction::Correct);
    }

    #[test]
    fn escalates_broken_edges_no_policy() {
        let mut p = pass_packet();
        p.graph.broken_edges = 1;
        assert_eq!(
            decide(&p, 0, 3, 0.0, None, None),
            CorrectionAction::Escalate
        );
    }

    #[test]
    fn escalates_broken_edges_when_graph_required() {
        let mut p = pass_packet();
        p.graph.broken_edges = 2;
        p.verifier_policy = Some(VerifierPolicy {
            required: vec![
                "lint".into(),
                "types".into(),
                "security".into(),
                "graph".into(),
            ],
            ..Default::default()
        });
        assert_eq!(
            decide(&p, 0, 3, 0.0, None, None),
            CorrectionAction::Escalate
        );
    }

    #[test]
    fn corrects_broken_edges_when_graph_advisory() {
        let mut p = pass_packet();
        p.graph.broken_edges = 3;
        p.verification.overall = OverallVerdict::Warn;
        p.verifier_policy = Some(VerifierPolicy {
            required: vec!["lint".into(), "types".into(), "security".into()],
            advisory: vec!["graph".into()],
            ..Default::default()
        });
        let d = decide_correction(&p, 0, 3, 100, 100, 0.0, None, None);
        assert_eq!(d.action, CorrectionAction::Correct);
        assert!(d.correction_prompt.is_some());
    }

    #[test]
    fn does_not_escalate_broken_edges_when_graph_absent() {
        let mut p = pass_packet();
        p.graph.broken_edges = 4;
        p.verification.overall = OverallVerdict::Warn;
        p.verifier_policy = Some(VerifierPolicy {
            required: vec!["lint".into(), "types".into()],
            ..Default::default()
        });
        let d = decide_correction(&p, 0, 3, 100, 100, 0.0, None, None);
        assert_eq!(d.action, CorrectionAction::Correct);
    }

    #[test]
    fn corrects_lint_errors() {
        let mut p = pass_packet();
        p.verification.lint.errors = 2;
        p.verification.overall = OverallVerdict::Fail;
        let d = decide_correction(&p, 0, 3, 100, 100, 0.0, None, None);
        assert_eq!(d.action, CorrectionAction::Correct);
        assert!(d.correction_prompt.is_some());
    }

    #[test]
    fn escalates_when_max_corrections_hit() {
        let mut p = pass_packet();
        p.verification.lint.errors = 1;
        p.verification.overall = OverallVerdict::Fail;
        assert_eq!(
            decide(&p, 3, 3, 0.0, None, None),
            CorrectionAction::Escalate
        );
    }

    #[test]
    fn task_pass_true_accepts() {
        let p = pass_packet();
        assert_eq!(
            decide(&p, 0, 3, 0.0, None, Some(true)),
            CorrectionAction::Accept
        );
    }

    #[test]
    fn task_pass_false_corrects_when_verifier_passes() {
        let p = pass_packet();
        let r = decide_correction(&p, 0, 3, 100, 100, 0.0, None, Some(false));
        assert_eq!(r.action, CorrectionAction::Correct);
        assert!(r.rationale.contains("external validator failed"));
    }

    #[test]
    fn task_pass_false_escalates_when_exhausted() {
        let p = pass_packet();
        let r = decide_correction(&p, 3, 3, 100, 100, 0.0, None, Some(false));
        assert_eq!(r.action, CorrectionAction::Escalate);
        assert!(r.rationale.contains("external validator failed"));
    }

    #[test]
    fn task_pass_false_escalates_when_cost_exceeded() {
        let p = pass_packet();
        let r = decide_correction(&p, 0, 3, 100, 100, 10.0, Some(5.0), Some(false));
        assert_eq!(r.action, CorrectionAction::Escalate);
        assert!(r.rationale.contains("external validator failed"));
    }

    #[test]
    fn task_pass_none_falls_through_to_verifier_logic() {
        let p = pass_packet();
        assert_eq!(decide(&p, 0, 3, 0.0, None, None), CorrectionAction::Accept);
    }

    // ── truth model ──

    #[test]
    fn protocol_break_is_fail() {
        let inp = TruthInput {
            task_validation: TaskValidationResult::default(),
            has_validator: false,
            final_action: FinalAction::Accept,
            packet: None,
            profile: TruthProfile {
                language: "text".into(),
                validator_required: false,
            },
            actual_mode: "artifact".into(),
            protocol_broken: true,
        };
        let out = compute_final_verdict(&inp);
        assert_eq!(out.final_verdict, FinalVerdict::Fail);
    }

    #[test]
    fn validator_pass_overrides_everything() {
        let inp = TruthInput {
            task_validation: TaskValidationResult {
                status: ValidationStatus::Pass,
            },
            has_validator: true,
            final_action: FinalAction::Escalate,
            packet: None,
            profile: TruthProfile {
                language: "python".into(),
                validator_required: true,
            },
            actual_mode: "artifact".into(),
            protocol_broken: false,
        };
        let out = compute_final_verdict(&inp);
        assert_eq!(out.final_verdict, FinalVerdict::Pass);
        assert_eq!(out.source_of_truth, SourceOfTruth::TaskValidator);
    }

    #[test]
    fn validator_recommended_but_missing_is_unknown() {
        let inp = TruthInput {
            task_validation: TaskValidationResult::default(),
            has_validator: false,
            final_action: FinalAction::Accept,
            packet: None,
            profile: TruthProfile {
                language: "rust".into(),
                validator_required: true,
            },
            actual_mode: "artifact".into(),
            protocol_broken: false,
        };
        let out = compute_final_verdict(&inp);
        assert_eq!(out.final_verdict, FinalVerdict::Unknown);
        assert_eq!(out.source_of_truth, SourceOfTruth::Verifier);
    }

    #[test]
    fn schema_contract_pass_is_unknown_no_files_written() {
        let p = ReducedStatePacket {
            verification: Verification {
                overall: OverallVerdict::Pass,
                ..Default::default()
            },
            ..Default::default()
        };
        let inp = TruthInput {
            task_validation: TaskValidationResult::default(),
            has_validator: false,
            final_action: FinalAction::Accept,
            packet: Some(&p),
            profile: TruthProfile {
                language: "text".into(),
                validator_required: false,
            },
            actual_mode: "schema-contract".into(),
            protocol_broken: false,
        };
        let out = compute_final_verdict(&inp);
        assert_eq!(out.final_verdict, FinalVerdict::Unknown);
    }

    #[test]
    fn verifier_pass_accept_is_pass() {
        let p = ReducedStatePacket {
            verification: Verification {
                overall: OverallVerdict::Pass,
                ..Default::default()
            },
            ..Default::default()
        };
        let inp = TruthInput {
            task_validation: TaskValidationResult::default(),
            has_validator: false,
            final_action: FinalAction::Accept,
            packet: Some(&p),
            profile: TruthProfile {
                language: "text".into(),
                validator_required: false,
            },
            actual_mode: "artifact".into(),
            protocol_broken: false,
        };
        let out = compute_final_verdict(&inp);
        assert_eq!(out.final_verdict, FinalVerdict::Pass);
        assert_eq!(out.source_of_truth, SourceOfTruth::Verifier);
    }

    #[test]
    fn escalate_with_no_fail_is_unknown() {
        let p = ReducedStatePacket {
            verification: Verification {
                overall: OverallVerdict::Warn,
                ..Default::default()
            },
            ..Default::default()
        };
        let inp = TruthInput {
            task_validation: TaskValidationResult::default(),
            has_validator: false,
            final_action: FinalAction::Escalate,
            packet: Some(&p),
            profile: TruthProfile {
                language: "text".into(),
                validator_required: false,
            },
            actual_mode: "artifact".into(),
            protocol_broken: false,
        };
        let out = compute_final_verdict(&inp);
        assert_eq!(out.final_verdict, FinalVerdict::Unknown);
    }

    #[test]
    fn validation_outcome_for_memory_maps_status() {
        assert_eq!(
            validation_outcome_for_memory(&TaskValidationResult {
                status: ValidationStatus::Pass
            }),
            "pass"
        );
        assert_eq!(
            validation_outcome_for_memory(&TaskValidationResult {
                status: ValidationStatus::Skipped
            }),
            "error"
        );
    }
}
