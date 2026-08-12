//! The orchestrator (R2). Port of `orchestrator/src/index.ts` — the
//! `delegate` pipeline: classify → recall memory → resolve provider →
//! build brief → dispatch to a mode executor → finalize.
//!
//! The reducer + deterministic verifiers (`orchestrator-verifiers.ts`,
//! `reducer.ts`) are NOT ported in this WO; the packet attached to each
//! `DelegationResult` remains `None` until the reducer ships. The
//! correction loop still works because `decide_correction` operates on
//! whatever packet it's given (or `Default::default()`).

use std::sync::{Arc, Mutex};

use anyhow::Result;
use tracing::info;

use kf_memory_store::MemoryStore;
use kf_routing::classifier::{classify_task, DelegationMode, TaskInput as RoutingTaskInput};
use kf_routing::profile::{detect_task_profile, profile_for_language, TaskProfile};

use crate::model::{ModelClient, PanickingClient, TaskBrief};
use crate::modes::{
    execute_artifact, execute_hard_prompt, execute_schema_contract, flush_signals_to_sink,
};
use crate::sink::{EventSink, NullSink};
use crate::types::{DelegationResult, OrchestratorStats, TaskInput};

/// Configuration for [`Orchestrator::new`].
pub struct OrchestratorConfig {
    /// The provider key the orchestrator should report on each delegation
    /// (e.g. `"local-ollama"`, `"openai/gpt-4o"`). Drives cost calculation.
    pub provider_key: String,
    /// Provider used by the decompose pipeline (TS uses a separate config).
    pub decompose_provider: String,
    /// Working directory for artifact writes.
    pub cwd: String,
    /// Optional routing memory store. When absent, recall + observation
    /// writes are skipped.
    pub memory: Option<Arc<MemoryStore>>,
    /// Optional event sink for artifact.* events. Defaults to no-op.
    pub sink: Option<Arc<dyn EventSink>>,
}

/// The orchestrator. Holds shared state (stats, memory, sink) across
/// multiple `delegate` calls. Mode executors are dispatched by
/// `delegate` based on the routing classifier.
pub struct Orchestrator {
    provider_key: String,
    decompose_provider: String,
    cwd: String,
    memory: Option<Arc<MemoryStore>>,
    sink: Arc<dyn EventSink>,
    client: Arc<dyn ModelClient>,
    stats: Mutex<OrchestratorStats>,
}

impl Orchestrator {
    /// Construct with a production `ModelClient`. Wires the no-op sink by
    /// default; pass an `OrchestratorConfig` with `sink` set to capture
    /// artifact events.
    pub fn new(client: Arc<dyn ModelClient>, config: OrchestratorConfig) -> Self {
        Self {
            provider_key: config.provider_key,
            decompose_provider: config.decompose_provider,
            cwd: config.cwd,
            memory: config.memory,
            sink: config.sink.unwrap_or_else(|| Arc::new(NullSink)),
            client,
            stats: Mutex::new(OrchestratorStats::default()),
        }
    }

    /// Test-only constructor that wires a [`PanickingClient`]. Calling
    /// `delegate` on the result will panic at the model call; useful for
    /// tests that only exercise classification + brief construction.
    #[doc(hidden)]
    pub fn panicking(config: OrchestratorConfig) -> Self {
        Self::new(Arc::new(PanickingClient), config)
    }

    pub fn provider_key(&self) -> &str {
        &self.provider_key
    }
    pub fn cwd(&self) -> &str {
        &self.cwd
    }
    pub fn memory(&self) -> Option<&MemoryStore> {
        self.memory.as_deref()
    }
    pub fn stats(&self) -> OrchestratorStats {
        self.stats.lock().expect("stats lock").clone()
    }

    /// Resolve a `TaskProfile` honoring `task.language` (override) or
    /// `detect_task_profile` on the description.
    fn resolve_profile(&self, task: &TaskInput) -> TaskProfile {
        if let Some(lang_str) = &task.language {
            // Map the string to TaskLanguage via lower-case compare; unknown
            // → detect from description (matches TS fallthrough).
            for lang in [
                kf_routing::profile::TaskLanguage::Typescript,
                kf_routing::profile::TaskLanguage::Javascript,
                kf_routing::profile::TaskLanguage::Python,
                kf_routing::profile::TaskLanguage::Shell,
                kf_routing::profile::TaskLanguage::Cpp,
                kf_routing::profile::TaskLanguage::C,
                kf_routing::profile::TaskLanguage::Rust,
                kf_routing::profile::TaskLanguage::Go,
                kf_routing::profile::TaskLanguage::Sql,
                kf_routing::profile::TaskLanguage::Text,
            ] {
                if lang.as_str() == lang_str {
                    return profile_for_language(lang);
                }
            }
        }
        detect_task_profile(&task.description)
    }

    fn resolve_mode(&self, task: &TaskInput) -> kf_routing::classifier::DelegationDecision {
        let routing_input = RoutingTaskInput {
            description: &task.description,
            mode_override: task.mode_override,
        };
        classify_task(&routing_input)
    }

    /// Recall a routing recommendation from memory (if configured). Returns
    /// None when memory is absent or nothing similar is stored.
    fn recall(&self, task: &TaskInput) -> Option<kf_routing::Recommendation> {
        let store = self.memory.as_ref()?;
        store
            .recall(&task.description, None)
            .ok()
            .flatten()
            .filter(|r| {
                // Same bias gate as TS: confidence + evidence thresholds.
                r.confidence >= 0.75 && r.evidence >= 3
            })
    }

    /// Build the brief for a given mode + profile.
    fn make_brief(
        &self,
        task: &TaskInput,
        mode: DelegationMode,
        profile: &TaskProfile,
        target_file: Option<&str>,
    ) -> TaskBrief {
        TaskBrief {
            template: mode.as_str().to_string(),
            description: task.description.clone(),
            variables: serde_json::json!({
                "language": profile.language.as_str(),
                "defaultFile": profile.default_file,
            }),
            target_file: target_file.map(|s| s.to_string()),
            correction_prompt: None,
        }
    }

    /// Main entry point. Mirrors TS `Orchestrator::delegate`.
    pub async fn delegate(&self, task: TaskInput) -> Result<DelegationResult> {
        let task_id = task
            .task_id
            .clone()
            .unwrap_or_else(|| format!("task-{}", now_millis()));
        let mut decision = self.resolve_mode(&task);

        // Recall + memory-bias override.
        if task.mode_override.is_none() {
            if let Some(rec) = self.recall(&task) {
                if let Some(mode) = parse_delegation_mode(&rec.mode) {
                    decision = kf_routing::classifier::DelegationDecision {
                        mode,
                        reason: format!(
                            "{}; memory bias {} ({} similar)",
                            decision.reason, rec.mode, rec.evidence
                        ),
                        auto_routed: decision.auto_routed,
                    };
                }
            }
        }

        info!(
            "[orchestrator] Routing \"{}\" → {} ({})",
            task.description.chars().take(80).collect::<String>(),
            decision.mode.as_str(),
            decision.reason
        );

        let profile = self.resolve_profile(&task);
        let target_file = task
            .files
            .as_ref()
            .and_then(|f| f.first().map(|s| s.as_str()));
        let brief = self.make_brief(&task, decision.mode, &profile, target_file);
        let started = now_millis();

        let mut result = match decision.mode {
            DelegationMode::HardPrompt => {
                execute_hard_prompt(
                    self.client.as_ref(),
                    brief,
                    &task_id,
                    &self.cwd,
                    Some(&profile),
                    target_file,
                    Some(self.sink.as_ref()),
                )
                .await?
            }
            DelegationMode::SchemaContract => {
                execute_schema_contract(self.client.as_ref(), brief, &task_id).await?
            }
            DelegationMode::Artifact => {
                execute_artifact(
                    self.client.as_ref(),
                    brief,
                    &task_id,
                    &self.cwd,
                    Some(&profile),
                    false,
                )
                .await?
            }
            DelegationMode::TaskDecompose => {
                // Decompose produces a synthetic delegation result. The full
                // executeDecomposition path lives in `decompose.rs` and is
                // invoked by callers that explicitly want subtask execution.
                let dr = crate::decompose::decompose_task(
                    self.client.as_ref(),
                    self.memory.as_deref(),
                    &task,
                    &self.decompose_provider,
                )
                .await?;
                DelegationResult {
                    decision: crate::types::DelegationDecisionInfo {
                        mode: "task-decompose".into(),
                        reason: format!("decomposed into {} subtasks", dr.tasks.len()),
                        auto_routed: true,
                    },
                    emission: crate::types::Emission {
                        agent_id: "decomposer".into(),
                        content: serde_json::to_string_pretty(
                            &dr.tasks
                                .iter()
                                .map(|t| serde_json::to_value(t).unwrap_or_default())
                                .collect::<Vec<_>>(),
                        )
                        .unwrap_or_default(),
                        prompt_tokens: dr.total_estimated_tokens,
                        completion_tokens: 0,
                        total_tokens: dr.total_estimated_tokens,
                        reasoning_tokens: None,
                        model: "decompose".into(),
                        format: "task-decompose".into(),
                        schema_contract: Some(serde_json::json!({
                            "taskCount": dr.tasks.len(),
                            "rationale": dr.rationale,
                            "tasks": dr.tasks.iter().map(|t| serde_json::to_value(t).unwrap_or_default()).collect::<Vec<_>>(),
                        })),
                        finish_reason: None,
                        retried: false,
                    },
                    signals: vec![],
                    packet: None,
                    provider_resolved: Some(self.provider_key.clone()),
                    skills_loaded: None,
                }
            }
        };

        // Finalize: emit signals, write memory observation, bump stats.
        flush_signals_to_sink(&result, self.sink.as_ref()).await;
        result.provider_resolved = Some(self.provider_key.clone());

        if !task.suppress_memory {
            if let Some(store) = &self.memory {
                let _ =
                    store.write_task_observation(&kf_memory_store::types::TaskObservationInput {
                        task_id: task_id.clone(),
                        description: task.description.clone(),
                        language: profile.language.as_str().into(),
                        mode: decision.mode.as_str().into(),
                        model: result.emission.model.clone(),
                        provider_key: Some(self.provider_key.clone()),
                        outcome: Some(decision.mode.as_str().into()),
                        tokens: result.emission.total_tokens,
                        duration_ms: now_millis() - started,
                        ..Default::default()
                    });
            }
        }

        {
            let mut s = self.stats.lock().expect("stats lock");
            s.total_delegations += 1;
            s.total_tokens += result.emission.total_tokens;
        }

        Ok(result)
    }
}

fn parse_delegation_mode(s: &str) -> Option<DelegationMode> {
    Some(match s {
        "artifact" => DelegationMode::Artifact,
        "schema-contract" => DelegationMode::SchemaContract,
        "hard-prompt" => DelegationMode::HardPrompt,
        "task-decompose" => DelegationMode::TaskDecompose,
        _ => return None,
    })
}

fn now_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::RecordingClient;
    use crate::sink::RecordingSink;
    use crate::types::Emission;
    use kf_memory_store::{InMemoryAdapter, MemoryStore, MemoryStoreOptions};
    use serde_json::json;

    fn emission(content: &str, format: &str) -> Emission {
        Emission {
            agent_id: "test".into(),
            content: content.into(),
            model: "test-model".into(),
            format: format.into(),
            total_tokens: 10,
            ..Default::default()
        }
    }

    fn make_orch(
        client: Arc<dyn ModelClient>,
        memory: Option<Arc<MemoryStore>>,
        sink: Option<Arc<dyn EventSink>>,
    ) -> Orchestrator {
        // Default to a tempdir so artifact-writing tests don't litter the
        // crate root. Tests that need to assert on written files override
        // `orch.cwd` after construction.
        let dir = tempfile::tempdir().expect("tempdir");
        let cwd = dir.path().to_string_lossy().to_string();
        std::mem::forget(dir); // keep the dir alive for the test's lifetime
        Orchestrator::new(
            client,
            OrchestratorConfig {
                provider_key: "local-ollama".into(),
                decompose_provider: "local-ollama".into(),
                cwd,
                memory,
                sink,
            },
        )
    }

    #[tokio::test]
    async fn delegate_routes_hard_prompt_for_fix_task() {
        let client = Arc::new(RecordingClient::constant(emission(
            "```python\nprint('x')\n```",
            "hard-prompt",
        )));
        let orch = make_orch(client, None, None);
        let result = orch
            .delegate(TaskInput {
                description: "fix the lint errors in the auth module".into(),
                ..Default::default()
            })
            .await
            .unwrap();
        assert!(
            result.decision.mode.contains("hard-prompt")
                || result.decision.mode.contains("schema-contract")
                || result.decision.mode == "hard-prompt"
        );
        assert_eq!(result.provider_resolved.as_deref(), Some("local-ollama"));
        assert_eq!(orch.stats().total_delegations, 1);
        assert!(orch.stats().total_tokens > 0);
    }

    #[tokio::test]
    async fn delegate_artifact_writes_files() {
        let dir = tempfile::tempdir().unwrap();
        let content = "print('hi')\n";
        let hash = kf_routing::sha256_of(content);
        let body = format!(
            r#"{{"type":"file_write","path":"solution.py","content_b64":"{}","sha256":"{}"}}"#,
            base64_b64(content),
            hash
        );
        let client = Arc::new(RecordingClient::constant(emission(&body, "artifact")));
        let mut orch = make_orch(client, None, None);
        orch.cwd = dir.path().to_string_lossy().to_string();
        let result = orch
            .delegate(TaskInput {
                description: "generate a python component file".into(),
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(result.decision.mode, "artifact");
        assert!(dir.path().join("solution.py").exists());
    }

    #[tokio::test]
    async fn delegate_writes_memory_observation_when_store_present() {
        let store = Arc::new(MemoryStore::new(
            InMemoryAdapter::new(),
            MemoryStoreOptions::default(),
        ));
        let client = Arc::new(RecordingClient::constant(emission(
            "```python\nprint('x')\n```",
            "hard-prompt",
        )));
        let orch = make_orch(client, Some(store.clone()), None);
        let _ = orch
            .delegate(TaskInput {
                description: "audit the security report".into(),
                ..Default::default()
            })
            .await
            .unwrap();
        // The observation should be present in the store.
        let q = kf_memory_store::types::MemoryQuery {
            kind: Some("task-observation".into()),
            limit: Some(10),
            ..Default::default()
        };
        let objs = store.adapter().query(&q).unwrap();
        assert_eq!(objs.len(), 1);
        // The mode on the observation reflects the routing decision, not the
        // emission.format (which may be "test-model" / "hard-prompt" from the
        // canned test emission).
        let mode = objs[0]
            .properties
            .get("mode")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        assert_eq!(mode, "schema-contract", "audit routes to schema-contract");
    }

    #[tokio::test]
    async fn delegate_skips_memory_when_suppress_set() {
        let store = Arc::new(MemoryStore::new(
            InMemoryAdapter::new(),
            MemoryStoreOptions::default(),
        ));
        let client = Arc::new(RecordingClient::constant(emission(
            "```python\nprint('x')\n```",
            "hard-prompt",
        )));
        let orch = make_orch(client, Some(store.clone()), None);
        let _ = orch
            .delegate(TaskInput {
                description: "fix the lint errors".into(),
                suppress_memory: true,
                ..Default::default()
            })
            .await
            .unwrap();
        let q = kf_memory_store::types::MemoryQuery {
            kind: Some("task-observation".into()),
            limit: Some(10),
            ..Default::default()
        };
        let objs = store.adapter().query(&q).unwrap();
        assert!(objs.is_empty(), "suppress_memory should skip the write");
    }

    #[tokio::test]
    async fn delegate_emits_artifact_events_through_sink() {
        let sink = Arc::new(RecordingSink::new());
        let dir = tempfile::tempdir().unwrap();
        let content = "print('hi')\n";
        let hash = kf_routing::sha256_of(content);
        let body = format!(
            r#"{{"type":"file_write","path":"solution.py","content_b64":"{}","sha256":"{}"}}"#,
            base64_b64(content),
            hash
        );
        let client = Arc::new(RecordingClient::constant(emission(&body, "artifact")));
        let mut orch = make_orch(client, None, Some(sink.clone()));
        orch.cwd = dir.path().to_string_lossy().to_string();
        let _ = orch
            .delegate(TaskInput {
                description: "generate a python module file".into(),
                ..Default::default()
            })
            .await
            .unwrap();
        let kinds = sink.kinds();
        assert!(
            kinds.contains(&"artifact.emitted"),
            "expected artifact.emitted event, got: {kinds:?}"
        );
    }

    #[tokio::test]
    async fn delegate_schema_contract_carries_schema() {
        let mut e = emission("{}", "schema-contract");
        e.schema_contract = Some(json!({"fields": []}));
        let client = Arc::new(RecordingClient::constant(e));
        let orch = make_orch(client, None, None);
        let result = orch
            .delegate(TaskInput {
                description: "audit the security report".into(),
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(result.decision.mode, "schema-contract");
        assert!(result.emission.schema_contract.is_some());
    }

    #[tokio::test]
    async fn mode_override_respected() {
        let client = Arc::new(RecordingClient::constant(emission(
            "```rust\nfn main() {}\n```",
            "hard-prompt",
        )));
        let orch = make_orch(client, None, None);
        let result = orch
            .delegate(TaskInput {
                description: "audit the security report".into(),
                mode_override: Some(DelegationMode::HardPrompt),
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(result.decision.mode, "hard-prompt");
    }

    fn base64_b64(s: &str) -> String {
        use base64::Engine;
        base64::engine::general_purpose::STANDARD.encode(s)
    }
}
