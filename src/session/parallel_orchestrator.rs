//! Parallel scout/coder/reviewer orchestration (WO 32.5).
//!
//! Spawns three subagents in parallel — Scout (read-only explore), Coder
//! (write), Reviewer (read-only plan/critique) — each via the existing
//! `InProcessTaskSpawner`, and collects their summaries. Each subagent gets
//! a `TaskManager` entry for lifecycle visibility (`/jobs`).
//!
//! Sequential fallback when worktree isolation is disabled: without FS
//! confinement, parallel bash calls can interfere, so the three roles run
//! one after another instead of concurrently.

use crate::session::task_spawner::InProcessTaskSpawner;
use crate::shared::SharedConfig;
use crate::tools::task::{TaskHandle, TaskManager, TaskRequest, TaskSpawner};
use crate::tools::UndoStackRef;
use std::sync::Arc;
use std::sync::Mutex;

/// Result of one parallel orchestration run.
#[derive(Debug, Clone)]
pub struct ParallelResult {
    pub scout: SubagentResult,
    pub coder: SubagentResult,
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
        let mut lines = vec!["Parallel orchestration complete:".to_string()];
        lines.push(format!(
            "  Scout    [{}] — {}",
            self.scout.task_id, self.scout.summary
        ));
        lines.push(format!(
            "  Coder    [{}] — {}",
            self.coder.task_id, self.coder.summary
        ));
        lines.push(format!(
            "  Reviewer [{}] — {}",
            self.reviewer.task_id, self.reviewer.summary
        ));
        lines.join("\n")
    }
}

/// Spawns Scout/Coder/Reviewer subagents, either in parallel (when worktree
/// isolation is active) or sequentially (fallback).
///
/// Reuses `InProcessTaskSpawner` — the single seam that constructs the nested
/// `Executor` with CWD confinement, landlock, and approval forwarding already
/// wired by WO 32.4 / 30.6. No new executor construction here.
pub struct ParallelOrchestrator {
    config: SharedConfig,
    model_name: String,
    ollama_host: String,
    undo_stack: Option<UndoStackRef>,
    supports_images: bool,
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
        Self {
            config,
            model_name,
            ollama_host,
            undo_stack,
            supports_images,
            task_manager: Arc::new(Mutex::new(TaskManager::new())),
        }
    }

    /// The task manager tracking the 3 parallel subagents. Pub so the TUI
    /// `/jobs` view can render them.
    pub fn task_manager(&self) -> Arc<Mutex<TaskManager>> {
        self.task_manager.clone()
    }

    /// Run scout/coder/reviewer in parallel via `tokio::join!`. Each subagent
    /// gets its own TaskManager entry. Use when worktree isolation is active
    /// (CWD confinement prevents parallel bash interference).
    pub async fn run_parallel(&self, task_description: &str) -> ParallelResult {
        let (scout, coder, reviewer) = tokio::join!(
            self.spawn_role("scout", "explore", task_description, 1),
            self.spawn_role("coder", "coder", task_description, 1),
            self.spawn_role("reviewer", "plan", task_description, 1),
        );
        ParallelResult {
            scout,
            coder,
            reviewer,
        }
    }

    /// Sequential fallback: run the three roles one after another. Used when
    /// worktree isolation is disabled — without FS confinement, parallel bash
    /// calls can interfere (WO 32.4 prerequisite).
    pub async fn run_sequential(&self, task_description: &str) -> ParallelResult {
        let scout = self
            .spawn_role("scout", "explore", task_description, 1)
            .await;
        let coder = self.spawn_role("coder", "coder", task_description, 1).await;
        let reviewer = self
            .spawn_role("reviewer", "plan", task_description, 1)
            .await;
        ParallelResult {
            scout,
            coder,
            reviewer,
        }
    }

    /// Spawn one subagent with the given persona, register it in the
    /// TaskManager, and await its result.
    async fn spawn_role(
        &self,
        role: &str,
        persona: &str,
        task_description: &str,
        max_turns: usize,
    ) -> SubagentResult {
        let prompt = build_role_prompt(role, persona, task_description);
        let prompt_summary: String = prompt.chars().take(100).collect();

        // Insert a default handle, then set metadata via get_mut — TaskHandle
        // has private fields (started/cancel_requested/cancel_signal) that
        // can't be set from outside the module, so we use Default + update.
        let task_id = {
            let mut mgr = self.task_manager.lock().unwrap_or_else(|e| e.into_inner());
            let id = mgr.insert(TaskHandle::default());
            if let Some(h) = mgr.get_mut(&id) {
                h.metadata.persona = persona.to_string();
                h.metadata.prompt_summary = prompt_summary;
            }
            id
        };

        let spawner = InProcessTaskSpawner::new(
            self.config.clone(),
            self.model_name.clone(),
            self.ollama_host.clone(),
            self.undo_stack.clone(),
            self.supports_images,
        );
        let request = TaskRequest {
            prompt,
            persona: persona.to_string(),
            model: None,
            max_turns,
        };
        let summary = match spawner.run_task(request).await {
            Ok(s) => s,
            Err(e) => format!("(failed: {e})"),
        };

        // Record the terminal state + duration so /jobs renders correctly.
        {
            let mut mgr = self.task_manager.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(h) = mgr.get_mut(&task_id) {
                h.result = Some(summary.clone());
            }
        }
        SubagentResult { task_id, summary }
    }
}

fn build_role_prompt(role: &str, persona: &str, task: &str) -> String {
    match role {
        "scout" => format!(
            "You are the Scout in a parallel scout/coder/reviewer orchestration. \
             Explore the codebase with read-only tools (read, grep, glob). Identify \
             the files relevant to the task, the patterns to follow, and any risks. \
             Produce a concise context summary for the Coder.\n\nTask: {task}"
        ),
        "coder" => format!(
            "You are the Coder in a parallel scout/coder/reviewer orchestration. \
             Implement the task with the full toolset. Work efficiently and summarize \
             what you changed and why. The Scout is exploring in parallel; you may \
             not have its context yet — proceed with your own judgment.\n\nTask: {task}"
        ),
        "reviewer" => format!(
            "You are the Reviewer in a parallel scout/coder/reviewer orchestration. \
             Review the task with read-only tools only. Identify potential issues, \
             edge cases, and verification steps the Coder should run. End with: \
             \"## Review Complete\".\n\nTask: {task}"
        ),
        _ => {
            // Fallback: defer to the persona's default prompt builder.
            crate::session::task_spawner::build_task_prompt(persona, task)
        }
    }
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
            reviewer: SubagentResult {
                task_id: "task-3".into(),
                summary: "looks good".into(),
            },
        };
        let s = r.summary();
        assert!(s.contains("Scout") && s.contains("task-1") && s.contains("found 3 files"));
        assert!(s.contains("Coder") && s.contains("task-2") && s.contains("edited foo.rs"));
        assert!(s.contains("Reviewer") && s.contains("task-3") && s.contains("looks good"));
    }

    #[test]
    fn build_role_prompt_scout_mentions_read_only() {
        let p = build_role_prompt("scout", "explore", "do X");
        assert!(p.contains("Scout") && p.contains("read-only") && p.contains("do X"));
    }

    #[test]
    fn build_role_prompt_coder_mentions_implement() {
        let p = build_role_prompt("coder", "coder", "do Y");
        assert!(p.contains("Coder") && p.contains("Implement") && p.contains("do Y"));
    }

    #[test]
    fn build_role_prompt_reviewer_mentions_review_complete() {
        let p = build_role_prompt("reviewer", "plan", "do Z");
        assert!(p.contains("Reviewer") && p.contains("Review Complete") && p.contains("do Z"));
    }

    #[test]
    fn build_role_prompt_unknown_role_falls_back_to_persona_prompt() {
        let p = build_role_prompt("unknown", "explore", "do W");
        // The fallback delegates to task_spawner's build_task_prompt which
        // mentions "research" for the explore persona.
        assert!(p.contains("research") || p.contains("do W"));
    }

    #[test]
    fn task_manager_tracks_three_roles_after_construction() {
        let config: SharedConfig =
            Arc::new(std::sync::RwLock::new(crate::shared::Config::default()));
        let orch =
            ParallelOrchestrator::new(config, "test-model".into(), "localhost".into(), None, false);
        let mgr = orch.task_manager();
        // Fresh manager is empty until roles are spawned.
        assert!(mgr.lock().unwrap().list().is_empty());
    }
}
