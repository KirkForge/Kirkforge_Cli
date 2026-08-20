//! Shared type surface for the orchestrator. Port of
//! `orchestrator/src/types.ts` (the parts the delegation/decompose/correction
//! pipeline needs). Verifier-shape types live in `kf_routing::correction`;
//! the fold that populates them is [`crate::reducer`] (ADR-076).

use serde::{Deserialize, Serialize};
use serde_json::Value;

use kf_routing::classifier::DelegationMode;
use kf_routing::correction::ReducedStatePacket;

/// Input to `Orchestrator::delegate`. Mirrors TS `TaskInput` (selected fields).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TaskInput {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    pub description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub files: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode_override: Option<DelegationMode>,
    /// External validator outcome. `Some(true)` short-circuits to accept;
    /// `Some(false)` triggers targeted correction; `None` defers to verifier.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_pass: Option<bool>,
    #[serde(default)]
    pub suppress_memory: bool,
    /// Language override (skips `detect_task_profile` regex matching).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    /// Authenticated actor id (audit trail only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actor: Option<String>,
}

/// Mode-shape emission produced by a mode executor. Subset of TS `Emission`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Emission {
    pub agent_id: String,
    pub content: String,
    #[serde(default)]
    pub prompt_tokens: i64,
    #[serde(default)]
    pub completion_tokens: i64,
    #[serde(default)]
    pub total_tokens: i64,
    #[serde(default)]
    pub reasoning_tokens: Option<i64>,
    pub model: String,
    /// Echoes the delegation mode that produced this emission.
    pub format: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schema_contract: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finish_reason: Option<String>,
    #[serde(default)]
    pub retried: bool,
}

impl Emission {
    /// True when the model hit a length limit. Drives truncation warnings.
    pub fn was_truncated(&self) -> bool {
        matches!(
            self.finish_reason.as_deref(),
            Some("length") | Some("max_tokens")
        )
    }
}

/// A discrete event attached to a delegation result. The orchestrator
/// re-emits selected signals (artifact.emitted, artifact.blocked, …) through
/// the [`crate::sink::EventSink`].
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Signal {
    pub id: String,
    pub task_id: String,
    pub domain: String,
    pub kind: String,
    pub source: String,
    pub ts: String,
    pub value: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confidence: Option<f64>,
}

/// Per-decision verdict attached to each result.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DelegationDecisionInfo {
    pub mode: String,
    pub reason: String,
    #[serde(default)]
    pub auto_routed: bool,
}

/// Outcome of a single delegation. Returned by mode executors and by
/// `Orchestrator::delegate`. Mirrors TS `DelegationResult`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DelegationResult {
    pub decision: DelegationDecisionInfo,
    pub emission: Emission,
    pub signals: Vec<Signal>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub packet: Option<ReducedStatePacket>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_resolved: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skills_loaded: Option<Vec<String>>,
}

/// File info carried by `artifact.emitted` / `files.written` signals.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EmittedFileInfo {
    pub path: String,
    pub sha256: String,
    pub bytes: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before_hash: Option<String>,
    #[serde(default)]
    pub existed: bool,
}

/// Pull the written-file list out of a delegation result by scanning its
/// signals (TS `extractWrittenFiles`).
pub fn extract_written_files(result: &DelegationResult) -> Vec<String> {
    for sig in &result.signals {
        if sig.kind == "files.written" || sig.kind == "artifact.emitted" {
            if let Some(files) = sig.value.get("files").and_then(|v| v.as_array()) {
                return files
                    .iter()
                    .filter_map(|f| {
                        if let Some(s) = f.as_str() {
                            Some(s.to_string())
                        } else {
                            f.get("path").and_then(|p| p.as_str()).map(String::from)
                        }
                    })
                    .filter(|s| !s.is_empty())
                    .collect();
            }
        }
    }
    Vec::new()
}

/// Pull full file metadata (sha256/bytes/…) out of a delegation result.
pub fn extract_emission_files(result: &DelegationResult) -> Vec<EmittedFileInfo> {
    for sig in &result.signals {
        if sig.kind == "artifact.emitted" || sig.kind == "files.written" {
            if let Some(files) = sig.value.get("files").and_then(|v| v.as_array()) {
                if !files.is_empty() {
                    return files
                        .iter()
                        .filter_map(|f| serde_json::from_value(f.clone()).ok())
                        .collect();
                }
            }
        }
    }
    Vec::new()
}

/// `Orchestrator::get_stats()` payload.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct OrchestratorStats {
    pub total_delegations: i64,
    pub total_tokens: i64,
    #[serde(default)]
    pub total_errors: i64,
}

/// `TaskNode` — subtask in a decomposition. Matches the TS canonical shape.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TaskNode {
    pub id: String,
    pub description: String,
    #[serde(default = "default_language")]
    pub language: String,
    #[serde(default)]
    pub depends_on: Vec<String>,
    #[serde(default = "default_complexity")]
    pub estimated_complexity: String,
    #[serde(default)]
    pub output_files: Vec<String>,
    #[serde(default)]
    pub verification_hint: String,
}

fn default_language() -> String {
    "text".to_string()
}
fn default_complexity() -> String {
    "moderate".to_string()
}

/// Output of a successful decomposition (TS `DecompositionResult`).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DecompositionResult {
    pub root_task: String,
    pub tasks: Vec<TaskNode>,
    pub total_estimated_tokens: i64,
    pub rationale: String,
}

/// Per-subtask outcome from `execute_decomposition`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SubtaskExecutionResult {
    pub node_id: String,
    pub ok: bool,
    pub description: String,
    pub language: String,
    pub duration_ms: i64,
    pub tokens_used: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verdict: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub files: Option<Vec<String>>,
}

/// Aggregate execution outcome (TS `DecompositionExecutionResult`).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DecompositionExecutionResult {
    pub root_task: String,
    pub results: Vec<SubtaskExecutionResult>,
    pub total_subtasks: i64,
    pub succeeded_count: i64,
    pub failed_count: i64,
    pub total_tokens: i64,
    pub total_duration_ms: i64,
}

/// External task-validator outcome (TS `TaskValidationResult`).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TaskValidationResult {
    /// "pass" | "fail" | "error" | "skipped"
    pub status: String,
    #[serde(default)]
    pub validator: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default)]
    pub duration_ms: i64,
}

impl TaskValidationResult {
    pub fn skipped(validator: impl Into<String>, reason: impl Into<String>) -> Self {
        Self {
            status: "skipped".into(),
            validator: validator.into(),
            reason: Some(reason.into()),
            duration_ms: 0,
        }
    }
}
/// Configuration for `run_correction_loop`.
#[derive(Debug, Clone, Default)]
pub struct CorrectionLoopConfig {
    pub max_corrections: i64,
    pub max_cost: Option<f64>,
    /// Optional external validator command (shell form).
    pub validator: Option<ValidatorConfig>,
}

#[derive(Debug, Clone)]
pub enum ValidatorConfig {
    Shell {
        command: String,
        timeout_ms: u64,
    },
    Structured {
        command: String,
        args: Vec<String>,
        cwd: Option<String>,
        timeout_ms: u64,
    },
}

/// Outcome of `run_correction_loop`.
#[derive(Debug, Clone, Default)]
pub struct CorrectionLoopOutcome {
    /// "accept" or "escalate".
    pub final_action: String,
    pub final_verdict: String,
    pub source_of_truth: String,
    pub task_validation: TaskValidationResult,
    pub task_outcome: String,
    pub session_tokens: i64,
    pub session_cost: f64,
    pub validator_duration_ms: i64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn emission_truncated_detection() {
        let truncate = |reason: Option<&str>| Emission {
            finish_reason: reason.map(str::to_string),
            ..Default::default()
        };
        assert!(truncate(Some("length")).was_truncated());
        assert!(truncate(Some("max_tokens")).was_truncated());
        assert!(!truncate(Some("stop")).was_truncated());
        assert!(!truncate(None).was_truncated());
    }

    #[test]
    fn extract_written_files_handles_string_and_object_shapes() {
        let mut r = DelegationResult::default();
        r.signals.push(Signal {
            id: "s1".into(),
            task_id: "t1".into(),
            domain: "code".into(),
            kind: "files.written".into(),
            source: "agent".into(),
            ts: "now".into(),
            value: json!({
                "files": ["a.py", {"path": "b.rs"}, "", {"path": ""}]
            }),
            confidence: None,
        });
        let files = extract_written_files(&r);
        assert_eq!(files, vec!["a.py".to_string(), "b.rs".to_string()]);
    }

    #[test]
    fn extract_emission_files_parses_full_shape() {
        let mut r = DelegationResult::default();
        r.signals.push(Signal {
            id: "s1".into(),
            task_id: "t1".into(),
            domain: "code".into(),
            kind: "artifact.emitted".into(),
            source: "agent".into(),
            ts: "now".into(),
            value: json!({
                "files": [
                    {"path": "x.py", "sha256": "abc", "bytes": 10, "existed": false}
                ]
            }),
            confidence: None,
        });
        let files = extract_emission_files(&r);
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, "x.py");
        assert_eq!(files[0].sha256, "abc");
    }

    #[test]
    fn extract_returns_empty_when_no_relevant_signal() {
        let mut r = DelegationResult::default();
        r.signals.push(Signal {
            id: "s".into(),
            task_id: "t".into(),
            domain: "task".into(),
            kind: "emission".into(),
            source: "agent".into(),
            ts: "now".into(),
            value: json!({}),
            confidence: None,
        });
        assert!(extract_written_files(&r).is_empty());
        assert!(extract_emission_files(&r).is_empty());
    }
}
