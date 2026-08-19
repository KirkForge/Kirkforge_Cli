//! Scout→coder→reviewer pipeline orchestration (WO 32.5; real pipeline
//! semantics WO 35.1; ModelClient seam WO 36.5).
//!
//! Three subagents run as a pipeline, not a fan-out: the Scout (read-only
//! `explore`) completes first and its context summary is injected into the
//! Coder's prompt; the Coder (write; own worktree when
//! `session.worktree_enabled`, WO 35.2) returns a change summary plus an
//! appliable diff patch, which is injected into the Reviewer's prompt; the
//! Reviewer (read-only `plan`) critiques the Coder's actual changes. Each
//! role registers a `TaskManager` entry so `cancel_all()` (WO 35.3) can stop
//! it cooperatively mid-flight — the entries are internal bookkeeping, not
//! rendered by `/jobs`.
//!
//! Since WO 36.5 roles execute through kf-orchestrator's `ModelClient`
//! seam (`ExecutorAdapter`): each role is a `TaskBrief` (persona, cancel
//! pair, and owner ride on the brief) and the emission's `content` is the
//! role summary — worktree patches still travel inside it via the WO 35.2
//! patch marker. One execution seam for the pipeline and the orchestrator
//! crate's delegation modes.

use crate::session::executor_adapter::ExecutorAdapter;
use crate::session::task_spawner::SUBAGENT_PATCH_MARKER;
use crate::shared::SharedConfig;
use crate::tools::task::{TaskHandle, TaskManager};
use crate::tools::UndoStackRef;
use kf_orchestrator::{BriefCancel, Emission, ModelClient, TaskBrief};
use std::sync::Arc;
use std::sync::Mutex;

/// Result of one pipeline orchestration run.
#[derive(Debug, Clone)]
pub struct ParallelResult {
    pub scout: SubagentResult,
    pub coder: SubagentResult,
    /// The Coder's appliable diff patch, extracted from its summary via the
    /// WO 35.2 patch marker. `None` when worktree isolation is off or the
    /// coder produced no changes.
    pub coder_patch: Option<String>,
    pub reviewer: SubagentResult,
}

/// One subagent's outcome: its TaskManager id and final summary.
#[derive(Debug, Clone)]
pub struct SubagentResult {
    pub task_id: String,
    pub summary: String,
}

impl ParallelResult {
    /// Human-readable multi-line summary for the TUI / line-mode output.
    pub fn summary(&self) -> String {
        let mut lines =
            vec!["Pipeline orchestration complete (scout → coder → reviewer):".to_string()];
        lines.push(format!(
            "  Scout    [{}] — {}",
            self.scout.task_id, self.scout.summary
        ));
        lines.push(format!(
            "  Coder    [{}] — {}",
            self.coder.task_id, self.coder.summary
        ));
        if let Some(patch) = &self.coder_patch {
            lines.push(format!(
                "  Coder patch: {} lines (apply in the parent with `git apply`)",
                patch.lines().count()
            ));
        }
        lines.push(format!(
            "  Reviewer [{}] — {}",
            self.reviewer.task_id, self.reviewer.summary
        ));
        lines.join("\n")
    }
}

/// Runs the Scout/Coder/Reviewer pipeline through kf-orchestrator's
/// `ModelClient` seam.
///
/// Holds one injectable `Arc<dyn ModelClient>` — production callers get
/// the `ExecutorAdapter`, which runs each role as an isolated subagent
/// session through the `task` tool's spawner path with CWD confinement,
/// landlock, and approval forwarding already wired by WO 32.4 / 30.6. No
/// new executor construction here.
pub struct ParallelOrchestrator {
    client: Arc<dyn ModelClient>,
    task_manager: Arc<Mutex<TaskManager>>,
}

impl ParallelOrchestrator {
    pub fn new(
        config: SharedConfig,
        model_name: String,
        ollama_host: String,
        undo_stack: Option<UndoStackRef>,
        supports_images: bool,
    ) -> Self {
        Self::with_client(Arc::new(ExecutorAdapter::new(
            config,
            model_name,
            ollama_host,
            undo_stack,
            supports_images,
        )))
    }

    // Test seam: inject a mock ModelClient — the trait's dyn dispatch
    // exists for exactly this. Production callers use `new`.
    fn with_client(client: Arc<dyn ModelClient>) -> Self {
        Self {
            client,
            task_manager: Arc::new(Mutex::new(TaskManager::new())),
        }
    }

    /// Run the scout→coder→reviewer pipeline. Entry point when worktree
    /// isolation is active (the coder then gets its own worktree, gated in
    /// `run_task_detailed` (via the brief) on `session.worktree_enabled`). Since WO 35.1 this is the
    /// same pipeline as `run_sequential` — the entry point no longer changes
    /// role ordering, only whether the coder is FS-isolated.
    pub async fn run_parallel(&self, task_description: &str) -> ParallelResult {
        self.run_pipeline(task_description).await
    }

    /// Run the scout→coder→reviewer pipeline. Entry point when worktree
    /// isolation is disabled (the coder shares the parent sandbox). Same
    /// pipeline as `run_parallel` since WO 35.1 — see its doc for the one
    /// remaining difference.
    pub async fn run_sequential(&self, task_description: &str) -> ParallelResult {
        self.run_pipeline(task_description).await
    }

    // The pipeline: scout's summary flows into the coder's prompt; the
    // coder's change summary + patch flow into the reviewer's prompt.
    async fn run_pipeline(&self, task_description: &str) -> ParallelResult {
        let scout = self
            .spawn_role("scout", "explore", task_description, None, 1)
            .await;
        let coder = self
            .spawn_role("coder", "coder", task_description, Some(&scout.summary), 1)
            .await;
        let reviewer = self
            .spawn_role(
                "reviewer",
                "plan",
                task_description,
                Some(&coder.summary),
                1,
            )
            .await;
        ParallelResult {
            scout,
            coder_patch: extract_patch(&coder.summary).map(str::to_string),
            coder,
            reviewer,
        }
    }

    /// Spawn one subagent with the given persona and upstream handoff,
    /// register it in the TaskManager, and await its result. The role runs
    /// as one `TaskBrief` through the `ModelClient` seam (WO 36.5): the
    /// persona marks the brief caller-framed, and the cancel pair + owner
    /// from the registered handle ride on the brief so `cancel_all` and
    /// cancel-by-owner keep working.
    async fn spawn_role(
        &self,
        role: &str,
        persona: &str,
        task_description: &str,
        handoff: Option<&str>,
        max_turns: usize,
    ) -> SubagentResult {
        let prompt = build_role_prompt(role, persona, task_description, handoff);
        let prompt_summary: String = prompt.chars().take(100).collect();

        // Insert a default handle (its WO 35.3 cancel pair — flag + token —
        // rides on the brief so cancel_all() can stop the in-flight role
        // cooperatively), then set metadata via get_mut.
        let (task_id, cancel) = {
            let mut mgr = self.task_manager.lock().unwrap_or_else(|e| e.into_inner());
            let handle = TaskHandle::default();
            let cancel = handle.cancel_handles();
            let id = mgr.insert(handle);
            if let Some(h) = mgr.get_mut(&id) {
                h.metadata.persona = persona.to_string();
                h.metadata.prompt_summary = prompt_summary;
            }
            (id, cancel)
        };

        let brief = TaskBrief {
            template: role.to_string(),
            description: prompt,
            variables: serde_json::Value::Null,
            target_file: None,
            correction_prompt: None,
            persona: Some(persona.to_string()),
            max_turns: Some(max_turns),
            owner: Some(task_id.clone()),
            cancel: Some(BriefCancel {
                flag: cancel.flag,
                token: cancel.token,
            }),
        };
        let summary = match self.client.execute(&brief).await {
            Ok(Emission { content, .. }) => content,
            Err(e) => format!("(failed: {e})"),
        };

        // Record the terminal state + duration so /jobs renders correctly.
        // A cancelled role keeps status Cancelled but retains its partial
        // output (incl. any worktree patch) in cancelled_result.
        {
            let mut mgr = self.task_manager.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(h) = mgr.get_mut(&task_id) {
                if h.cancel_requested.load(std::sync::atomic::Ordering::SeqCst) {
                    h.cancelled_result = Some(summary.clone());
                } else {
                    h.result = Some(summary.clone());
                }
            }
        }
        SubagentResult { task_id, summary }
    }

    /// Cooperatively cancel every in-flight role (WO 35.3). Each role's
    /// subagent session observes its cancel flag between turn steps and
    /// its token in-flight, runs cleanup, and returns. Returns the number
    /// of roles that were still cancellable.
    pub fn cancel_all(&self) -> usize {
        let mgr = self.task_manager.lock().unwrap_or_else(|e| e.into_inner());
        mgr.list()
            .iter()
            .filter(|e| !e.status.is_terminal())
            .map(|e| mgr.cancel(&e.id))
            .filter(|cancelled| *cancelled)
            .count()
    }
}

// Role prompt for one pipeline stage. `handoff` is the previous stage's
// output: the scout summary for the coder, the coder's change summary +
// patch for the reviewer. The returned prompt is passed to the role's
// brief verbatim (WO 35.1: no second generic wrapper on top).
fn build_role_prompt(role: &str, persona: &str, task: &str, handoff: Option<&str>) -> String {
    let handoff = handoff.unwrap_or("");
    match role {
        "scout" => format!(
            "You are the Scout in a scout→coder→reviewer pipeline. Explore \
             the codebase with read-only tools (read, grep, glob). Identify \
             the files relevant to the task, the patterns to follow, and any \
             risks. Produce a concise context summary for the Coder.\n\nTask: {task}"
        ),
        "coder" => format!(
            "You are the Coder in a scout→coder→reviewer pipeline. Implement \
             the task with the full toolset, using the Scout's context summary \
             below. Work efficiently and summarize what you changed and \
             why.\n\nTask: {task}\n\nScout context summary:\n{handoff}"
        ),
        "reviewer" => format!(
            "You are the Reviewer in a scout→coder→reviewer pipeline. Review \
             the Coder's changes below with read-only tools only — critique \
             the actual change summary and diff patch, not the task in the \
             abstract. Identify potential issues, edge cases, and \
             verification steps the Coder should run. End with: \
             \"## Review Complete\".\n\nTask: {task}\n\nCoder's change summary and diff:\n{handoff}"
        ),
        _ => {
            // Fallback: defer to the persona's default prompt builder.
            crate::tools::task::build_task_prompt(persona, task)
        }
    }
}

// Split the WO 35.2 patch artifact back out of a coder summary. Returns the
// patch text after the marker, or None when the summary carries no patch
// (worktree isolation off, or no changes).
fn extract_patch(summary: &str) -> Option<&str> {
    summary
        .split_once(SUBAGENT_PATCH_MARKER)
        .map(|(_, patch)| patch.trim())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parallel_result_summary_lists_all_three_roles() {
        let r = ParallelResult {
            scout: SubagentResult {
                task_id: "task-1".into(),
                summary: "found 3 files".into(),
            },
            coder: SubagentResult {
                task_id: "task-2".into(),
                summary: "edited foo.rs".into(),
            },
            coder_patch: None,
            reviewer: SubagentResult {
                task_id: "task-3".into(),
                summary: "looks good".into(),
            },
        };
        let s = r.summary();
        assert!(s.contains("Scout") && s.contains("task-1") && s.contains("found 3 files"));
        assert!(s.contains("Coder") && s.contains("task-2") && s.contains("edited foo.rs"));
        assert!(s.contains("Reviewer") && s.contains("task-3") && s.contains("looks good"));
        assert!(!s.contains("patch:"), "no patch line without a patch");
    }

    #[test]
    fn parallel_result_summary_notes_coder_patch_when_present() {
        let r = ParallelResult {
            scout: SubagentResult {
                task_id: "task-1".into(),
                summary: "found 3 files".into(),
            },
            coder: SubagentResult {
                task_id: "task-2".into(),
                summary: format!("edited foo.rs\n\n{SUBAGENT_PATCH_MARKER}\n+one\n+two"),
            },
            coder_patch: Some("+one\n+two".into()),
            reviewer: SubagentResult {
                task_id: "task-3".into(),
                summary: "looks good".into(),
            },
        };
        let s = r.summary();
        assert!(s.contains("Coder patch: 2 lines"), "got: {s}");
    }

    #[test]
    fn build_role_prompt_scout_mentions_read_only() {
        let p = build_role_prompt("scout", "explore", "do X", None);
        assert!(p.contains("Scout") && p.contains("read-only") && p.contains("do X"));
    }

    #[test]
    fn build_role_prompt_coder_mentions_implement() {
        let p = build_role_prompt("coder", "coder", "do Y", None);
        assert!(p.contains("Coder") && p.contains("Implement") && p.contains("do Y"));
    }

    #[test]
    fn build_role_prompt_reviewer_mentions_review_complete() {
        let p = build_role_prompt("reviewer", "plan", "do Z", None);
        assert!(p.contains("Reviewer") && p.contains("Review Complete") && p.contains("do Z"));
    }

    #[test]
    fn build_role_prompt_unknown_role_falls_back_to_persona_prompt() {
        let p = build_role_prompt("unknown", "explore", "do W", None);
        // The fallback delegates to tools::task's build_task_prompt which
        // mentions "research" for the explore persona.
        assert!(p.contains("research") || p.contains("do W"));
    }

    // WO 35.1 gate (a): the coder prompt carries the scout summary.
    #[test]
    fn build_role_prompt_coder_carries_scout_handoff() {
        let p = build_role_prompt(
            "coder",
            "coder",
            "do Y",
            Some("SCOUT: a.rs is load-bearing"),
        );
        assert!(
            p.contains("Scout context summary") && p.contains("SCOUT: a.rs is load-bearing"),
            "got: {p}"
        );
        // The pre-35.1 "may not have its context yet" excuse must be gone.
        assert!(!p.contains("may not have its context"));
    }

    // WO 35.1 gate (b): the reviewer prompt carries the coder's change
    // summary + diff patch.
    #[test]
    fn build_role_prompt_reviewer_carries_coder_handoff() {
        let coder_summary = "edited a.rs\n\n{SUBAGENT_PATCH_MARKER}\n+a.rs +1";
        let p = build_role_prompt("reviewer", "plan", "do Z", Some(coder_summary));
        assert!(
            p.contains("Coder's change summary and diff")
                && p.contains("edited a.rs")
                && p.contains("+a.rs +1"),
            "got: {p}"
        );
    }

    #[test]
    fn extract_patch_splits_on_marker_or_returns_none() {
        let with = format!("summary text\n\n{SUBAGENT_PATCH_MARKER}\n+diff\n-line");
        assert_eq!(extract_patch(&with), Some("+diff\n-line"));
        assert_eq!(extract_patch("no patch here"), None);
    }

    #[test]
    fn task_manager_tracks_three_roles_after_construction() {
        let config: SharedConfig =
            Arc::new(std::sync::RwLock::new(crate::shared::Config::default()));
        let orch =
            ParallelOrchestrator::new(config, "test-model".into(), "localhost".into(), None, false);
        // Fresh manager is empty until roles are spawned. (Private field —
        // the pub task_manager() accessor was removed in WO 35.1; /jobs
        // never read it.)
        assert!(orch.task_manager.lock().unwrap().list().is_empty());
    }

    // WO 35.3: cancel_all must cooperatively cancel every non-terminal
    // role handle (the flags/tokens spawn_role threads onto the brief).
    // End-to-end "roles stop" needs a live model (integration tier); the
    // wiring is proven per-layer: this test (manager), the task-tool
    // CooperativeSpawner test (request threading), and the executor
    // attached-token test (in-flight tool death).
    #[test]
    fn cancel_all_cancels_inflight_role_handles() {
        let config: SharedConfig =
            Arc::new(std::sync::RwLock::new(crate::shared::Config::default()));
        let orch =
            ParallelOrchestrator::new(config, "test-model".into(), "localhost".into(), None, false);
        let mgr = &orch.task_manager;
        // Simulate three in-flight roles: inserted handles with live cancel
        // pairs, exactly what spawn_role registers before the brief runs.
        let tokens: Vec<_> = {
            let mut guard = mgr.lock().unwrap();
            (0..3)
                .map(|_| {
                    let handle = TaskHandle::default();
                    let token = handle.cancel_handles().token;
                    guard.insert(handle);
                    token
                })
                .collect()
        };
        assert_eq!(orch.cancel_all(), 3);
        assert!(tokens.iter().all(|t| t.is_cancelled()));
        // Second call: everything terminal now — nothing left to cancel.
        assert_eq!(orch.cancel_all(), 0);
    }

    // ── WO 35.1: pipeline semantics at the TaskSpawner seam ──

    // Records start/end events per role and the exact prompt each role
    // received, returning persona-canned summaries so the handoff content
    // is observable. The 10ms in-flight pause gives a (wrong) concurrent
    // dispatch room to interleave its starts before the first end.
    struct PipelineProbe {
        events: Arc<Mutex<Vec<String>>>,
        prompts: Arc<Mutex<Vec<(String, String)>>>,
    }

    impl PipelineProbe {
        fn phase(persona: &str) -> String {
            match persona {
                "explore" => "scout".to_string(),
                "plan" => "reviewer".to_string(),
                other => other.to_string(),
            }
        }

        fn canned_summary(persona: &str) -> String {
            match persona {
                "explore" => "SCOUT-SUMMARY: a.rs and b.rs are relevant".to_string(),
                "coder" => {
                    format!("CODED: edited a.rs\n\n{SUBAGENT_PATCH_MARKER}\n+a.rs +1 line")
                }
                _ => "REVIEW: looks good".to_string(),
            }
        }

        fn orchestrator(self) -> ParallelOrchestrator {
            ParallelOrchestrator::with_client(Arc::new(self))
        }
    }

    // The probe speaks the ModelClient seam (WO 36.5): the brief's
    // persona identifies the role, its description is the role prompt.
    #[async_trait::async_trait]
    impl ModelClient for PipelineProbe {
        async fn execute(&self, brief: &TaskBrief) -> anyhow::Result<Emission> {
            let persona = brief.persona.clone().unwrap_or_default();
            let phase = Self::phase(&persona);
            self.events.lock().unwrap().push(format!("start:{phase}"));
            self.prompts
                .lock()
                .unwrap()
                .push((persona.clone(), brief.description.clone()));
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            self.events.lock().unwrap().push(format!("end:{phase}"));
            Ok(Emission {
                agent_id: "pipeline-probe".into(),
                content: Self::canned_summary(&persona),
                format: brief.template.clone(),
                ..Default::default()
            })
        }
    }
    // WO 35.1 gate (c): roles run strictly scout → coder → reviewer — the
    // reviewer must not start before the coder completes. Also proves the
    // prompt handoffs (a)/(b) end-to-end and the coder_patch extraction.
    #[tokio::test]
    async fn run_pipeline_sequences_roles_and_threads_context() {
        let probe = PipelineProbe {
            events: Arc::new(Mutex::new(Vec::new())),
            prompts: Arc::new(Mutex::new(Vec::new())),
        };
        let events = probe.events.clone();
        let prompts = probe.prompts.clone();
        let orch = probe.orchestrator();

        let result = orch.run_parallel("refactor the frobnicator").await;

        // (c) strict pipeline order: each role ends before the next starts.
        // A tokio::join! fan-out would emit all three starts first.
        let ev = events.lock().unwrap().clone();
        assert_eq!(
            ev,
            vec![
                "start:scout",
                "end:scout",
                "start:coder",
                "end:coder",
                "start:reviewer",
                "end:reviewer"
            ],
            "roles must run strictly in sequence, got {ev:?}"
        );

        // (a) coder prompt contains the scout summary; no generic wrapper
        // doubles it up (WO 35.1 double-wrap fix).
        let prompts = prompts.lock().unwrap().clone();
        let coder_prompt = &prompts
            .iter()
            .find(|(p, _)| p == "coder")
            .expect("coder prompt recorded")
            .1;
        assert!(
            coder_prompt.contains("SCOUT-SUMMARY: a.rs and b.rs are relevant"),
            "coder must receive the scout summary, got: {coder_prompt}"
        );
        assert!(
            !coder_prompt.contains("focused implementation assistant"),
            "role prompt must not be double-wrapped in the generic preamble"
        );

        // (b) reviewer prompt contains the coder's change summary + patch.
        let reviewer_prompt = &prompts
            .iter()
            .find(|(p, _)| p == "plan")
            .expect("reviewer prompt recorded")
            .1;
        assert!(
            reviewer_prompt.contains("CODED: edited a.rs")
                && reviewer_prompt.contains("+a.rs +1 line"),
            "reviewer must receive the coder's diff, got: {reviewer_prompt}"
        );

        // The patch is extracted into the result (WO 35.2 artifact reuse).
        assert_eq!(result.coder_patch.as_deref(), Some("+a.rs +1 line"));
        assert_eq!(result.reviewer.summary, "REVIEW: looks good");
        assert_eq!(
            result.scout.summary,
            "SCOUT-SUMMARY: a.rs and b.rs are relevant"
        );
    }

    // run_sequential shares the pipeline: same handoffs, same order.
    #[tokio::test]
    async fn run_sequential_is_the_same_pipeline() {
        let probe = PipelineProbe {
            events: Arc::new(Mutex::new(Vec::new())),
            prompts: Arc::new(Mutex::new(Vec::new())),
        };
        let events = probe.events.clone();
        let prompts = probe.prompts.clone();
        let orch = probe.orchestrator();

        let result = orch.run_sequential("do the thing").await;
        assert_eq!(
            events.lock().unwrap().clone(),
            vec![
                "start:scout",
                "end:scout",
                "start:coder",
                "end:coder",
                "start:reviewer",
                "end:reviewer"
            ]
        );
        let reviewer_prompt = prompts
            .lock()
            .unwrap()
            .iter()
            .find(|(p, _)| p == "plan")
            .map(|(_, prompt)| prompt.clone())
            .expect("reviewer prompt recorded");
        assert!(reviewer_prompt.contains("+a.rs +1 line"));
        assert!(result.coder_patch.is_some());
    }
}
