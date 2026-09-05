//! `list_agents` tool: enumerate subagent tasks.
//!
//! Reads from [`TaskManager`] and returns a formatted list of every task
//! (running and completed) with its task id, persona, status label, and a
//! prompt-summary excerpt. Mirrors Claude Code's `ListAgents` tool for
//! agent-team coordination: a subagent can discover its peers before
//! deciding whether to `send_message` or `update_task`.

use crate::shared::{ToolDef, ToolError, ToolOutcome};
use crate::tools::task::TaskManager;
use crate::tools::{Tool, ToolContext};
use std::sync::{Arc, Mutex};

/// `list_agents` tool: list all subagent tasks with their status.
pub struct ListAgents {
    task_manager: Arc<Mutex<TaskManager>>,
}

impl ListAgents {
    pub fn new(task_manager: Arc<Mutex<TaskManager>>) -> Self {
        Self { task_manager }
    }
}

#[async_trait::async_trait]
impl Tool for ListAgents {
    fn def(&self) -> ToolDef {
        ToolDef {
            name: "list_agents",
            description: "List all subagent tasks (running and completed) with their task id, \
                          persona, status (pending/running/completed/failed/cancelled), and a \
                          prompt summary excerpt. Pass status=\"running\" to filter to live \
                          subagents only. Use this to discover peers before send_message or to \
                          check on a delegated task.",
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "status": {
                        "type": "string",
                        "description": "Optional filter: one of pending, running, completed, failed, cancelled. Omit to list all."
                    }
                },
                "required": []
            }),
        }
    }

    async fn run(&self, _ctx: &ToolContext, args: serde_json::Value) -> ToolOutcome {
        let filter = args.get("status").and_then(|v| v.as_str());
        if let Some(f) = filter {
            let valid = ["pending", "running", "completed", "failed", "cancelled"];
            let lower = f.to_lowercase();
            if !valid.contains(&lower.as_str()) {
                return ToolOutcome::Failure(ToolError::invalid_args(format!(
                    "unknown status filter '{lower}', expected one of: \
                     pending/running/completed/failed/cancelled"
                )));
            }
        }
        let guard = self.task_manager.lock().unwrap_or_else(|e| e.into_inner());
        let entries = guard.list();
        let filtered: Vec<_> = match filter {
            Some(f) => {
                let lower = f.to_lowercase();
                entries
                    .into_iter()
                    .filter(|e| e.status.label() == lower)
                    .collect()
            }
            None => entries,
        };

        if filtered.is_empty() {
            return ToolOutcome::Success {
                content: "No subagent tasks.".to_string(),
            };
        }

        let mut lines = Vec::with_capacity(filtered.len());
        for e in &filtered {
            let notes_count = guard
                .get(&e.id)
                .map(|h| h.notes.lock().unwrap_or_else(|e| e.into_inner()).len())
                .unwrap_or(0);
            let notes_tag = if notes_count > 0 {
                format!(
                    " ({} note{})",
                    notes_count,
                    if notes_count == 1 { "" } else { "s" }
                )
            } else {
                String::new()
            };
            lines.push(format!(
                "{} | {} | {} | {}{}",
                e.id,
                e.metadata.persona,
                e.status.label(),
                excerpt(&e.metadata.prompt_summary, 60),
                notes_tag,
            ));
        }
        ToolOutcome::Success {
            content: lines.join("\n"),
        }
    }
}

fn excerpt(s: &str, max: usize) -> String {
    let s = s.trim();
    if s.chars().count() <= max {
        return s.to_string();
    }
    let truncated: String = s.chars().take(max).collect();
    format!("{truncated}…")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::task::{TaskHandle, TaskMetadata};
    use crate::tools::ToolContext;
    use std::sync::atomic::AtomicBool;

    fn mgr_with_tasks() -> Arc<Mutex<TaskManager>> {
        let manager = Arc::new(Mutex::new(TaskManager::new()));
        {
            let mut mgr = manager.lock().unwrap();
            mgr.insert(TaskHandle {
                started: Arc::new(AtomicBool::new(true)),
                metadata: TaskMetadata {
                    persona: "coder".into(),
                    prompt_summary: "implement the foo bar feature across three files".into(),
                    ..Default::default()
                },
                ..Default::default()
            });
            mgr.insert(TaskHandle {
                result: Some("done".into()),
                metadata: TaskMetadata {
                    persona: "explore".into(),
                    prompt_summary: "scan the repo for patterns".into(),
                    ..Default::default()
                },
                ..Default::default()
            });
        }
        manager
    }

    #[tokio::test]
    async fn list_agents_returns_all_tasks() {
        let manager = mgr_with_tasks();
        let tool = ListAgents::new(manager);
        let outcome = tool.run(&ToolContext::new(), serde_json::json!({})).await;
        match outcome {
            ToolOutcome::Success { content } => {
                assert!(content.contains("coder"), "got: {content}");
                assert!(content.contains("explore"), "got: {content}");
                assert!(content.contains("running"), "got: {content}");
                assert!(content.contains("completed"), "got: {content}");
            }
            other => panic!("expected Success, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn list_agents_filter_running_excludes_completed() {
        let manager = mgr_with_tasks();
        let tool = ListAgents::new(manager);
        let outcome = tool
            .run(
                &ToolContext::new(),
                serde_json::json!({"status": "running"}),
            )
            .await;
        match outcome {
            ToolOutcome::Success { content } => {
                assert!(content.contains("coder"), "got: {content}");
                assert!(content.contains("running"), "got: {content}");
                assert!(
                    !content.contains("explore"),
                    "completed task must be filtered out: {content}"
                );
            }
            other => panic!("expected Success, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn list_agents_filter_completed_excludes_running() {
        let manager = mgr_with_tasks();
        let tool = ListAgents::new(manager);
        let outcome = tool
            .run(
                &ToolContext::new(),
                serde_json::json!({"status": "completed"}),
            )
            .await;
        match outcome {
            ToolOutcome::Success { content } => {
                assert!(content.contains("explore"), "got: {content}");
                assert!(content.contains("completed"), "got: {content}");
                assert!(
                    !content.contains("coder"),
                    "running task must be filtered out: {content}"
                );
            }
            other => panic!("expected Success, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn list_agents_empty_manager_says_no_tasks() {
        let manager = Arc::new(Mutex::new(TaskManager::new()));
        let tool = ListAgents::new(manager);
        let outcome = tool.run(&ToolContext::new(), serde_json::json!({})).await;
        match outcome {
            ToolOutcome::Success { content } => {
                assert_eq!(content, "No subagent tasks.");
            }
            other => panic!("expected Success, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn list_agents_rejects_bad_status_filter() {
        let manager = mgr_with_tasks();
        let tool = ListAgents::new(manager);
        let outcome = tool
            .run(&ToolContext::new(), serde_json::json!({"status": "bogus"}))
            .await;
        match outcome {
            ToolOutcome::Failure(ToolError::InvalidArgs { message }) => {
                assert!(message.contains("unknown status filter"), "got: {message}");
            }
            other => panic!("expected InvalidArgs, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn list_agents_shows_notes_count_when_present() {
        let manager = Arc::new(Mutex::new(TaskManager::new()));
        let _id = {
            let mut mgr = manager.lock().unwrap();
            let id = mgr.insert(TaskHandle {
                started: Arc::new(AtomicBool::new(true)),
                metadata: TaskMetadata {
                    persona: "coder".into(),
                    prompt_summary: "work".into(),
                    ..Default::default()
                },
                ..Default::default()
            });
            mgr.append_note(&id, "halfway done");
            mgr.append_note(&id, "almost finished");
            id
        };
        let tool = ListAgents::new(manager);
        let outcome = tool.run(&ToolContext::new(), serde_json::json!({})).await;
        match outcome {
            ToolOutcome::Success { content } => {
                assert!(content.contains("2 notes"), "got: {content}");
            }
            other => panic!("expected Success, got {other:?}"),
        }
    }

    #[test]
    fn list_agents_def_name_and_no_required_args() {
        let manager = Arc::new(Mutex::new(TaskManager::new()));
        let tool = ListAgents::new(manager);
        let def = tool.def();
        assert_eq!(def.name, "list_agents");
        let required = def
            .parameters
            .get("required")
            .and_then(|r| r.as_array())
            .expect("required array");
        assert!(required.is_empty(), "no required args");
    }

    #[test]
    fn excerpt_truncates_long_strings() {
        let short = "hi there";
        assert_eq!(excerpt(short, 60), "hi there");
        let long = "a".repeat(80);
        let out = excerpt(&long, 60);
        assert!(out.ends_with('…'), "got: {out}");
        assert!(out.chars().count() == 61, "got: {out}");
    }

    #[test]
    fn excerpt_trims_whitespace_first() {
        assert_eq!(excerpt("  spaced  ", 60), "spaced");
    }
}
