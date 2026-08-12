use crate::shared::{ToolDef, ToolError, ToolOutcome};
use crate::tools::{Tool, ToolContext};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// Request to spawn a subagent task.
#[derive(Debug, Clone)]
pub struct TaskRequest {
    pub prompt: String,
    pub persona: String,
    pub model: Option<String>,
    pub max_turns: usize,
}

/// Handle returned for a background task.
#[derive(Debug, Clone)]
pub struct TaskHandle {
    pub result: Option<String>,
    pub error: Option<String>,
    pub completed: Arc<tokio::sync::Notify>,
}

/// Trait for an object that can spawn isolated subagent tasks.
#[async_trait::async_trait]
// ponytail: single impl, dyn dispatch for test injection; inline if MockSpawner ever removed
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

/// Concurrency mode for background tasks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TaskConcurrencyMode {
    Queue,
    Reject,
}

impl std::fmt::Display for TaskConcurrencyMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TaskConcurrencyMode::Queue => write!(f, "queue"),
            TaskConcurrencyMode::Reject => write!(f, "reject"),
        }
    }
}

impl std::str::FromStr for TaskConcurrencyMode {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        match s.to_lowercase().as_str() {
            "queue" => Ok(TaskConcurrencyMode::Queue),
            "reject" => Ok(TaskConcurrencyMode::Reject),
            other => Err(format!(
                "invalid task_concurrency_mode '{other}', expected 'queue' or 'reject'"
            )),
        }
    }
}

/// Built-in `task` tool: run a prompt in an isolated subagent context and
/// return the final assistant summary.
pub struct Task {
    task_manager: Arc<Mutex<TaskManager>>,
    bg_semaphore: Arc<tokio::sync::Semaphore>,
    concurrency_mode: TaskConcurrencyMode,
    max_bg: usize,
}

impl Task {
    pub fn new() -> Self {
        Self {
            task_manager: Arc::new(Mutex::new(TaskManager::new())),
            bg_semaphore: Arc::new(tokio::sync::Semaphore::new(4)),
            concurrency_mode: TaskConcurrencyMode::Queue,
            max_bg: 4,
        }
    }

    pub fn with_manager(manager: Arc<Mutex<TaskManager>>) -> Self {
        Self {
            task_manager: manager,
            bg_semaphore: Arc::new(tokio::sync::Semaphore::new(4)),
            concurrency_mode: TaskConcurrencyMode::Queue,
            max_bg: 4,
        }
    }

    pub fn with_config(
        manager: Arc<Mutex<TaskManager>>,
        max_background_tasks: usize,
        concurrency_mode: TaskConcurrencyMode,
    ) -> Self {
        let permits = max_background_tasks.clamp(1, 64);
        Self {
            task_manager: manager,
            bg_semaphore: Arc::new(tokio::sync::Semaphore::new(permits)),
            concurrency_mode,
            max_bg: permits,
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
                    },
                    "model": {
                        "type": "string",
                        "description": "Model to use for this subagent (optional). If omitted, uses the parent session's model. Example: 'qwen2.5:0.5b' for a cheap read-only exploration, 'opencode/big-pickle' for a free subagent."
                    },
                    "max_turns": {
                        "type": "integer",
                        "default": 1,
                        "description": "Maximum number of model turns for this subagent (1 = single turn, higher = multi-turn dialog). The subagent loops until FinishReason::Stop or max_turns reached."
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
        let model = args
            .get("model")
            .and_then(|m| m.as_str())
            .map(|s| s.to_string());
        let background = args
            .get("background")
            .and_then(|b| b.as_bool())
            .unwrap_or(false);
        let max_turns = args
            .get("max_turns")
            .and_then(|m| m.as_u64())
            .map(|m| (m as usize).max(1))
            .unwrap_or(1);

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
                model: model.clone(),
                max_turns,
            };
            let max_bg = self.max_bg;
            let permit = match self.concurrency_mode {
                TaskConcurrencyMode::Reject => {
                    match self.bg_semaphore.clone().try_acquire_owned() {
                        Ok(p) => p,
                        Err(_) => {
                            return ToolOutcome::Failure(ToolError::invalid_args(
                            format!("Background task concurrency limit reached ({max_bg} running). Use `task_output` to check running tasks or increase `max_background_tasks`.")
                        ));
                        }
                    }
                }
                TaskConcurrencyMode::Queue => {
                    let sem = self.bg_semaphore.clone();
                    sem.acquire_owned()
                        .await
                        .unwrap_or_else(|_| panic!("bg_semaphore closed unexpectedly"))
                }
            };
            let id = {
                let mut guard = manager.lock().unwrap_or_else(|e| e.into_inner());
                let notify = Arc::new(tokio::sync::Notify::new());
                let id = guard.insert(TaskHandle {
                    result: None,
                    error: None,
                    completed: notify.clone(),
                });
                (id, notify)
            };
            let id = id.0;
            let id_for_spawn = id.clone();
            tokio::spawn(async move {
                let result = spawner.run_task(request).await;
                drop(permit);
                let mut guard = manager.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(handle) = guard.tasks.get_mut(&id_for_spawn) {
                    let notify = handle.completed.clone();
                    match result {
                        Ok(summary) => handle.result = Some(summary),
                        Err(err) => handle.error = Some(err),
                    }
                    notify.notify_one();
                }
            });
            ToolOutcome::Success {
                content: format!(
                    "Started background task {id}. Use task_output to retrieve the result."
                ),
            }
        } else {
            let request = TaskRequest {
                prompt,
                persona,
                model,
                max_turns,
            };
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

    pub fn is_completed(&self, id: &str) -> bool {
        let guard = self.task_manager.lock().unwrap_or_else(|e| e.into_inner());
        guard
            .get(id)
            .is_some_and(|h| h.result.is_some() || h.error.is_some())
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::ToolContext;
    use std::sync::atomic::{AtomicBool, Ordering};

    struct MockSpawner {
        result: Result<String, String>,
    }

    #[async_trait::async_trait]
    impl TaskSpawner for MockSpawner {
        async fn run_task(&self, _request: TaskRequest) -> Result<String, String> {
            self.result.clone()
        }
    }

    struct BlockingSpawner {
        started: Arc<tokio::sync::Notify>,
        finish: Arc<AtomicBool>,
    }

    #[async_trait::async_trait]
    impl TaskSpawner for BlockingSpawner {
        async fn run_task(&self, _request: TaskRequest) -> Result<String, String> {
            self.started.notify_one();
            while !self.finish.load(Ordering::Relaxed) {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
            Ok("done".to_string())
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
            completed: Arc::new(tokio::sync::Notify::new()),
        });
        let id2 = mgr.insert(TaskHandle {
            result: None,
            error: None,
            completed: Arc::new(tokio::sync::Notify::new()),
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
                completed: Arc::new(tokio::sync::Notify::new()),
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

    #[test]
    fn task_request_with_model() {
        let req = TaskRequest {
            prompt: "explore the codebase".to_string(),
            persona: "explorer".to_string(),
            model: Some("opencode/big-pickle".to_string()),
            max_turns: 1,
        };
        assert_eq!(req.model.as_deref(), Some("opencode/big-pickle"));
    }

    #[test]
    fn task_request_model_defaults_none() {
        let req = TaskRequest {
            prompt: "explore the codebase".to_string(),
            persona: "explorer".to_string(),
            model: None,
            max_turns: 1,
        };
        assert!(req.model.is_none());
    }

    #[tokio::test]
    async fn task_empty_prompt_is_invalid_args() {
        let tool = Task::new();
        let spawner: Arc<dyn TaskSpawner> = Arc::new(MockSpawner {
            result: Ok("ok".to_string()),
        });
        let ctx = ToolContext::with_spawner(spawner);
        let outcome = tool.run(&ctx, serde_json::json!({"prompt": "   "})).await;
        assert!(
            matches!(outcome, ToolOutcome::Failure(ToolError::InvalidArgs { .. })),
            "got {outcome:?}"
        );
    }

    #[tokio::test]
    async fn task_missing_prompt_is_invalid_args() {
        let tool = Task::new();
        let spawner: Arc<dyn TaskSpawner> = Arc::new(MockSpawner {
            result: Ok("ok".to_string()),
        });
        let ctx = ToolContext::with_spawner(spawner);
        let outcome = tool.run(&ctx, serde_json::json!({})).await;
        assert!(
            matches!(outcome, ToolOutcome::Failure(ToolError::InvalidArgs { .. })),
            "got {outcome:?}"
        );
    }

    #[tokio::test]
    async fn task_spawner_error_is_surfaced_as_error_outcome() {
        let tool = Task::new();
        let spawner: Arc<dyn TaskSpawner> = Arc::new(MockSpawner {
            result: Err("boom".to_string()),
        });
        let ctx = ToolContext::with_spawner(spawner);
        let outcome = tool
            .run(&ctx, serde_json::json!({"prompt": "do thing"}))
            .await;
        match outcome {
            ToolOutcome::Error { message } => assert_eq!(message, "boom"),
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn task_output_missing_id_is_invalid_args() {
        let manager = Arc::new(Mutex::new(TaskManager::new()));
        let tool = TaskOutput::new(manager);
        let outcome = tool.run(&ToolContext::new(), serde_json::json!({})).await;
        assert!(
            matches!(outcome, ToolOutcome::Failure(ToolError::InvalidArgs { .. })),
            "got {outcome:?}"
        );
    }

    #[tokio::test]
    async fn task_output_unknown_id_is_invalid_args() {
        let manager = Arc::new(Mutex::new(TaskManager::new()));
        let tool = TaskOutput::new(manager);
        let outcome = tool
            .run(&ToolContext::new(), serde_json::json!({"id": "nope"}))
            .await;
        match outcome {
            ToolOutcome::Failure(ToolError::InvalidArgs { message }) => {
                assert!(message.contains("Unknown task id"), "got {message}");
            }
            other => panic!("expected InvalidArgs, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn task_output_pending_task_reports_still_running() {
        let manager = Arc::new(Mutex::new(TaskManager::new()));
        let id = {
            let mut mgr = manager.lock().unwrap();
            mgr.insert(TaskHandle {
                result: None,
                error: None,
                completed: Arc::new(tokio::sync::Notify::new()),
            })
        };
        let tool = TaskOutput::new(manager);
        let outcome = tool
            .run(&ToolContext::new(), serde_json::json!({"id": id}))
            .await;
        match outcome {
            ToolOutcome::Success { content } => {
                assert!(content.contains("still running"), "got: {content}");
            }
            other => panic!("expected Success, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn task_output_failed_task_returns_error_outcome() {
        let manager = Arc::new(Mutex::new(TaskManager::new()));
        let id = {
            let mut mgr = manager.lock().unwrap();
            mgr.insert(TaskHandle {
                result: None,
                error: Some("task blew up".to_string()),
                completed: Arc::new(tokio::sync::Notify::new()),
            })
        };
        let tool = TaskOutput::new(manager);
        let outcome = tool
            .run(&ToolContext::new(), serde_json::json!({"id": id}))
            .await;
        match outcome {
            ToolOutcome::Error { message } => assert_eq!(message, "task blew up"),
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[test]
    fn task_manager_default_is_new() {
        let mut mgr = TaskManager::default();
        let id = mgr.insert(TaskHandle {
            result: Some("x".to_string()),
            error: None,
            completed: Arc::new(tokio::sync::Notify::new()),
        });
        assert!(id.starts_with("task-"));
    }

    #[test]
    fn task_default_is_new() {
        let tool = Task::default();
        assert_eq!(tool.def().name, "task");
    }

    #[test]
    fn task_request_debug_includes_fields() {
        let req = TaskRequest {
            prompt: "p".into(),
            persona: "coder".into(),
            model: Some("m".into()),
            max_turns: 3,
        };
        let s = format!("{req:?}");
        assert!(s.contains("coder") && s.contains("p") && s.contains("m"));
    }

    #[tokio::test]
    async fn task_background_with_prompt_starts_async_task() {
        use std::sync::Mutex as StdMutex;
        struct CountingSpawner {
            calls: Arc<StdMutex<usize>>,
        }
        #[async_trait::async_trait]
        impl TaskSpawner for CountingSpawner {
            async fn run_task(&self, _r: TaskRequest) -> Result<String, String> {
                *self.calls.lock().unwrap() += 1;
                Ok("done".to_string())
            }
        }
        let calls = Arc::new(StdMutex::new(0usize));
        let spawner: Arc<dyn TaskSpawner> = Arc::new(CountingSpawner {
            calls: calls.clone(),
        });
        let tool = Task::new();
        let ctx = ToolContext::with_spawner(spawner);
        let outcome = tool
            .run(
                &ctx,
                serde_json::json!({"prompt": "do thing", "background": true}),
            )
            .await;
        let content = match outcome {
            ToolOutcome::Success { content } => content,
            other => panic!("expected Success, got {other:?}"),
        };
        assert!(
            content.contains("Started background task"),
            "got: {content}"
        );
        for _ in 0..50 {
            if *calls.lock().unwrap() > 0 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert!(*calls.lock().unwrap() >= 1, "spawner was not invoked");
    }

    #[tokio::test]
    async fn task_persona_defaults_to_coder() {
        let tool = Task::new();
        let spawner: Arc<dyn TaskSpawner> = Arc::new(MockSpawner {
            result: Ok("ok".to_string()),
        });
        let ctx = ToolContext::with_spawner(spawner);
        let outcome = tool
            .run(
                &ctx,
                serde_json::json!({"prompt": "do thing", "persona": "PLAN"}),
            )
            .await;
        assert!(
            matches!(outcome, ToolOutcome::Success { .. }),
            "persona should be lowercased and accepted, got {outcome:?}"
        );
    }

    #[test]
    fn task_output_def_has_correct_name_and_required_id() {
        let manager = Arc::new(Mutex::new(TaskManager::new()));
        let tool = TaskOutput::new(manager);
        let def = tool.def();
        assert_eq!(def.name, "task_output");
        let required = def
            .parameters
            .get("required")
            .and_then(|r| r.as_array())
            .expect("required array");
        assert!(required.iter().any(|v| v.as_str() == Some("id")));
    }

    #[test]
    fn multi_turn_prompt_uses_continue_on_subsequent_turns() {
        let prompt = "do thing".to_string();
        for turn_num in 0..3 {
            let input = if turn_num == 0 {
                prompt.as_str()
            } else {
                "continue"
            };
            if turn_num == 0 {
                assert_eq!(input, "do thing");
            } else {
                assert_eq!(input, "continue");
            }
        }
    }

    #[test]
    fn task_concurrency_mode_from_str() {
        assert_eq!(
            "queue".parse::<TaskConcurrencyMode>(),
            Ok(TaskConcurrencyMode::Queue)
        );
        assert_eq!(
            "reject".parse::<TaskConcurrencyMode>(),
            Ok(TaskConcurrencyMode::Reject)
        );
        assert_eq!(
            "QUEUE".parse::<TaskConcurrencyMode>(),
            Ok(TaskConcurrencyMode::Queue)
        );
        assert_eq!(
            "Reject".parse::<TaskConcurrencyMode>(),
            Ok(TaskConcurrencyMode::Reject)
        );
        assert!("invalid".parse::<TaskConcurrencyMode>().is_err());
    }

    #[test]
    fn task_with_config_clamps_semaphore_size() {
        let manager = Arc::new(Mutex::new(TaskManager::new()));
        let task = Task::with_config(manager, 0, TaskConcurrencyMode::Queue);
        assert_eq!(task.max_bg, 1);
        let manager = Arc::new(Mutex::new(TaskManager::new()));
        let task = Task::with_config(manager, 100, TaskConcurrencyMode::Queue);
        assert_eq!(task.max_bg, 64);
        let manager = Arc::new(Mutex::new(TaskManager::new()));
        let task = Task::with_config(manager, 8, TaskConcurrencyMode::Reject);
        assert_eq!(task.max_bg, 8);
        assert_eq!(task.concurrency_mode, TaskConcurrencyMode::Reject);
    }

    #[tokio::test]
    async fn task_reject_mode_returns_failure_when_semaphore_full() {
        let manager = Arc::new(Mutex::new(TaskManager::new()));
        let task = Task::with_config(manager, 1, TaskConcurrencyMode::Reject);
        let spawner: Arc<dyn TaskSpawner> = Arc::new(BlockingSpawner {
            started: Arc::new(tokio::sync::Notify::new()),
            finish: Arc::new(AtomicBool::new(false)),
        });
        let ctx = ToolContext::with_spawner(spawner.clone());
        let outcome1 = task
            .run(
                &ctx,
                serde_json::json!({"prompt": "first", "background": true}),
            )
            .await;
        assert!(
            matches!(outcome1, ToolOutcome::Success { .. }),
            "first task should start: {outcome1:?}"
        );
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let outcome2 = task
            .run(
                &ctx,
                serde_json::json!({"prompt": "second", "background": true}),
            )
            .await;
        match outcome2 {
            ToolOutcome::Failure(ToolError::InvalidArgs { message }) => {
                assert!(
                    message.contains("concurrency limit"),
                    "expected concurrency limit message, got: {message}"
                );
            }
            other => panic!(
                "expected Failure(InvalidArgs) for second task in reject mode, got {other:?}"
            ),
        }
    }
}
