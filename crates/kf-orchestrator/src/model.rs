//! `ModelClient` — async seam between the orchestrator's mode executors and
//! whatever actually runs the LLM turn. The TS code instantiates `Agent` with
//! a `ModelProviderConfig` and a prompt template; here we collapse that to a
//! single async call returning an [`Emission`].
//!
//! The production wiring lives in the kf-code binary (WO 35.6):
//! `session::executor_adapter::ExecutorAdapter` implements this trait over
//! the executor's subagent sessions (see ADR-075 for the flattening
//! decision). Tests use [`RecordingClient`] which returns canned emissions
//! from a queue.

use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use crate::types::Emission;

/// Cooperative-cancel handles a brief carries to the production adapter
/// (WO 36.5): the flag ends the turn loop between steps, the token kills
/// in-flight tool work. Mirrors the binary's `TaskCancel` pair — defined
/// here so `TaskBrief` stays crate-local (the binary cannot be a dep).
#[derive(Debug, Clone)]
pub struct BriefCancel {
    pub flag: Arc<AtomicBool>,
    pub token: CancellationToken,
}

/// Inputs the orchestrator hands to a model. `template` is the
/// TS template name (`"hard-prompt"`, `"schema-contract"`, `"artifact"`,
/// `"task-decompose"`); the production adapter maps it to its own
/// system-prompt registry. `description` and `variables` are the brief.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TaskBrief {
    pub template: String,
    pub description: String,
    #[serde(default)]
    pub variables: serde_json::Value,
    /// Pinned target file (the harness --file arg). When set, mode
    /// executors treat the emission as overwriting that file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_file: Option<String>,
    /// Verifier feedback appended by the correction loop. Lets the
    /// production adapter decide how to splice it into the prompt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub correction_prompt: Option<String>,
    // ── Execution hints (WO 36.5) ── consumed only by the production
    // adapter; `None` everywhere is the unchanged delegation-mode path.
    // Not serialized: they are per-call execution plumbing, not brief
    // content. `persona` set ⇒ the caller owns the prompt frame — the
    // adapter runs `description` verbatim as the complete subagent
    // prompt (pipeline roles); unset ⇒ the adapter frames the
    // delegation-mode prompt itself.
    #[serde(skip)]
    pub persona: Option<String>,
    #[serde(skip)]
    pub max_turns: Option<usize>,
    /// Owning task id (WO 36.2): tags the subagent's background bash
    /// jobs so cancel-by-owner reaches them.
    #[serde(skip)]
    pub owner: Option<String>,
    #[serde(skip)]
    pub cancel: Option<BriefCancel>,
}

#[async_trait]
pub trait ModelClient: Send + Sync {
    /// Run one model turn against `brief`. The returned [`Emission`]’s
    /// `format` field must echo the delegation mode that requested it
    /// (`"hard-prompt"`/`"schema-contract"`/`"artifact"`/`"task-decompose"`).
    async fn execute(&self, brief: &TaskBrief) -> Result<Emission>;
}

/// Stub client used by `Orchestrator::new` when no model adapter is wired.
/// Panics on `execute` to make the "not yet wired" failure loud at the call
/// site rather than silent in test output.
pub struct PanickingClient;

#[async_trait]
impl ModelClient for PanickingClient {
    async fn execute(&self, _brief: &TaskBrief) -> Result<Emission> {
        panic!("kf-orchestrator: ModelClient not wired (WO 29.7 deferral). Pass a real ModelClient to Orchestrator::new.");
    }
}

/// Test double: pops canned emissions from a FIFO queue. If the queue is
/// empty when `execute` is called, returns an error rather than panicking
/// so tests can assert the no-response path.
pub struct RecordingClient {
    emissions: Mutex<Vec<Emission>>,
    calls: Mutex<Vec<TaskBrief>>,
}

impl RecordingClient {
    pub fn new(emissions: Vec<Emission>) -> Self {
        Self {
            emissions: Mutex::new(emissions),
            calls: Mutex::new(Vec::new()),
        }
    }

    /// One emission returned for every call (looped).
    pub fn constant(emission: Emission) -> Self {
        Self::new(vec![emission])
    }

    /// Snapshot of the briefs handed to `execute`.
    pub fn calls(&self) -> Vec<TaskBrief> {
        self.calls.lock().expect("calls lock").clone()
    }
}

#[async_trait]
impl ModelClient for RecordingClient {
    async fn execute(&self, brief: &TaskBrief) -> Result<Emission> {
        self.calls.lock().expect("calls lock").push(brief.clone());
        let mut q = self.emissions.lock().expect("emissions lock");
        if q.is_empty() {
            anyhow::bail!("RecordingClient queue empty");
        }
        if q.len() == 1 {
            Ok(q[0].clone())
        } else {
            Ok(q.remove(0))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[tokio::test]
    async fn recording_client_returns_constant_emission() {
        let c = RecordingClient::constant(emission("hi", "hard-prompt"));
        let b = TaskBrief {
            description: "do thing".into(),
            ..Default::default()
        };
        let e = c.execute(&b).await.unwrap();
        assert_eq!(e.content, "hi");
        let e2 = c.execute(&b).await.unwrap();
        assert_eq!(e2.content, "hi");
        assert_eq!(c.calls().len(), 2);
    }

    #[tokio::test]
    async fn recording_client_pops_queue_in_order() {
        let c = RecordingClient::new(vec![
            emission("first", "hard-prompt"),
            emission("second", "hard-prompt"),
        ]);
        let b = TaskBrief::default();
        assert_eq!(c.execute(&b).await.unwrap().content, "first");
        assert_eq!(c.execute(&b).await.unwrap().content, "second");
    }

    #[tokio::test]
    async fn recording_client_errors_when_queue_empty() {
        let c = RecordingClient::new(vec![]);
        assert!(c.execute(&TaskBrief::default()).await.is_err());
    }
}
