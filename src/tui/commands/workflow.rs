//! `/workflow` slash-command handler.
//!
//! Workflows are user-editable JSON DAGs defined in the `kirkforge-workflow`
//! crate. This module wires them into the TUI: loading, command dispatch,
//! rendering, and cancellation. Each step is executed via the existing
//! `task` tool's `InProcessTaskSpawner` through a thin `StepRunner`
//! implementation — we do NOT spawn parallel subagent orchestrators.

use crate::shared::read_shared_config;
use crate::tools::task::TaskSpawner;
use crate::tui::app::AppState;
use crate::tui::commands::PersonaResult;
use anyhow::Result;
use kirkforge_workflow::{StepOutput, StepRunner, Workflow, WorkflowExecutor};
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
///   `/workflow run <name>` — load and start a workflow.
///   `/workflow status`     — show step progress of the running workflow.
///   `/workflow cancel`     — abort the running workflow.
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
                "Usage: /workflow run <name> | status | cancel".into()
            } else {
                format!(
                    "Usage: /workflow run <name> | status | cancel\nGot: /workflow {sub} {rest}"
                )
            }
        }
    }
}

fn handle_status(state: &AppState) -> String {
    match &state.workflow_in_progress {
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
    if let Some(cancel) = state.workflow_cancel.take() {
        cancel.store(true, Ordering::SeqCst);
        state.workflow_in_progress = None;
        state.workflow_cancel = None;
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
    let name = name.to_string();
    if name.is_empty() {
        return "Usage: /workflow run <name>".into();
    }

    if state.workflow_in_progress.is_some() {
        return format!(
            "A workflow ('{}') is already running. /workflow status or /workflow cancel first.",
            state.workflow_in_progress.as_ref().unwrap().name
        );
    }

    let path = match kirkforge_workflow::find_workflow_file(&name) {
        Some(p) => p,
        None => {
            return format!(
                "Workflow '{name}' not found. Looked in .kirkforge/workflows/{name}.json and ~/.local/share/kirkforge/workflows/{name}.json"
            );
        }
    };

    let workflow = match Workflow::from_file(&path) {
        Ok(w) => w,
        Err(e) => return format!("Failed to load workflow '{name}': {e}"),
    };

    let cfg = read_shared_config(&state.config).clone();
    let model_name = state
        .model_info
        .as_ref()
        .map(|m| m.name.clone())
        .unwrap_or_else(|| cfg.default_model.clone());
    let ollama_host = cfg.ollama_host.clone();
    let supports_images = state
        .model_info
        .as_ref()
        .map(|m| m.supports_images)
        .unwrap_or(false);
    let undo_stack = state.undo_stack.clone();

    let step_count = workflow.steps.len();
    let handle = WorkflowHandle {
        name: name.to_string(),
        step_count,
        completed: Vec::new(),
        outputs: HashMap::new(),
    };
    state.workflow_in_progress = Some(handle);

    let cancel = Arc::new(AtomicBool::new(false));
    state.workflow_cancel = Some(cancel.clone());

    let name_for_spawn = name.clone();
    tokio::spawn(async move {
        let runner = TuiStepRunner {
            model_name,
            ollama_host,
            config: cfg,
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
        let result = executor.run(&runner, Some(&cancel)).await;

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
                fork_path: std::path::PathBuf::new(),
                success,
                summary,
                error,
            }),
            "workflow completion channel receiver dropped"
        );
    });

    format!("🚀 Started workflow '{name}' ({step_count} steps).")
}

fn build_final_summary(name: &str, summary: &kirkforge_workflow::WorkflowSummary) -> String {
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

fn ordered_names(summary: &kirkforge_workflow::WorkflowSummary) -> Vec<String> {
    let mut names: Vec<String> = summary.outputs.keys().cloned().collect();
    names.sort();
    names
}

/// Step runner backed by the existing `task` tool spawner.
struct TuiStepRunner {
    model_name: String,
    ollama_host: String,
    config: crate::shared::Config,
    supports_images: bool,
    undo_stack: Option<crate::tools::UndoStackRef>,
    handle: Arc<Mutex<WorkflowHandle>>,
}

#[async_trait::async_trait]
impl StepRunner for TuiStepRunner {
    async fn run_step(&self, name: &str, prompt: &str, persona: &str) -> Result<String> {
        let spawner = Arc::new(crate::tools::task::InProcessTaskSpawner::new(
            self.config.clone(),
            self.model_name.clone(),
            self.ollama_host.clone(),
            self.undo_stack.clone(),
            self.supports_images,
        ));
        let summary = spawner
            .run_task(crate::tools::task::TaskRequest {
                prompt: prompt.to_string(),
                persona: persona.to_string(),
            })
            .await
            .map_err(|e| anyhow::anyhow!("step {name} failed: {e}"))?;

        if let Ok(mut h) = self.handle.lock() {
            h.completed.push(name.to_string());
            h.outputs.insert(
                name.to_string(),
                StepOutput {
                    name: name.to_string(),
                    persona: persona.to_string(),
                    summary: summary.clone(),
                    critique: None,
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
    pub config: crate::shared::Config,
    pub supports_images: bool,
    pub undo_stack: Option<crate::tools::UndoStackRef>,
}

#[async_trait::async_trait]
impl StepRunner for LineStepRunner {
    async fn run_step(&self, name: &str, prompt: &str, persona: &str) -> Result<String> {
        let spawner = Arc::new(crate::tools::task::InProcessTaskSpawner::new(
            self.config.clone(),
            self.model_name.clone(),
            self.ollama_host.clone(),
            self.undo_stack.clone(),
            self.supports_images,
        ));
        spawner
            .run_task(crate::tools::task::TaskRequest {
                prompt: prompt.to_string(),
                persona: persona.to_string(),
            })
            .await
            .map_err(|e| anyhow::anyhow!("step {name} failed: {e}"))
    }
}

/// Render a workflow summary for line-mode output.
pub fn format_summary(name: &str, summary: &kirkforge_workflow::WorkflowSummary) -> String {
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
    use crate::shared::Config;
    use crate::tui::app::AppState;
    use crate::tui::commands::PersonaResult;
    use std::sync::{Arc, RwLock};
    use tokio::sync::mpsc;

    fn empty_state() -> AppState {
        AppState::new(Arc::new(RwLock::new(Config::default())))
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
        let wf_dir = tmp.path().join(".kirkforge/workflows");
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
        assert!(state.workflow_in_progress.is_some());
        assert!(state.workflow_cancel.is_some());

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
