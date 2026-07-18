use crate::adapters;
use crate::session::conversation::ConversationLog;
use crate::session::executor::{ApprovalRequest, ApprovalResponse, Executor};
use crate::session::toolset::{CompositeToolset, VecToolset};
use crate::shared::{Config, Role, SharedConfig};
use crate::shared::{ToolDef, ToolError, ToolOutcome};
use crate::tools::{Tool, ToolContext, UndoStackRef};
use std::collections::HashMap;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;

/// Request to spawn a subagent task.
#[derive(Debug, Clone)]
pub struct TaskRequest {
    pub prompt: String,
    /// Restrict the subagent toolset: "explore", "plan", or "coder".
    pub persona: String,
}

/// Handle returned for a background task.
#[derive(Debug, Clone)]
pub struct TaskHandle {
    pub result: Option<String>,
    pub error: Option<String>,
}

/// Trait for an object that can spawn isolated subagent tasks.
#[async_trait::async_trait]
pub trait TaskSpawner: Send + Sync {
    /// Spawn and run a task synchronously, returning its summary.
    async fn run_task(&self, request: TaskRequest) -> Result<String, String>;
}

/// Per-session background task manager.
pub struct TaskManager {
    next_id: usize,
    tasks: HashMap<String, TaskHandle>,
}

impl TaskManager {
    pub fn new() -> Self {
        Self {
            next_id: 1,
            tasks: HashMap::new(),
        }
    }

    pub fn insert(&mut self, handle: TaskHandle) -> String {
        let id = format!("task-{}", self.next_id);
        self.next_id += 1;
        self.tasks.insert(id.clone(), handle);
        id
    }

    pub fn get(&self, id: &str) -> Option<&TaskHandle> {
        self.tasks.get(id)
    }
}

impl Default for TaskManager {
    fn default() -> Self {
        Self::new()
    }
}

/// Built-in `task` tool: run a prompt in an isolated subagent context and
/// return the final assistant summary.
pub struct Task {
    task_manager: Arc<Mutex<TaskManager>>,
}

impl Task {
    pub fn new() -> Self {
        Self {
            task_manager: Arc::new(Mutex::new(TaskManager::new())),
        }
    }

    pub fn with_manager(manager: Arc<Mutex<TaskManager>>) -> Self {
        Self {
            task_manager: manager,
        }
    }
}

impl Default for Task {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl Tool for Task {
    fn def(&self) -> ToolDef {
        ToolDef {
            name: "task",
            description: "Run a prompt through an isolated subagent with its own conversation and toolset. Use this for research, planning, or focused implementation that should not pollute the main thread. If background=true the task runs asynchronously and returns an id; retrieve the result with task_output.",
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "prompt": {
                        "type": "string",
                        "description": "Task description for the subagent"
                    },
                    "persona": {
                        "type": "string",
                        "enum": ["explore", "plan", "coder"],
                        "default": "coder",
                        "description": "Tool restriction persona for the subagent"
                    },
                    "background": {
                        "type": "boolean",
                        "default": false,
                        "description": "Run asynchronously and return a task id"
                    }
                },
                "required": ["prompt"]
            }),
        }
    }

    async fn run(&self, ctx: &ToolContext, args: serde_json::Value) -> ToolOutcome {
        let prompt = match args.get("prompt").and_then(|p| p.as_str()) {
            Some(p) if !p.trim().is_empty() => p.trim().to_string(),
            _ => {
                return ToolOutcome::Failure(ToolError::invalid_args(
                    "Missing or empty 'prompt' argument",
                ));
            }
        };

        let persona = args
            .get("persona")
            .and_then(|p| p.as_str())
            .unwrap_or("coder")
            .to_lowercase();
        let background = args
            .get("background")
            .and_then(|b| b.as_bool())
            .unwrap_or(false);

        let spawner = match &ctx.task_spawner {
            Some(s) => s.clone(),
            None => {
                return ToolOutcome::Error {
                    message: "task tool is not available in this context".to_string(),
                };
            }
        };

        if background {
            let manager = self.task_manager.clone();
            let request = TaskRequest {
                prompt: prompt.clone(),
                persona: persona.clone(),
            };
            let id = {
                let mut guard = manager.lock().unwrap_or_else(|e| e.into_inner());
                guard.insert(TaskHandle {
                    result: None,
                    error: None,
                })
            };
            let id_for_spawn = id.clone();
            tokio::spawn(async move {
                let result = spawner.run_task(request).await;
                let mut guard = manager.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(handle) = guard.tasks.get_mut(&id_for_spawn) {
                    match result {
                        Ok(summary) => handle.result = Some(summary),
                        Err(err) => handle.error = Some(err),
                    }
                }
            });
            ToolOutcome::Success {
                content: format!(
                    "Started background task {id}. Use task_output to retrieve the result."
                ),
            }
        } else {
            let request = TaskRequest { prompt, persona };
            match spawner.run_task(request).await {
                Ok(summary) => ToolOutcome::Success { content: summary },
                Err(err) => ToolOutcome::Error { message: err },
            }
        }
    }
}

/// `task_output` tool: retrieve the result of a background task spawned by `task`.
pub struct TaskOutput {
    task_manager: Arc<Mutex<TaskManager>>,
}

impl TaskOutput {
    pub fn new(task_manager: Arc<Mutex<TaskManager>>) -> Self {
        Self { task_manager }
    }
}

#[async_trait::async_trait]
impl Tool for TaskOutput {
    fn def(&self) -> ToolDef {
        ToolDef {
            name: "task_output",
            description: "Retrieve the result of a background task previously started with task(background=true).",
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "id": {
                        "type": "string",
                        "description": "Task id returned by task"
                    }
                },
                "required": ["id"]
            }),
        }
    }

    async fn run(&self, _ctx: &ToolContext, args: serde_json::Value) -> ToolOutcome {
        let id = match args.get("id").and_then(|i| i.as_str()) {
            Some(i) => i,
            None => {
                return ToolOutcome::Failure(ToolError::invalid_args("Missing 'id' argument"));
            }
        };

        let guard = self.task_manager.lock().unwrap_or_else(|e| e.into_inner());
        match guard.get(id) {
            Some(handle) if handle.result.is_some() => ToolOutcome::Success {
                content: handle.result.clone().unwrap_or_default(),
            },
            Some(handle) if handle.error.is_some() => ToolOutcome::Error {
                message: handle.error.clone().unwrap_or_default(),
            },
            Some(_) => ToolOutcome::Success {
                content: format!("Task {id} is still running."),
            },
            None => ToolOutcome::Failure(ToolError::invalid_args(format!("Unknown task id: {id}"))),
        }
    }
}

/// Spawn a subagent task inside an isolated `Executor` with a temporary
/// conversation log.
///
/// This reuses the persona tool restriction logic without requiring the
/// TUI's fork manager, so the `task` tool can run anywhere the executor
/// exists.
pub struct InProcessTaskSpawner {
    config: Config,
    model_name: String,
    ollama_host: String,
    undo_stack: Option<UndoStackRef>,
    supports_images: bool,
}

impl InProcessTaskSpawner {
    pub fn new(
        config: Config,
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
        }
    }
}

#[async_trait::async_trait]
impl TaskSpawner for InProcessTaskSpawner {
    async fn run_task(&self, request: TaskRequest) -> Result<String, String> {
        let adapter = adapters::caching::maybe_wrap_cached(
            adapters::adapter_for(
                &self.model_name,
                &self.ollama_host,
                None,
                self.config.request_timeout_secs,
            ),
            &self.config,
        );

        let (deny_list, path_guard, _read_gate) =
            crate::session::access::access_from_config(&self.config);
        let all = crate::tools::all_tools(
            self.undo_stack.clone(),
            self.supports_images,
            deny_list,
            path_guard,
            self.config.bash_sandbox_workdir,
            self.config.minify_write_side,
        );
        let tools: Vec<Arc<dyn Tool>> = match request.persona.as_str() {
            "explore" => all
                .into_iter()
                .filter(|t| {
                    matches!(
                        t.def().name,
                        "read_file"
                            | "read_image"
                            | "grep"
                            | "glob"
                            | "bash"
                            | "bash_status"
                            | "bash_cancel"
                            | "task"
                    )
                })
                .collect(),
            "plan" => all
                .into_iter()
                .filter(|t| {
                    matches!(
                        t.def().name,
                        "read_file" | "read_image" | "grep" | "glob" | "task"
                    )
                })
                .collect(),
            _ => all,
        };

        let temp_dir = std::env::temp_dir().join(format!(
            "kirkforge-task-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&temp_dir)
            .map_err(|e| format!("failed to create task temp dir: {e}"))?;
        let log_path = temp_dir.join("conversation.ndjson");

        let conversation = ConversationLog::open_async(log_path.clone())
            .await
            .map_err(|e| format!("failed to open task conversation log: {e}"))?
            .0;

        let shared_config: SharedConfig = Arc::new(std::sync::RwLock::new(self.config.clone()));
        let mut composite = CompositeToolset::empty();
        composite.add(Box::new(VecToolset::new("task", tools)));
        let mut executor = Executor::with_log_and_undo(
            adapter,
            composite,
            shared_config,
            conversation,
            None,
            self.undo_stack.clone(),
        );

        if request.persona == "explore" {
            executor.set_plan_mode(true);
        }

        let (approval_tx, mut approval_rx) = mpsc::unbounded_channel::<ApprovalRequest>();
        tokio::spawn(async move {
            while let Some(req) = approval_rx.recv().await {
                crate::send_or_warn!(
                    req.response.send(ApprovalResponse::Approved),
                    "task approval response receiver dropped"
                );
            }
        });

        let cancelled = Arc::new(AtomicBool::new(false));
        let prompt = build_task_prompt(&request.persona, &request.prompt);
        executor
            .run_turn_collecting(&prompt, &approval_tx, &cancelled)
            .await
            .map_err(|e| format!("task turn failed: {e}"))?;

        let summary = executor
            .conversation_log()
            .all()
            .iter()
            .rev()
            .find(|m| matches!(m.role, Role::Assistant) && !m.content.is_empty())
            .map(|m| m.content.clone())
            .unwrap_or_else(|| "(no assistant response produced)".to_string());

        let _ = std::fs::remove_dir_all(&temp_dir);
        Ok(summary)
    }
}

fn build_task_prompt(persona: &str, task: &str) -> String {
    match persona {
        "explore" => format!(
            "You are an exploratory research assistant. Read files, search, and gather context. \
             Do not edit files or run destructive commands. Produce a concise summary.\n\nTask: {task}"
        ),
        "plan" => format!(
            "You are a software architect. Explore with read-only tools only. \
             Design a step-by-step implementation plan and end with: \"## Plan Complete\".\n\nTask: {task}"
        ),
        _ => format!(
            "You are a focused implementation assistant with the full toolset. \
             Work efficiently in this isolated context and summarize what you changed and why.\n\nTask: {task}"
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::ToolContext;

    struct MockSpawner {
        result: Result<String, String>,
    }

    #[async_trait::async_trait]
    impl TaskSpawner for MockSpawner {
        async fn run_task(&self, _request: TaskRequest) -> Result<String, String> {
            self.result.clone()
        }
    }

    #[test]
    fn task_def_is_valid() {
        let tool = Task::new();
        let def = tool.def();
        assert_eq!(def.name, "task");
        assert!(def.parameters.get("properties").is_some());
    }

    #[test]
    fn task_manager_generates_unique_ids() {
        let mut mgr = TaskManager::new();
        let id1 = mgr.insert(TaskHandle {
            result: None,
            error: None,
        });
        let id2 = mgr.insert(TaskHandle {
            result: None,
            error: None,
        });
        assert_ne!(id1, id2);
        assert!(mgr.get(&id1).is_some());
    }

    #[tokio::test]
    async fn task_runs_synchronously_without_background() {
        let tool = Task::new();
        let spawner: Arc<dyn TaskSpawner> = Arc::new(MockSpawner {
            result: Ok("summary text".to_string()),
        });
        let ctx = ToolContext::with_spawner(spawner);
        let outcome = tool
            .run(&ctx, serde_json::json!({"prompt": "do thing"}))
            .await;
        assert!(
            matches!(outcome, ToolOutcome::Success { ref content } if content == "summary text"),
            "got {outcome:?}"
        );
    }

    #[tokio::test]
    async fn task_returns_error_when_spawner_unavailable() {
        let tool = Task::new();
        let ctx = ToolContext::new();
        let outcome = tool
            .run(&ctx, serde_json::json!({"prompt": "do thing"}))
            .await;
        assert!(
            matches!(outcome, ToolOutcome::Error { .. }),
            "got {outcome:?}"
        );
    }

    #[tokio::test]
    async fn task_output_retrieves_completed_result() {
        let manager = Arc::new(Mutex::new(TaskManager::new()));
        let id = {
            let mut mgr = manager.lock().unwrap_or_else(|e| e.into_inner());
            mgr.insert(TaskHandle {
                result: Some("done".to_string()),
                error: None,
            })
        };
        let tool = TaskOutput::new(manager);
        let ctx = ToolContext::new();
        let outcome = tool.run(&ctx, serde_json::json!({"id": id})).await;
        assert!(
            matches!(outcome, ToolOutcome::Success { ref content } if content == "done"),
            "got {outcome:?}"
        );
    }
}
