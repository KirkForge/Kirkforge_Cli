//! `update_task` tool: let a subagent update its own or a peer's task
//! status or append a progress note.
//!
//! Wraps [`TaskManager`]'s `append_note`, `set_status`, `set_completed`,
//! and `set_failed` helpers. The `status` arg accepts the lowercase labels
//! from [`TaskStatus::label`]; "completed" and "failed" require the note
//! arg to carry the summary/message (so a subagent can't silently complete
//! a task with an empty payload). Mirrors Claude Code's `TaskUpdate`.

use crate::shared::{ToolDef, ToolError, ToolOutcome};
use crate::tools::task::TaskManager;
use crate::tools::{Tool, ToolContext};
use std::sync::{Arc, Mutex};

/// `update_task` tool: update a task's status or append a note.
pub struct UpdateTask {
    task_manager: Arc<Mutex<TaskManager>>,
}

impl UpdateTask {
    pub fn new(task_manager: Arc<Mutex<TaskManager>>) -> Self {
        Self { task_manager }
    }
}

#[async_trait::async_trait]
impl Tool for UpdateTask {
    fn def(&self) -> ToolDef {
        ToolDef {
            name: "update_task",
            description: "Update a subagent task by id: set its status (pending/running/completed/\
                          failed/cancelled) and/or append a progress note. A subagent uses this to \
                          mark its own task Done with a summary, record progress notes visible to \
                          list_agents, or escalate a peer's task to Failed. For status=\"completed\" \
                          or status=\"failed\", the note becomes the terminal summary/message.",
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "task_id": {
                        "type": "string",
                        "description": "The task id to update, as returned by the task tool."
                    },
                    "status": {
                        "type": "string",
                        "description": "Optional new status: pending, running, completed, failed, or cancelled. Omit to only append a note."
                    },
                    "note": {
                        "type": "string",
                        "description": "Optional progress note appended to the task's notes log, or — for status=completed/failed — the terminal summary/message."
                    }
                },
                "required": ["task_id"]
            }),
        }
    }

    async fn run(&self, _ctx: &ToolContext, args: serde_json::Value) -> ToolOutcome {
        let task_id = match args.get("task_id").and_then(|v| v.as_str()) {
            Some(s) => s,
            None => {
                return ToolOutcome::Failure(ToolError::invalid_args(
                    "Missing required 'task_id' argument",
                ));
            }
        };
        let status = args.get("status").and_then(|v| v.as_str());
        let note = args.get("note").and_then(|v| v.as_str());

        if status.is_none() && note.is_none() {
            return ToolOutcome::Failure(ToolError::invalid_args(
                "At least one of 'status' or 'note' must be provided",
            ));
        }

        let mut guard = self.task_manager.lock().unwrap_or_else(|e| e.into_inner());

        // First, the status update (may itself reject completed/failed with
        // an empty payload, or reject an unknown status string).
        if let Some(status) = status {
            let lower = status.to_lowercase();
            let outcome = match lower.as_str() {
                "completed" => {
                    let summary = note.unwrap_or("").trim();
                    if summary.is_empty() {
                        return ToolOutcome::Failure(ToolError::invalid_args(
                            "status=\"completed\" requires a non-empty 'note' to use as the summary",
                        ));
                    }
                    if guard.set_completed(task_id, summary) {
                        Ok(())
                    } else {
                        Err(format!("Unknown task id: {task_id}"))
                    }
                }
                "failed" => {
                    let message = note.unwrap_or("").trim();
                    if message.is_empty() {
                        return ToolOutcome::Failure(ToolError::invalid_args(
                            "status=\"failed\" requires a non-empty 'note' to use as the message",
                        ));
                    }
                    if guard.set_failed(task_id, message) {
                        Ok(())
                    } else {
                        Err(format!("Unknown task id: {task_id}"))
                    }
                }
                _ => guard.set_status(task_id, &lower).map_err(|e| e.to_string()),
            };
            if let Err(msg) = outcome {
                return ToolOutcome::Failure(ToolError::invalid_args(msg));
            }
            // For completed/failed the note was consumed as the summary;
            // don't also append it as a separate note.
            if matches!(lower.as_str(), "completed" | "failed") {
                return ToolOutcome::Success {
                    content: format!("Task {task_id} marked {lower}."),
                };
            }
        }

        // Then the note (separate from a completed/failed status, since
        // those consumed the note above).
        if let Some(note) = note {
            if !note.trim().is_empty() && !guard.append_note(task_id, note) {
                return ToolOutcome::Failure(ToolError::invalid_args(format!(
                    "Unknown task id: {task_id}"
                )));
            }
        }

        let label = status
            .map(|s| s.to_lowercase())
            .unwrap_or_else(|| "noted".to_string());
        ToolOutcome::Success {
            content: format!("Task {task_id} updated ({label})."),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::task::{TaskHandle, TaskMetadata, TaskStatus};
    use crate::tools::ToolContext;
    use std::sync::atomic::AtomicBool;

    fn mgr_with_running() -> (Arc<Mutex<TaskManager>>, String) {
        let manager = Arc::new(Mutex::new(TaskManager::new()));
        let id = {
            let mut mgr = manager.lock().unwrap();
            mgr.insert(TaskHandle {
                started: Arc::new(AtomicBool::new(true)),
                metadata: TaskMetadata {
                    persona: "coder".into(),
                    prompt_summary: "work".into(),
                    ..Default::default()
                },
                ..Default::default()
            })
        };
        (manager, id)
    }

    #[tokio::test]
    async fn update_task_appends_note_only() {
        let (manager, id) = mgr_with_running();
        let tool = UpdateTask::new(manager.clone());
        let outcome = tool
            .run(
                &ToolContext::new(),
                serde_json::json!({"task_id": id, "note": "halfway done"}),
            )
            .await;
        match outcome {
            ToolOutcome::Success { content } => {
                assert!(content.contains("noted"), "got: {content}");
            }
            other => panic!("expected Success, got {other:?}"),
        }
        let mgr = manager.lock().unwrap();
        let handle = mgr.get(&id).unwrap();
        assert_eq!(handle.notes, vec!["halfway done".to_string()]);
        // Status unchanged.
        assert_eq!(handle.status(), TaskStatus::Running);
    }

    #[tokio::test]
    async fn update_task_sets_running_status() {
        let manager = Arc::new(Mutex::new(TaskManager::new()));
        let id = {
            let mut mgr = manager.lock().unwrap();
            mgr.insert(TaskHandle::default())
        };
        let tool = UpdateTask::new(manager.clone());
        let outcome = tool
            .run(
                &ToolContext::new(),
                serde_json::json!({"task_id": id, "status": "running"}),
            )
            .await;
        assert!(matches!(outcome, ToolOutcome::Success { .. }));
        let mgr = manager.lock().unwrap();
        assert_eq!(mgr.get(&id).unwrap().status(), TaskStatus::Running);
    }

    #[tokio::test]
    async fn update_task_sets_cancelled_status() {
        let (manager, id) = mgr_with_running();
        let tool = UpdateTask::new(manager.clone());
        let outcome = tool
            .run(
                &ToolContext::new(),
                serde_json::json!({"task_id": id, "status": "cancelled"}),
            )
            .await;
        assert!(matches!(outcome, ToolOutcome::Success { .. }));
        let mgr = manager.lock().unwrap();
        assert_eq!(mgr.get(&id).unwrap().status(), TaskStatus::Cancelled);
    }

    #[tokio::test]
    async fn update_task_completed_requires_note_as_summary() {
        let (manager, id) = mgr_with_running();
        let tool = UpdateTask::new(manager.clone());
        // No note → rejected.
        let outcome = tool
            .run(
                &ToolContext::new(),
                serde_json::json!({"task_id": id, "status": "completed"}),
            )
            .await;
        assert!(matches!(
            outcome,
            ToolOutcome::Failure(ToolError::InvalidArgs { .. })
        ));
        // With note → summary set, note NOT appended as a separate note.
        let outcome = tool
            .run(
                &ToolContext::new(),
                serde_json::json!({"task_id": id, "status": "completed", "note": "shipped it"}),
            )
            .await;
        match outcome {
            ToolOutcome::Success { content } => {
                assert!(content.contains("completed"), "got: {content}");
            }
            other => panic!("expected Success, got {other:?}"),
        }
        let mgr = manager.lock().unwrap();
        let handle = mgr.get(&id).unwrap();
        assert_eq!(handle.status(), TaskStatus::Completed("shipped it".into()));
        assert!(
            handle.notes.is_empty(),
            "completed note must not double-append"
        );
    }

    #[tokio::test]
    async fn update_task_failed_requires_note_as_message() {
        let (manager, id) = mgr_with_running();
        let tool = UpdateTask::new(manager.clone());
        let outcome = tool
            .run(
                &ToolContext::new(),
                serde_json::json!({"task_id": id, "status": "failed", "note": "blew up"}),
            )
            .await;
        assert!(matches!(outcome, ToolOutcome::Success { .. }));
        let mgr = manager.lock().unwrap();
        let handle = mgr.get(&id).unwrap();
        assert_eq!(handle.status(), TaskStatus::Failed("blew up".into()));
        assert!(handle.notes.is_empty());
    }

    #[tokio::test]
    async fn update_task_rejects_unknown_id() {
        let manager = Arc::new(Mutex::new(TaskManager::new()));
        let tool = UpdateTask::new(manager);
        let outcome = tool
            .run(
                &ToolContext::new(),
                serde_json::json!({"task_id": "task-999", "note": "hi"}),
            )
            .await;
        match outcome {
            ToolOutcome::Failure(ToolError::InvalidArgs { message }) => {
                assert!(message.contains("Unknown task id"), "got: {message}");
            }
            other => panic!("expected InvalidArgs, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn update_task_rejects_bad_status_string() {
        let (manager, id) = mgr_with_running();
        let tool = UpdateTask::new(manager);
        let outcome = tool
            .run(
                &ToolContext::new(),
                serde_json::json!({"task_id": id, "status": "bogus"}),
            )
            .await;
        match outcome {
            ToolOutcome::Failure(ToolError::InvalidArgs { message }) => {
                assert!(message.contains("unknown status"), "got: {message}");
            }
            other => panic!("expected InvalidArgs, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn update_task_rejects_no_status_and_no_note() {
        let (manager, id) = mgr_with_running();
        let tool = UpdateTask::new(manager);
        let outcome = tool
            .run(&ToolContext::new(), serde_json::json!({"task_id": id}))
            .await;
        assert!(matches!(
            outcome,
            ToolOutcome::Failure(ToolError::InvalidArgs { .. })
        ));
    }

    #[tokio::test]
    async fn update_task_rejects_missing_task_id() {
        let manager = Arc::new(Mutex::new(TaskManager::new()));
        let tool = UpdateTask::new(manager);
        let outcome = tool
            .run(&ToolContext::new(), serde_json::json!({"note": "hi"}))
            .await;
        assert!(matches!(
            outcome,
            ToolOutcome::Failure(ToolError::InvalidArgs { .. })
        ));
    }

    #[tokio::test]
    async fn update_task_empty_note_is_ignored_not_appended() {
        let (manager, id) = mgr_with_running();
        let tool = UpdateTask::new(manager.clone());
        let outcome = tool
            .run(
                &ToolContext::new(),
                serde_json::json!({"task_id": id, "note": "   "}),
            )
            .await;
        // Empty note with no status → the "at least one of" gate passes
        // (note was provided), but the empty note is ignored. The success
        // message reports "noted" for backward compatibility with the
        // status=Some path; here status=None so it still reports "noted".
        assert!(matches!(outcome, ToolOutcome::Success { .. }));
        let mgr = manager.lock().unwrap();
        assert!(mgr.get(&id).unwrap().notes.is_empty());
    }

    #[test]
    fn update_task_def_name_and_required_task_id() {
        let manager = Arc::new(Mutex::new(TaskManager::new()));
        let tool = UpdateTask::new(manager);
        let def = tool.def();
        assert_eq!(def.name, "update_task");
        let required = def
            .parameters
            .get("required")
            .and_then(|r| r.as_array())
            .expect("required array");
        assert!(required.iter().any(|v| v.as_str() == Some("task_id")));
    }

    #[tokio::test]
    async fn update_task_status_and_note_both_for_non_terminal_status() {
        let (manager, id) = mgr_with_running();
        let tool = UpdateTask::new(manager.clone());
        // Set running + append a note in one call (running isn't a terminal
        // status, so the note is NOT consumed as a summary).
        let outcome = tool
            .run(
                &ToolContext::new(),
                serde_json::json!({"task_id": id, "status": "running", "note": "still going"}),
            )
            .await;
        assert!(matches!(outcome, ToolOutcome::Success { .. }));
        let mgr = manager.lock().unwrap();
        let handle = mgr.get(&id).unwrap();
        assert_eq!(handle.status(), TaskStatus::Running);
        assert_eq!(handle.notes, vec!["still going".to_string()]);
    }
}
