//! Shared type surface for the memory store. Port of
//! `memory-palace/src/types.ts`.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// A single memory entry. Properties is an open-ended JSON object so the
/// store can carry arbitrary routing/observation metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryObject {
    pub id: String,
    pub kind: String,
    pub task_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    pub timestamp: String,
    pub description: String,
    pub properties: Value,
    pub tags: Vec<String>,
}

/// Query filter for `MemoryAdapter::query`.
#[derive(Debug, Clone, Default)]
pub struct MemoryQuery {
    pub kind: Option<String>,
    pub tags: Option<Vec<String>>,
    pub limit: Option<usize>,
    pub since: Option<String>,
}

impl MemoryQuery {
    pub fn new() -> Self {
        Self::default()
    }
}

#[derive(Debug, Clone, Default)]
pub struct MemoryStats {
    pub total_objects: usize,
    pub last_write: String,
}

/// Emitted-file record written by the agent and replayed into memory.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmittedFileRecord {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub path: String,
    pub sha256: String,
    pub bytes: i64,
    pub before_hash: Option<String>,
    pub existed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<String>,
}

/// Run record produced by the orchestrator. Persisted via either the
/// specialized `runs` table (SqliteAdapter) or as a generic MemoryObject.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunRecord {
    pub run_id: String,
    pub task_id: String,
    pub description: String,
    pub language: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_family: Option<String>,
    pub mode: String,
    pub model: String,
    #[serde(default)]
    pub provider_key: String,
    #[serde(default)]
    pub provider_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    pub outcome: String,
    pub outcome_class: String,
    #[serde(default = "default_routing_lesson")]
    pub routing_lesson: String,
    pub final_verdict: String,
    pub source_of_truth: String,
    pub final_action: String,
    #[serde(default)]
    pub tokens: i64,
    #[serde(default)]
    pub duration_ms: i64,
    #[serde(default)]
    pub turns: i64,
    #[serde(default)]
    pub validator_duration_ms: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verifier_overall: Option<String>,
    #[serde(default)]
    pub files_emitted: i64,
    #[serde(default)]
    pub total_bytes_emitted: i64,
    #[serde(default)]
    pub emissions: Vec<EmittedFileRecord>,
    #[serde(default)]
    pub emission_ids: Vec<String>,
    pub timestamp: String,
}

fn default_routing_lesson() -> String {
    "neutral".to_string()
}

/// Task observation input for `MemoryStore::write_task_observation`.
#[derive(Debug, Clone, Default)]
pub struct TaskObservationInput {
    pub task_id: String,
    pub description: String,
    pub task_family: Option<String>,
    pub language: String,
    pub runtime: Option<String>,
    pub mode: String,
    pub model: String,
    pub provider_key: Option<String>,
    pub provider_type: Option<String>,
    pub base_url: Option<String>,
    pub prompt_shape: Option<String>,
    pub verifier_overall: Option<String>,
    pub final_action: Option<String>,
    pub task_pass: Option<bool>,
    pub outcome: Option<String>,
    pub outcome_class: Option<String>,
    pub routing_lesson: Option<String>,
    pub reason: Option<String>,
    pub tokens: i64,
    pub duration_ms: i64,
    pub turns: Option<i64>,
    pub final_verdict: Option<String>,
    pub source_of_truth: Option<String>,
    pub task_validation: Option<Value>,
    pub emissions: Option<Vec<EmittedFileRecord>>,
    pub emission_ids: Option<Vec<String>>,
    pub validator_duration_ms: Option<i64>,
}

/// Normalized run row for SQLite specialized storage (camelCase → snake_case
/// column mapping happens in the adapter).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunRow {
    pub run_id: String,
    pub task_id: String,
    pub description: String,
    pub language: String,
    pub task_family: Option<String>,
    pub mode: String,
    pub model: String,
    pub provider_key: String,
    pub provider_type: String,
    pub base_url: Option<String>,
    pub outcome: String,
    pub outcome_class: String,
    pub routing_lesson: String,
    pub final_verdict: String,
    pub source_of_truth: String,
    pub final_action: String,
    pub tokens: i64,
    pub duration_ms: i64,
    pub turns: i64,
    pub validator_duration_ms: i64,
    pub verifier_overall: Option<String>,
    pub files_emitted: i64,
    pub total_bytes_emitted: i64,
    pub emission_ids: Vec<String>,
    pub timestamp: String,
}

impl From<&RunRecord> for RunRow {
    fn from(r: &RunRecord) -> Self {
        RunRow {
            run_id: r.run_id.clone(),
            task_id: r.task_id.clone(),
            description: r.description.clone(),
            language: r.language.clone(),
            task_family: r.task_family.clone(),
            mode: r.mode.clone(),
            model: r.model.clone(),
            provider_key: r.provider_key.clone(),
            provider_type: r.provider_type.clone(),
            base_url: r.base_url.clone(),
            outcome: r.outcome.clone(),
            outcome_class: r.outcome_class.clone(),
            routing_lesson: r.routing_lesson.clone(),
            final_verdict: r.final_verdict.clone(),
            source_of_truth: r.source_of_truth.clone(),
            final_action: r.final_action.clone(),
            tokens: r.tokens,
            duration_ms: r.duration_ms,
            turns: r.turns,
            validator_duration_ms: r.validator_duration_ms,
            verifier_overall: r.verifier_overall.clone(),
            files_emitted: r.files_emitted,
            total_bytes_emitted: r.total_bytes_emitted,
            emission_ids: r.emission_ids.clone(),
            timestamp: r.timestamp.clone(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmissionRow {
    pub id: String,
    pub run_id: String,
    pub task_id: String,
    pub turn: i64,
    pub path: String,
    pub sha256: String,
    pub bytes: i64,
    pub before_hash: Option<String>,
    pub existed: bool,
    pub timestamp: String,
}

/// Metadata returned by `SqliteAdapter::backup`.
#[derive(Debug, Clone)]
pub struct BackupMetadata {
    pub file_path: String,
    pub size_bytes: u64,
    pub sha256: String,
    pub schema_version: Option<i64>,
    pub timestamp: String,
    pub row_count: BackupRowCount,
}

#[derive(Debug, Clone, Default)]
pub struct BackupRowCount {
    pub observations: i64,
    pub runs: i64,
    pub emissions: i64,
}
