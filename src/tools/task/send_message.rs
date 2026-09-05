//! `send_message` tool: inter-subagent messaging.
//!
//! Lets a subagent (or the main session) queue a message for a running
//! subagent by task id. The message is appended to the target
//! [`TaskHandle`]'s `pending_messages` Vec; the subagent's executor drains
//! and clears the Vec at the start of its next turn and prepends the joined
//! text to the turn input, so the message lands as a system-level context
//! addition without restructuring the conversation log.

use crate::shared::{ToolDef, ToolError, ToolOutcome};
use crate::tools::task::TaskManager;
use crate::tools::{Tool, ToolContext};
use std::sync::{Arc, Mutex};

/// `send_message` tool: queue a message for a running subagent.
pub struct SendMessage {
    task_manager: Arc<Mutex<TaskManager>>,
}

impl SendMessage {
    pub fn new(task_manager: Arc<Mutex<TaskManager>>) -> Self {
        Self { task_manager }
    }
}

#[async_trait::async_trait]
impl Tool for SendMessage {
    fn def(&self) -> ToolDef {
        ToolDef {
            name: "send_message",
            description: "Send a message to a running subagent by task id. The message is \
                          injected into the target subagent's next turn as additional context. \
                          Use this for inter-agent coordination: ask a subagent to switch focus, \
                          report partial results, or stop early.",
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "task_id": {
                        "type": "string",
                        "description": "The task id (e.g. \"task-3\") of the target subagent, as returned by the task tool."
                    },
                    "message": {
                        "type": "string",
                        "description": "The message body to deliver. Keep it concise — it is prepended to the target's next turn input."
                    }
                },
                "required": ["task_id", "message"]
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
        let message = match args.get("message").and_then(|v| v.as_str()) {
            Some(s) => s,
            None => {
                return ToolOutcome::Failure(ToolError::invalid_args(
                    "Missing required 'message' argument",
                ));
            }
        };
        if message.trim().is_empty() {
            return ToolOutcome::Failure(ToolError::invalid_args("'message' must not be empty"));
        }

        let mut guard = self.task_manager.lock().unwrap_or_else(|e| e.into_inner());
        if guard.send_message(task_id, message) {
            ToolOutcome::Success {
                content: format!("Message queued for {task_id}."),
            }
        } else {
            let exists = guard.get(task_id).is_some();
            if !exists {
                ToolOutcome::Failure(ToolError::invalid_args(format!(
                    "Unknown task id: {task_id}"
                )))
            } else {
                ToolOutcome::Error {
                    message: format!(
                        "Task {task_id} is no longer running (already completed, failed, or cancelled)."
                    ),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::task::TaskHandle;
    use crate::tools::ToolContext;

    fn mgr_with_running(task_id_out: &mut Option<String>) -> Arc<Mutex<TaskManager>> {
        let manager = Arc::new(Mutex::new(TaskManager::new()));
        let id = {
            let mut mgr = manager.lock().unwrap();
            mgr.insert(TaskHandle {
                started: Arc::new(std::sync::atomic::AtomicBool::new(true)),
                metadata: crate::tools::task::TaskMetadata {
                    persona: "coder".into(),
                    prompt_summary: "do work".into(),
                    ..Default::default()
                },
                ..Default::default()
            })
        };
        *task_id_out = Some(id);
        manager
    }

    #[tokio::test]
    async fn send_message_queues_on_running_task() {
        let mut id_holder = None;
        let manager = mgr_with_running(&mut id_holder);
        let id = id_holder.unwrap();
        let tool = SendMessage::new(manager.clone());
        let outcome = tool
            .run(
                &ToolContext::new(),
                serde_json::json!({"task_id": id, "message": "hello from main"}),
            )
            .await;
        match outcome {
            ToolOutcome::Success { content } => {
                assert!(content.contains(&id), "got: {content}");
            }
            other => panic!("expected Success, got {other:?}"),
        }
        let mgr = manager.lock().unwrap();
        let handle = mgr.get(&id).expect("task present");
        let msgs = handle.pending_messages.lock().unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0], "hello from main");
    }

    #[tokio::test]
    async fn send_message_rejects_unknown_id() {
        let manager = Arc::new(Mutex::new(TaskManager::new()));
        let tool = SendMessage::new(manager);
        let outcome = tool
            .run(
                &ToolContext::new(),
                serde_json::json!({"task_id": "task-999", "message": "hi"}),
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
    async fn send_message_rejects_terminal_task() {
        let manager = Arc::new(Mutex::new(TaskManager::new()));
        let id = {
            let mut mgr = manager.lock().unwrap();
            mgr.insert(TaskHandle {
                result: Some("done".into()),
                ..Default::default()
            })
        };
        let tool = SendMessage::new(manager.clone());
        let outcome = tool
            .run(
                &ToolContext::new(),
                serde_json::json!({"task_id": id, "message": "hi"}),
            )
            .await;
        match outcome {
            ToolOutcome::Error { message } => {
                assert!(message.contains("no longer running"), "got: {message}");
            }
            other => panic!("expected Error, got {other:?}"),
        }
        // Nothing was queued.
        let mgr = manager.lock().unwrap();
        assert!(mgr
            .get(&id)
            .unwrap()
            .pending_messages
            .lock()
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn send_message_rejects_empty_message() {
        let mut id_holder = None;
        let manager = mgr_with_running(&mut id_holder);
        let id = id_holder.unwrap();
        let tool = SendMessage::new(manager.clone());
        let outcome = tool
            .run(
                &ToolContext::new(),
                serde_json::json!({"task_id": id, "message": "   "}),
            )
            .await;
        match outcome {
            ToolOutcome::Failure(ToolError::InvalidArgs { message }) => {
                assert!(message.contains("empty"), "got: {message}");
            }
            other => panic!("expected InvalidArgs, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn send_message_rejects_missing_args() {
        let manager = Arc::new(Mutex::new(TaskManager::new()));
        let tool = SendMessage::new(manager);
        // Missing task_id.
        let outcome = tool
            .run(&ToolContext::new(), serde_json::json!({"message": "hi"}))
            .await;
        assert!(matches!(
            outcome,
            ToolOutcome::Failure(ToolError::InvalidArgs { .. })
        ));
        // Missing message.
        let outcome = tool
            .run(
                &ToolContext::new(),
                serde_json::json!({"task_id": "task-1"}),
            )
            .await;
        assert!(matches!(
            outcome,
            ToolOutcome::Failure(ToolError::InvalidArgs { .. })
        ));
    }

    #[test]
    fn send_message_def_name_and_required_args() {
        let manager = Arc::new(Mutex::new(TaskManager::new()));
        let tool = SendMessage::new(manager);
        let def = tool.def();
        assert_eq!(def.name, "send_message");
        let required = def
            .parameters
            .get("required")
            .and_then(|r| r.as_array())
            .expect("required array");
        assert!(required.iter().any(|v| v.as_str() == Some("task_id")));
        assert!(required.iter().any(|v| v.as_str() == Some("message")));
    }

    #[tokio::test]
    async fn send_message_appends_multiple_messages_in_order() {
        let mut id_holder = None;
        let manager = mgr_with_running(&mut id_holder);
        let id = id_holder.unwrap();
        let tool = SendMessage::new(manager.clone());
        tool.run(
            &ToolContext::new(),
            serde_json::json!({"task_id": id, "message": "first"}),
        )
        .await;
        tool.run(
            &ToolContext::new(),
            serde_json::json!({"task_id": id, "message": "second"}),
        )
        .await;
        let mgr = manager.lock().unwrap();
        let handle = mgr.get(&id).unwrap();
        let msgs = handle.pending_messages.lock().unwrap();
        assert_eq!(*msgs, vec!["first".to_string(), "second".to_string()]);
    }
}
