//! Production `ModelClient` for kf-orchestrator (WO 35.6, ADR-075).
//!
//! `Emission` models ONE model turn; the Executor runs a multi-turn
//! tool-using session. Per ADR-075 the adapter flattens the session:
//! `content` is the final assistant message, usage fields are the sum
//! of every turn's `CostStats`, `format` echoes `TaskBrief.template`,
//! and `finish_reason` is derived from the turn outcome. A session
//! variant on `Emission` was rejected because the orchestrator's mode
//! executors parse `content` as text.

use anyhow::Result;
use async_trait::async_trait;
use kf_orchestrator::{Emission, ModelClient, TaskBrief};

use crate::session::task_spawner::InProcessTaskSpawner;
use crate::shared::SharedConfig;
use crate::tools::task::{TaskCancel, TaskRequest};
use crate::tools::UndoStackRef;

// Turn budget for one orchestrated delegation. The executor loop ends a
// session early on a tool-call-free assistant message, so this is a
// ceiling, not a fixed cost.
// ponytail: single constant, no config knob — bump when a mode genuinely
// needs longer sessions; wire it into [tools] if two modes disagree.
const ORCHESTRATOR_MAX_TURNS: usize = 8;

/// kf-code-side `ModelClient`: runs each `TaskBrief` as an isolated
/// subagent session through the `task` tool's spawner path.
pub struct ExecutorAdapter {
    config: SharedConfig,
    model_name: String,
    ollama_host: String,
    undo_stack: Option<UndoStackRef>,
    supports_images: bool,
    // WO 50.05 H1: nesting depth threaded into every role's TaskRequest so
    // the `task` tool's ceiling guard sees the real level. The orchestrator
    // pipeline is a top-level caller, so production sets this to 1 (roles
    // are one level below the root session).
    subagent_depth: usize,
}

impl ExecutorAdapter {
    pub fn new(
        config: SharedConfig,
        model_name: String,
        ollama_host: String,
        undo_stack: Option<UndoStackRef>,
        supports_images: bool,
        subagent_depth: usize,
    ) -> Self {
        Self {
            config,
            model_name,
            ollama_host,
            undo_stack,
            supports_images,
            subagent_depth,
        }
    }
}

// Decomposition is read-only analysis; the three writer modes may pin a
// target file, so they get the full coder toolset.
fn persona_for_template(template: &str) -> &'static str {
    match template {
        "task-decompose" => "plan",
        _ => "coder",
    }
}

// Map the brief onto the subagent prompt. Mode-specific instruction
// (JSON shapes, code fences) is the orchestrator's job — it rides in
// `description`; the adapter only adds the mode frame and the brief
// fields the modes set.
fn brief_prompt(brief: &TaskBrief) -> String {
    let mut out = format!(
        "You are executing the \"{}\" delegation mode for the KirkForge orchestrator.\n\nTask:\n{}\n",
        brief.template, brief.description
    );
    if !brief.variables.is_null() {
        out.push_str(&format!("\nVariables:\n{}\n", brief.variables));
    }
    if let Some(target) = &brief.target_file {
        out.push_str(&format!("\nTarget file: write your result to {target}.\n"));
    }
    if let Some(correction) = &brief.correction_prompt {
        out.push_str(&format!(
            "\nVerifier feedback on a previous attempt — address every item \
             before finishing:\n{correction}\n"
        ));
    }
    out
}

#[async_trait]
impl ModelClient for ExecutorAdapter {
    async fn execute(&self, brief: &TaskBrief) -> Result<Emission> {
        // Per-call spawner: run_task_detailed builds its own executor +
        // conversation from the config snapshot, so there is no state to
        // carry between calls (simplest &self story).
        let spawner = InProcessTaskSpawner::new(
            self.config.clone(),
            self.model_name.clone(),
            self.ollama_host.clone(),
            self.undo_stack.clone(),
            self.supports_images,
        );
        // WO 36.5 execution hints: a brief carrying a persona is
        // caller-framed (pipeline roles own their complete prompt — no
        // mode frame on top, same one-wrapper rule as WO 35.1); a plain
        // delegation-mode brief gets the adapter's frame below.
        let persona = brief
            .persona
            .clone()
            .unwrap_or_else(|| persona_for_template(&brief.template).to_string());
        let prompt = if brief.persona.is_some() {
            brief.description.clone()
        } else {
            brief_prompt(brief)
        };
        let detail = spawner
            .run_task_detailed(TaskRequest {
                prompt,
                persona,
                model: None,
                max_turns: brief.max_turns.unwrap_or(ORCHESTRATOR_MAX_TURNS),
                cancel: brief.cancel.clone().map(|c| TaskCancel {
                    flag: c.flag,
                    token: c.token,
                }),
                owner: brief.owner.clone(),
                // WO 50.05 H1: thread the adapter's depth so orchestrator
                // roles don't reset the ceiling to 0.
                subagent_depth: self.subagent_depth,
                pending_messages: None,
            })
            .await
            .map_err(anyhow::Error::msg)?;

        Ok(Emission {
            agent_id: "executor-adapter".into(),
            content: detail.summary,
            model: self.model_name.clone(),
            // Spec: the format field echoes the delegation mode that
            // requested the emission.
            format: brief.template.clone(),
            prompt_tokens: detail.prompt_tokens,
            completion_tokens: detail.completion_tokens,
            total_tokens: detail.prompt_tokens + detail.completion_tokens,
            finish_reason: Some(detail.finish_reason),
            ..Default::default()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shared::Config;
    use std::collections::HashMap;
    use std::sync::Arc;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn adapter_config(mock_uri: &str) -> SharedConfig {
        let mut cfg = Config::default();
        cfg.model.request_timeout_secs = 10;
        // Force Ollama routing for the e2e model name (same extension
        // point the wiremock executor tests use).
        cfg.model.adapter_routing = HashMap::from([("e2e-".to_string(), "Ollama".to_string())]);
        let _ = mock_uri;
        Arc::new(std::sync::RwLock::new(cfg))
    }

    async fn mount_reply(server: &MockServer, content: &str, usage: &str) {
        // Trailing newline is load-bearing: the NDJSON parser only decodes
        // complete \n-terminated lines.
        let body = format!(
            r#"{{"message":{{"content":"{content}"}},"done":true,"done_reason":"stop","usage":{usage}}}"#
        ) + "\n";
        Mock::given(method("POST"))
            .and(path("/api/chat"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "application/x-ndjson")
                    .set_body_string(body),
            )
            .mount(server)
            .await;
    }

    #[test]
    fn persona_maps_decompose_to_plan_and_writers_to_coder() {
        assert_eq!(persona_for_template("task-decompose"), "plan");
        assert_eq!(persona_for_template("hard-prompt"), "coder");
        assert_eq!(persona_for_template("schema-contract"), "coder");
        assert_eq!(persona_for_template("artifact"), "coder");
    }

    #[test]
    fn brief_prompt_carries_every_brief_field() {
        let brief = TaskBrief {
            template: "artifact".into(),
            description: "generate a python module".into(),
            variables: serde_json::json!({"language": "python"}),
            target_file: Some("solution.py".into()),
            correction_prompt: Some("fix the missing hash".into()),
            ..Default::default()
        };
        let prompt = brief_prompt(&brief);
        assert!(prompt.contains("\"artifact\" delegation mode"));
        assert!(prompt.contains("generate a python module"));
        assert!(
            prompt.contains("\"language\": \"python\"")
                || prompt.contains("\"language\":\"python\"")
        );
        assert!(prompt.contains("write your result to solution.py"));
        assert!(prompt.contains("fix the missing hash"));
    }

    // Spec-conformance gate from the workorder: format echoes the
    // template, token fields sum the session's CostStats, content is
    // the final assistant message, finish_reason is set.
    #[tokio::test]
    async fn execute_produces_spec_conformant_emission() {
        let server = MockServer::start().await;
        mount_reply(
            &server,
            "all done",
            r#"{"prompt_tokens":3,"completion_tokens":5}"#,
        )
        .await;

        let adapter = ExecutorAdapter::new(
            adapter_config(&server.uri()),
            "e2e-test-model".into(),
            server.uri(),
            None,
            false,
            1,
        );
        let brief = TaskBrief {
            template: "hard-prompt".into(),
            description: "say hi".into(),
            ..Default::default()
        };
        let emission = adapter.execute(&brief).await.expect("execute must succeed");

        assert_eq!(emission.format, "hard-prompt", "format echoes the template");
        assert_eq!(
            emission.content, "all done",
            "content is the final assistant message"
        );
        assert_eq!(emission.prompt_tokens, 3);
        assert_eq!(emission.completion_tokens, 5);
        assert_eq!(emission.total_tokens, 8, "total = prompt + completion");
        assert_eq!(emission.finish_reason.as_deref(), Some("stop"));
        assert_eq!(emission.model, "e2e-test-model");
        assert!(!emission.was_truncated());
    }

    // WO 36.5 step 2: Orchestrator::delegate is drivable end-to-end from
    // the binary's own executor — classify → brief → ExecutorAdapter →
    // spawner → wiremock, back through hard-prompt finalize into the
    // DelegationResult. The sink wiring also proves the WO 36.6 bridge
    // end-to-end: the artifact.emitted event lands on the bus.
    #[tokio::test]
    async fn delegate_runs_end_to_end_through_the_real_adapter() {
        let server = MockServer::start().await;
        mount_reply(
            &server,
            "all done",
            r#"{"prompt_tokens":7,"completion_tokens":11}"#,
        )
        .await;

        let dir = tempfile::tempdir().expect("tempdir");
        let bus = crate::shared::event_bus::EventBus::default();
        let sink = crate::session::event_sink_bridge::EventBusSink::new(bus.clone());
        let seen: std::sync::Arc<std::sync::Mutex<Vec<crate::shared::event_bus::Event>>> =
            Default::default();
        let recorder = seen.clone();
        let _unsub = bus.on(
            &crate::shared::event_bus::BusEventKind::ArtifactEmitted,
            move |e| {
                recorder.lock().unwrap().push(e);
                std::future::ready(Ok(()))
            },
        );

        let orch = kf_orchestrator::Orchestrator::new(
            Arc::new(ExecutorAdapter::new(
                adapter_config(&server.uri()),
                "e2e-test-model".into(),
                server.uri(),
                None,
                false,
                1,
            )),
            kf_orchestrator::OrchestratorConfig {
                provider_key: "local-ollama".into(),
                decompose_provider: "local-ollama".into(),
                cwd: dir.path().to_string_lossy().to_string(),
                memory: None,
                sink: Some(Arc::new(sink)),
            },
        );

        let result = orch
            .delegate(kf_orchestrator::TaskInput {
                description: "fix the lint errors in the auth module".into(),
                mode_override: Some(kf_orchestrator::DelegationMode::HardPrompt),
                ..Default::default()
            })
            .await
            .expect("delegate must succeed");

        assert_eq!(result.decision.mode, "hard-prompt");
        assert_eq!(result.emission.content, "all done");
        assert_eq!(result.emission.format, "hard-prompt");
        assert_eq!(result.emission.total_tokens, 18, "7 prompt + 11 completion");
        assert_eq!(result.provider_resolved.as_deref(), Some("local-ollama"));
        assert_eq!(orch.stats().total_delegations, 1);

        // WO 37.2 (ADR-076): the reducer attaches a real packet — this
        // clean delegation (no files written) folds to Pass.
        let packet = result.packet.as_ref().expect("packet must be populated");
        assert_eq!(
            packet.verification.overall,
            kf_orchestrator::OverallVerdict::Pass
        );
        assert_eq!(packet.turn, 0);

        // The delegate() flush is awaited, so the bus handler has run.
        let events = seen.lock().unwrap().clone();
        assert_eq!(
            events.len(),
            1,
            "artifact.emitted must reach the bus, got {events:?}"
        );
        assert_eq!(
            events[0].kind,
            crate::shared::event_bus::BusEventKind::ArtifactEmitted
        );
        assert!(events[0].value.as_ref().unwrap()["taskId"]
            .as_str()
            .is_some_and(|t| t.starts_with("task-")));
    }
}
