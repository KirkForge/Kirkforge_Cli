//! `/workflow` slash-command handler.
//!
//! Workflows are user-editable JSON DAGs defined in the `kf-workflow`
//! crate. This module wires them into the TUI: loading, command dispatch,
//! rendering, and cancellation. Each step is executed via the existing
//! `task` tool's `InProcessTaskSpawner` through a thin `StepRunner`
//! implementation. The `--parallel` flag (WO 32.5) dispatches to
//! `ParallelOrchestrator` for the scout→coder→reviewer pipeline (WO 35.1
//! gave it real stage-to-stage context handoff) instead of the sequential
//! DAG runner.

use crate::shared::read_shared_config;
use crate::tools::task::TaskSpawner;
use crate::tui::app::AppState;
use crate::tui::commands::PersonaResult;
use anyhow::Result;
use kf_workflow::{StepOutput, StepRunner, Workflow, WorkflowExecutor};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

/// Handle to a workflow currently running in the background.
#[derive(Debug, Clone)]
pub struct WorkflowHandle {
    pub name: String,
    pub step_count: usize,
    pub completed: Vec<String>,
    pub outputs: HashMap<String, StepOutput>,
}

impl WorkflowHandle {
    pub fn status_line(&self) -> String {
        let done = self.completed.len();
        let total = self.step_count;
        let pct = if total == 0 { 0 } else { done * 100 / total };
        format!(
            "workflow {}: {}/{} steps ({}%)",
            self.name, done, total, pct
        )
    }

    pub fn summary(&self) -> String {
        let mut lines = vec![format!("Workflow '{}' complete:", self.name)];
        for name in self.ordered_step_names() {
            if let Some(out) = self.outputs.get(&name) {
                lines.push(format!("  {} [{}] — {}", name, out.persona, out.summary));
                if let Some(critique) = &out.critique {
                    lines.push(format!("    critique: {critique}"));
                }
            } else {
                lines.push(format!("  {name} — pending"));
            }
        }
        lines.join("\n")
    }

    fn ordered_step_names(&self) -> Vec<String> {
        let mut names: Vec<String> = self.outputs.keys().cloned().collect();
        names.sort();
        names
    }
}

/// Run the `/workflow` command.
///
/// Subcommands:
///   `/workflow run <name>`          — load and start a workflow.
///   `/workflow run <name> --parallel` — scout/coder/reviewer fan-out (WO 32.5).
///   `/workflow status`              — show step progress of the running workflow.
///   `/workflow cancel`              — abort the running workflow.
pub async fn handle_workflow_command(
    args: &str,
    state: &mut AppState,
    completion_tx: tokio::sync::mpsc::UnboundedSender<PersonaResult>,
) -> String {
    let trimmed = args.trim();
    let (sub, rest) = trimmed.split_once(' ').unwrap_or((trimmed, ""));
    let sub = sub.trim();
    let rest = rest.trim();

    match sub {
        "run" => handle_run(rest, state, completion_tx).await,
        "status" => handle_status(state),
        "cancel" => handle_cancel(state),
        _ => {
            if sub.is_empty() {
                "Usage: /workflow run <name> [--parallel] | status | cancel".into()
            } else {
                format!(
                    "Usage: /workflow run <name> [--parallel] | status | cancel\nGot: /workflow {sub} {rest}"
                )
            }
        }
    }
}

fn handle_status(state: &AppState) -> String {
    match &state.generation.workflow_in_progress {
        Some(h) => {
            let mut out = h.status_line();
            out.push('\n');
            out.push_str("Completed steps: ");
            if h.completed.is_empty() {
                out.push_str("none");
            } else {
                out.push_str(&h.completed.join(", "));
            }
            out
        }
        None => "No workflow is currently running. Use /workflow run <name>.".into(),
    }
}

fn handle_cancel(state: &mut AppState) -> String {
    // WO 38.4: the parallel path needs a real cancel — the shared flag
    // below is only ever read by the sequential DAG runner. Reach the
    // live orchestrator: cancel_all() stops in-flight roles and arms the
    // pipeline flag so phases that never registered a handle (reviewer
    // while scout runs) never start.
    let cancelled_parallel = state
        .generation
        .workflow_orchestrator
        .take()
        .map(|orch| orch.cancel_all())
        .is_some();
    if let Some(cancel) = state.generation.workflow_cancel.take() {
        cancel.store(true, Ordering::SeqCst);
        state.generation.workflow_in_progress = None;
        state.generation.workflow_cancel = None;
        state.generation.workflow_orchestrator = None;
        "⛔ Workflow cancelled.".into()
    } else if cancelled_parallel {
        state.generation.workflow_in_progress = None;
        state.generation.workflow_orchestrator = None;
        "⛔ Workflow cancelled.".into()
    } else {
        "No workflow is running.".into()
    }
}

async fn handle_run(
    name: &str,
    state: &mut AppState,
    completion_tx: tokio::sync::mpsc::UnboundedSender<PersonaResult>,
) -> String {
    // WO 32.5: `--parallel` flag triggers scout/coder/reviewer fan-out
    // instead of the sequential DAG runner.
    let (name, parallel) = if let Some(stripped) = name.strip_suffix("--parallel") {
        (stripped.trim().to_string(), true)
    } else if let Some(stripped) = name.strip_suffix("--parallel ") {
        (stripped.trim().to_string(), true)
    } else {
        (name.trim().to_string(), false)
    };

    if name.is_empty() {
        return "Usage: /workflow run <name> [--parallel]".into();
    }

    if state.generation.workflow_in_progress.is_some() {
        return format!(
            "A workflow ('{}') is already running. /workflow status or /workflow cancel first.",
            state.generation.workflow_in_progress.as_ref().unwrap().name
        );
    }

    let path = match kf_workflow::find_workflow_file(&name) {
        Some(p) => p,
        None => {
            return format!(
                "Workflow '{name}' not found. Looked in .kf-code/workflows/{name}.json and ~/.local/share/kf-code/workflows/{name}.json"
            );
        }
    };

    let workflow = match Workflow::from_file(&path) {
        Ok(w) => w,
        Err(e) => return format!("Failed to load workflow '{name}': {e}"),
    };

    let cfg = read_shared_config(&state.services.config).clone();
    let shared_cfg: crate::shared::SharedConfig = std::sync::Arc::new(std::sync::RwLock::new(cfg));
    let model_name = state
        .provider
        .model_info
        .as_ref()
        .map(|m| m.name.clone())
        .unwrap_or_else(|| {
            let c = crate::shared::read_shared_config(&shared_cfg);
            c.model.default_model.clone()
        });
    let ollama_host = {
        let c = crate::shared::read_shared_config(&shared_cfg);
        c.model.ollama_host.clone()
    };
    let supports_images = state
        .provider
        .model_info
        .as_ref()
        .map(|m| m.supports_images)
        .unwrap_or(false);
    let undo_stack = state.session.undo_stack.clone();

    let step_count = workflow.steps.len();
    let handle = WorkflowHandle {
        name: name.to_string(),
        step_count,
        completed: Vec::new(),
        outputs: HashMap::new(),
    };
    state.generation.workflow_in_progress = Some(handle);

    let cancel = Arc::new(AtomicBool::new(false));
    state.generation.workflow_cancel = Some(cancel.clone());

    let name_for_spawn = name.clone();
    if parallel {
        // WO 32.5/35.1: scout→coder→reviewer pipeline. The workflow's
        // first agent step prompt becomes the task description for all
        // three roles; each stage's output is handed to the next. The
        // worktree flag selects the entry point (coder isolation), not
        // the ordering — both entries run the same pipeline.
        let task_description = workflow
            .steps
            .iter()
            .find(|s| s.prompt.is_some())
            .and_then(|s| s.prompt.clone())
            .unwrap_or_else(|| {
                tracing::warn!(
                    "parallel mode with no prompt-bearing step; using workflow name as task"
                );
                name.clone()
            });
        let worktree_enabled = {
            let c = crate::shared::read_shared_config(&shared_cfg);
            c.session.worktree_enabled
        };
        // WO 38.4: constructed here (not inside the spawn) so the TUI
        // state keeps an Arc for `/workflow cancel` to reach.
        let orchestrator = Arc::new(
            crate::session::parallel_orchestrator::ParallelOrchestrator::new(
                shared_cfg,
                model_name,
                ollama_host,
                undo_stack,
                supports_images,
            ),
        );
        state.generation.workflow_orchestrator = Some(orchestrator.clone());
        tokio::spawn(async move {
            let result = if worktree_enabled {
                orchestrator.run_parallel(&task_description).await
            } else {
                tracing::info!("worktree disabled; coder runs without an isolated worktree");
                orchestrator.run_sequential(&task_description).await
            };
            // WO 38.4: an aborted pipeline (prior-phase error or cancel)
            // is a failure — the summary names the reason; no lying UI.
            let (success, error) = match &result.aborted {
                None => (true, None),
                Some(reason) => (false, Some(reason.clone())),
            };
            let summary = result.summary();
            crate::send_or_warn!(
                completion_tx.send(PersonaResult {
                    kind: crate::tui::commands::PersonaKind::Coder,
                    task: format!("workflow {name_for_spawn} (parallel)"),
                    success,
                    summary,
                    error,
                }),
                "parallel workflow completion channel receiver dropped"
            );
        });
        let mode = if worktree_enabled {
            "coder worktree isolated"
        } else {
            "coder worktree shared"
        };
        return format!("🚀 Started workflow '{name}' — scout/coder/reviewer pipeline ({mode}).");
    }

    tokio::spawn(async move {
        let runner = TuiStepRunner {
            model_name,
            ollama_host,
            config: shared_cfg,
            supports_images,
            undo_stack,
            handle: Arc::new(Mutex::new(WorkflowHandle {
                name: name_for_spawn.clone(),
                step_count,
                completed: Vec::new(),
                outputs: HashMap::new(),
            })),
        };
        let executor = WorkflowExecutor::new(workflow);
        let result = executor
            .run(std::sync::Arc::new(runner), Some(&cancel))
            .await;

        let (success, summary, error) = match result {
            Ok(s) => {
                let summary = build_final_summary(&name_for_spawn, &s);
                (true, summary, None)
            }
            Err(e) => {
                let msg = e.to_string();
                (false, String::new(), Some(msg.clone()))
            }
        };

        crate::send_or_warn!(
            completion_tx.send(PersonaResult {
                kind: crate::tui::commands::PersonaKind::Coder,
                task: format!("workflow {name_for_spawn}"),
                success,
                summary,
                error,
            }),
            "workflow completion channel receiver dropped"
        );
    });

    format!("🚀 Started workflow '{name}' ({step_count} steps).")
}

fn build_final_summary(name: &str, summary: &kf_workflow::WorkflowSummary) -> String {
    let mut lines = vec![format!("Workflow '{name}' complete:")];
    for step in summary.ordered_outputs(&ordered_names(summary)) {
        lines.push(format!(
            "  {} [{}] — {}",
            step.name, step.persona, step.summary
        ));
        if let Some(critique) = &step.critique {
            lines.push(format!("    critique: {critique}"));
        }
    }
    lines.join("\n")
}

fn ordered_names(summary: &kf_workflow::WorkflowSummary) -> Vec<String> {
    let mut names: Vec<String> = summary.outputs.keys().cloned().collect();
    names.sort();
    names
}

/// Step runner backed by the existing `task` tool spawner.
struct TuiStepRunner {
    model_name: String,
    ollama_host: String,
    config: crate::shared::SharedConfig,
    supports_images: bool,
    undo_stack: Option<crate::tools::UndoStackRef>,
    handle: Arc<Mutex<WorkflowHandle>>,
}

#[async_trait::async_trait]
impl StepRunner for TuiStepRunner {
    async fn run_step(&self, name: &str, prompt: &str, persona: &str) -> Result<String> {
        let spawner = Arc::new(crate::session::task_spawner::InProcessTaskSpawner::new(
            self.config.clone(),
            self.model_name.clone(),
            self.ollama_host.clone(),
            self.undo_stack.clone(),
            self.supports_images,
        ));
        let summary = spawner
            .run_task(crate::tools::task::TaskRequest {
                // WO 35.1: callers own the persona preamble — run_task is
                // verbatim now.
                prompt: crate::tools::task::build_task_prompt(persona, prompt),
                persona: persona.to_string(),
                model: None,
                max_turns: 1,
                cancel: None,
                owner: None,
            })
            .await
            .map_err(|e| anyhow::anyhow!("step {name} failed: {e}"))?;

        if let Ok(mut h) = self.handle.lock() {
            h.completed.push(name.to_string());
            h.outputs.insert(
                name.to_string(),
                StepOutput {
                    name: name.to_string(),
                    kind: kf_workflow::StepKind::Agent,
                    persona: persona.to_string(),
                    summary: summary.clone(),
                    critique: None,
                    structured_output: None,
                },
            );
        }

        Ok(summary)
    }
}

/// Step runner used by line-mode / non-interactive workflow runs.
pub struct LineStepRunner {
    pub model_name: String,
    pub ollama_host: String,
    pub config: crate::shared::SharedConfig,
    pub supports_images: bool,
    pub undo_stack: Option<crate::tools::UndoStackRef>,
}

#[async_trait::async_trait]
impl StepRunner for LineStepRunner {
    async fn run_step(&self, name: &str, prompt: &str, persona: &str) -> Result<String> {
        let spawner = Arc::new(crate::session::task_spawner::InProcessTaskSpawner::new(
            self.config.clone(),
            self.model_name.clone(),
            self.ollama_host.clone(),
            self.undo_stack.clone(),
            self.supports_images,
        ));
        spawner
            .run_task(crate::tools::task::TaskRequest {
                prompt: crate::tools::task::build_task_prompt(persona, prompt),
                persona: persona.to_string(),
                model: None,
                max_turns: 1,
                cancel: None,
                owner: None,
            })
            .await
            .map_err(|e| anyhow::anyhow!("step {name} failed: {e}"))
    }
}

/// Render a workflow summary for line-mode output.
pub fn format_summary(name: &str, summary: &kf_workflow::WorkflowSummary) -> String {
    let mut lines = vec![format!("Workflow '{name}' complete:")];
    let mut names: Vec<String> = summary.outputs.keys().cloned().collect();
    names.sort();
    for name in names {
        if let Some(out) = summary.outputs.get(&name) {
            lines.push(format!(
                "  {} [{}] — {}",
                out.name, out.persona, out.summary
            ));
            if let Some(critique) = &out.critique {
                lines.push(format!("    critique: {critique}"));
            }
        }
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shared::test_util::app_state;
    use crate::tui::app::AppState;
    use crate::tui::commands::PersonaResult;
    use tokio::sync::mpsc;

    fn empty_state() -> AppState {
        app_state()
    }

    #[test]
    fn status_when_idle_shows_usage() {
        let state = empty_state();
        let out = handle_status(&state);
        assert!(out.contains("No workflow"));
    }

    #[test]
    fn cancel_when_idle_returns_error() {
        let mut state = empty_state();
        let out = handle_cancel(&mut state);
        assert!(out.contains("No workflow"));
    }

    #[tokio::test]
    async fn run_missing_name_returns_usage() {
        let mut state = empty_state();
        let (tx, _rx) = mpsc::unbounded_channel::<PersonaResult>();
        let out = handle_run("", &mut state, tx).await;
        assert!(out.contains("Usage"));
    }

    #[tokio::test]
    async fn run_unknown_workflow_returns_not_found() {
        let mut state = empty_state();
        let (tx, _rx) = mpsc::unbounded_channel::<PersonaResult>();
        let out = handle_run("definitely_not_there_12345", &mut state, tx).await;
        assert!(out.contains("not found"));
    }

    #[tokio::test]
    async fn run_starts_workflow_and_sets_state() {
        let tmp = tempfile::tempdir().unwrap();
        let wf_dir = tmp.path().join(".kf-code/workflows");
        std::fs::create_dir_all(&wf_dir).unwrap();
        std::fs::write(
            wf_dir.join("smoke.json"),
            r#"{"name":"smoke","steps":[{"name":"a","prompt":"x","persona":"explore"}]}"#,
        )
        .unwrap();
        let cwd = std::env::current_dir().unwrap();
        std::env::set_current_dir(tmp.path()).unwrap();

        let mut state = empty_state();
        let (tx, _rx) = mpsc::unbounded_channel::<PersonaResult>();
        let out = handle_run("smoke", &mut state, tx).await;
        assert!(out.contains("Started workflow 'smoke'"));
        assert!(state.generation.workflow_in_progress.is_some());
        assert!(state.generation.workflow_cancel.is_some());

        std::env::set_current_dir(cwd).unwrap();
    }

    #[tokio::test]
    async fn run_parallel_flag_starts_parallel_mode() {
        let tmp = tempfile::tempdir().unwrap();
        let wf_dir = tmp.path().join(".kf-code/workflows");
        std::fs::create_dir_all(&wf_dir).unwrap();
        std::fs::write(
            wf_dir.join("par.json"),
            r#"{"name":"par","steps":[{"name":"a","prompt":"do the thing","persona":"explore"}]}"#,
        )
        .unwrap();
        let cwd = std::env::current_dir().unwrap();
        std::env::set_current_dir(tmp.path()).unwrap();

        let mut state = empty_state();
        let (tx, _rx) = mpsc::unbounded_channel::<PersonaResult>();
        let out = handle_run("par --parallel", &mut state, tx).await;
        // Parallel mode prints "scout/coder/reviewer" — distinct from the
        // sequential "(N steps)" message.
        assert!(
            out.contains("scout/coder/reviewer"),
            "parallel flag should trigger fan-out message, got: {out}"
        );
        assert!(state.generation.workflow_in_progress.is_some());

        std::env::set_current_dir(cwd).unwrap();
    }

    #[tokio::test]
    async fn run_parallel_flag_only_returns_usage_when_name_empty() {
        let mut state = empty_state();
        let (tx, _rx) = mpsc::unbounded_channel::<PersonaResult>();
        let out = handle_run("--parallel", &mut state, tx).await;
        assert!(out.contains("Usage"));
    }

    // WO 38.4 gap test: /workflow cancel on the parallel path must reach
    // the live orchestrator (previously a lying no-op — the flag was
    // never read by the parallel branch and cancel_all had zero
    // production callers).
    #[tokio::test]
    async fn parallel_workflow_cancel_reaches_orchestrator() {
        let tmp = tempfile::tempdir().unwrap();
        let wf_dir = tmp.path().join(".kf-code/workflows");
        std::fs::create_dir_all(&wf_dir).unwrap();
        std::fs::write(
            wf_dir.join("par.json"),
            r#"{"name":"par","steps":[{"name":"a","prompt":"do the thing","persona":"explore"}]}"#,
        )
        .unwrap();
        let cwd = std::env::current_dir().unwrap();
        std::env::set_current_dir(tmp.path()).unwrap();

        let mut state = empty_state();
        let (tx, _rx) = mpsc::unbounded_channel::<PersonaResult>();
        let out = handle_run("par --parallel", &mut state, tx).await;
        assert!(out.contains("scout/coder/reviewer"), "got: {out}");

        // Clone the Arc (not take) so the state slot stays populated for
        // handle_cancel to act on, while staying observable afterwards.
        let orch = state
            .generation
            .workflow_orchestrator
            .as_ref()
            .map(std::sync::Arc::clone)
            .expect("parallel run must store the orchestrator for cancel");
        assert!(!orch.cancel_requested());

        let out = handle_cancel(&mut state);
        assert!(out.contains("cancelled"), "got: {out}");
        assert!(
            orch.cancel_requested(),
            "cancel_all must arm the pipeline flag on the live orchestrator"
        );
        assert!(state.generation.workflow_orchestrator.is_none());
        assert!(state.generation.workflow_in_progress.is_none());

        std::env::set_current_dir(cwd).unwrap();
    }

    #[test]
    fn workflow_handle_status_line_percent() {
        let h = WorkflowHandle {
            name: "demo".into(),
            step_count: 4,
            completed: vec!["a".into(), "b".into()],
            outputs: HashMap::new(),
        };
        assert!(h.status_line().contains("50%"));
    }
}
