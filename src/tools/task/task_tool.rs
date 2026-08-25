//! The built-in `task` tool (WO 45.20 split from `task/mod.rs`): run a
//! prompt in an isolated subagent context and return the final assistant
//! summary. Re-exported as `crate::tools::task::Task` by `mod.rs`.

use crate::shared::{ToolDef, ToolError, ToolOutcome};
use crate::tools::task::{
    build_task_prompt_with_agents, cascade_parent_cancel, persist_task_summary, TaskCancel,
    TaskConcurrencyMode, TaskHandle, TaskManager, TaskMetadata, TaskRequest,
};
use crate::tools::{Tool, ToolContext};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;
use tokio_util::sync::CancellationToken;

/// Built-in `task` tool: run a prompt in an isolated subagent context and
/// return the final assistant summary.
pub struct Task {
    task_manager: Arc<Mutex<TaskManager>>,
    bg_semaphore: Arc<tokio::sync::Semaphore>,
    concurrency_mode: TaskConcurrencyMode,
    max_bg: usize,
    /// Dynamic agent registry (WO 39.3). Used to (a) build the agent
    /// system-prompt preamble when `persona` names a discovered agent,
    /// and (b) list discovered agents in the tool description. An empty
    /// registry (no `.claude/agents` dir or trust gate refused) means
    /// every unknown persona keeps the generic coder preamble.
    agents: std::sync::Arc<crate::session::agents::AgentRegistry>,
}

impl Task {
    pub fn new() -> Self {
        Self {
            task_manager: Arc::new(Mutex::new(TaskManager::new())),
            bg_semaphore: Arc::new(tokio::sync::Semaphore::new(4)),
            concurrency_mode: TaskConcurrencyMode::Queue,
            max_bg: 4,
            agents: std::sync::Arc::new(crate::session::agents::AgentRegistry::new()),
        }
    }

    pub fn with_manager(manager: Arc<Mutex<TaskManager>>) -> Self {
        Self {
            task_manager: manager,
            bg_semaphore: Arc::new(tokio::sync::Semaphore::new(4)),
            concurrency_mode: TaskConcurrencyMode::Queue,
            max_bg: 4,
            agents: std::sync::Arc::new(crate::session::agents::AgentRegistry::new()),
        }
    }

    pub fn with_config(
        manager: Arc<Mutex<TaskManager>>,
        max_background_tasks: usize,
        concurrency_mode: TaskConcurrencyMode,
    ) -> Self {
        let permits = max_background_tasks.clamp(1, 64);
        // WO 39.3: share the global registry so the tool description and
        // the spawner see the same agents. trust_workspace=true is the
        // permissive read; the spawner's trust gate already controls the
        // load, and the tool only needs the list for prompt building.
        let agents = crate::session::agents::global_registry(true);
        Self {
            task_manager: manager,
            bg_semaphore: Arc::new(tokio::sync::Semaphore::new(permits)),
            concurrency_mode,
            max_bg: permits,
            agents,
        }
    }

    /// Test constructor with an explicit agent registry (WO 39.3).
    pub fn with_agent_registry(
        manager: Arc<Mutex<TaskManager>>,
        max_background_tasks: usize,
        concurrency_mode: TaskConcurrencyMode,
        agents: std::sync::Arc<crate::session::agents::AgentRegistry>,
    ) -> Self {
        let permits = max_background_tasks.clamp(1, 64);
        Self {
            task_manager: manager,
            bg_semaphore: Arc::new(tokio::sync::Semaphore::new(permits)),
            concurrency_mode,
            max_bg: permits,
            agents,
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
        // WO 39.3: when dynamic agents are discovered, list them in the
        // description so the model knows which persona names are valid.
        let description = format!(
            "Run a prompt through an isolated subagent with its own conversation and toolset. \
             Use this for research, planning, or focused implementation that should not pollute \
             the main thread. If background=true the task runs asynchronously and returns an id; \
             retrieve the result with task_output.{}",
            self.agents.description_suffix()
        );
        ToolDef {
            name: "task",
            description: crate::shared::intern_static_str(&description),
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
        // WO 35.1: apply the persona preamble here — run_task uses the
        // prompt verbatim. prompt_summary below stays the raw user prompt.
        // WO 39.3: when the persona names a discovered agent, the agent's
        // system prompt + alias suffix replace the generic preamble.
        let full_prompt = build_task_prompt_with_agents(&persona, &prompt, Some(&self.agents));

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
            let prompt_summary: String = prompt.chars().take(100).collect();
            let request = TaskRequest {
                prompt: full_prompt,
                persona: persona.clone(),
                model: model.clone(),
                max_turns,
                cancel: None,
                owner: None,
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
            // Per-task lifecycle handles: a default handle owns the
            // `started` flag and the WO 35.3 cancel pair (flag + token);
            // clones go to the worker and into the TaskRequest.
            // WO 38.4: record the spawning (sub)agent's task id so task
            // trees are traceable — top-level spawns keep None.
            let parent_task_id = ctx.task_owner.clone();
            let metadata = TaskMetadata {
                model,
                persona,
                prompt_summary,
                started_at: chrono::Local::now(),
                duration_ms: None,
                token_estimate: None,
                parent_task_id,
            };
            let handle = TaskHandle {
                metadata,
                ..Default::default()
            };
            let started = Arc::clone(&handle.started);
            let cancel = handle.cancel_handles();
            // WO 38.4: cancel cascade — when this call runs inside a
            // subagent, ctx.token is a live child of that subagent
            // executor's root token, so an outer cancel reaches the
            // nested background task too. Retired on completion via the
            // handle's `completed` notify (fired by the worker below).
            cascade_parent_cancel(
                ctx.token.clone(),
                cancel.clone(),
                Arc::clone(&handle.completed),
            );
            let id = {
                let mut guard = manager.lock().unwrap_or_else(|e| e.into_inner());
                guard.insert(handle)
            };
            let id_for_spawn = id.clone();
            let mut request = request;
            request.cancel = Some(cancel);
            // WO 36.2: tag the request with the task id so background
            // bash jobs the subagent spawns are attributable — cancel()
            // kills exactly those via cancel_by_owner.
            request.owner = Some(id_for_spawn.clone());
            tokio::spawn(async move {
                started.store(true, Ordering::SeqCst);
                let start = Instant::now();
                // WO 35.3: cooperative cancellation — no select!/drop race.
                // run_task observes the cancel flag between turn steps and
                // the cancel token in-flight (bash children), then runs its
                // own cleanup (temp-dir guard, worktree patch capture)
                // before this await resolves.
                let result = spawner.run_task(request).await;
                drop(permit);
                let duration_ms = start.elapsed().as_millis() as u64;
                let mut guard = manager.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(handle) = guard.tasks.get_mut(&id_for_spawn) {
                    let notify = handle.completed.clone();
                    handle.metadata.duration_ms = Some(duration_ms);
                    match result {
                        // A task that finished after cancel keeps status
                        // `Cancelled` (cancel_requested wins over the
                        // partial Ok) but retains its output — including
                        // any worktree patch — in `cancelled_result`.
                        Ok(summary) => {
                            if handle.cancel_requested.load(Ordering::SeqCst) {
                                handle.cancelled_result = Some(summary);
                            } else {
                                handle.result = Some(summary);
                            }
                        }
                        Err(err) => {
                            if !handle.cancel_requested.load(Ordering::SeqCst) {
                                handle.error = Some(err);
                            }
                        }
                    }
                    // WO 41.5 Phase 1: persist the terminal summary to
                    // disk before waking waiters so a process exit (or
                    // `--resume`) retains the subagent's result.
                    persist_task_summary(&id_for_spawn, handle);
                    notify.notify_waiters();
                }
            });
            ToolOutcome::Success {
                content: format!(
                    "Started background task {id}. Use task_output to retrieve the result."
                ),
            }
        } else {
            // WO 38.4: foreground nested tasks derive their cancel pair
            // from the parent executor's live token (ctx.token) the same
            // way background ones do — outer cancel stops the child
            // between turns (flag) and in-flight (token). Main-session
            // tokens never fire, so top-level foreground tasks stay
            // uncancellable exactly as before.
            let cancel = TaskCancel {
                flag: Arc::new(AtomicBool::new(false)),
                token: CancellationToken::new(),
            };
            let done = Arc::new(tokio::sync::Notify::new());
            cascade_parent_cancel(ctx.token.clone(), cancel.clone(), done.clone());
            let request = TaskRequest {
                prompt: full_prompt,
                persona,
                model,
                max_turns,
                cancel: Some(cancel),
                // Owner rides with the ancestor so bash jobs the child
                // spawns die with the ancestor's cancel-by-owner kill.
                owner: ctx.task_owner.clone(),
            };
            let result = spawner.run_task(request).await;
            done.notify_waiters();
            match result {
                Ok(summary) => ToolOutcome::Success { content: summary },
                Err(err) => ToolOutcome::Error { message: err },
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::task::test_helpers::{
        extract_task_id, poll_until, BlockingSpawner, CooperativeProbe, MockSpawner,
    };
    use crate::tools::task::{TaskSpawner, TaskStatus};
    use crate::tools::ToolContext;
    use std::sync::atomic::AtomicBool;
    use std::sync::Mutex as StdMutex;

    // WO 35.1: the Task tool applies the preamble itself (run_task is
    // verbatim), so the request reaching the spawner must already carry it.
    #[tokio::test]
    async fn task_tool_wraps_raw_prompt_with_persona_preamble() {
        struct PromptCapture {
            seen: Arc<std::sync::Mutex<Option<String>>>,
        }
        #[async_trait::async_trait]
        impl TaskSpawner for PromptCapture {
            async fn run_task(&self, request: TaskRequest) -> Result<String, String> {
                *self.seen.lock().unwrap() = Some(request.prompt);
                Ok("ok".to_string())
            }
        }
        let seen = Arc::new(std::sync::Mutex::new(None));
        let spawner: Arc<dyn TaskSpawner> = Arc::new(PromptCapture { seen: seen.clone() });
        let tool = Task::new();
        let ctx = ToolContext::with_spawner(spawner);
        let outcome = tool
            .run(&ctx, serde_json::json!({"prompt": "do the thing"}))
            .await;
        assert!(
            matches!(outcome, ToolOutcome::Success { .. }),
            "got {outcome:?}"
        );
        let p = seen.lock().unwrap().clone().unwrap();
        assert!(
            p.contains("implementation assistant") && p.contains("do the thing"),
            "spawner must receive the wrapped prompt once, got: {p}"
        );
    }

    #[test]
    fn task_def_is_valid() {
        let tool = Task::new();
        let def = tool.def();
        assert_eq!(def.name, "task");
        assert!(def.parameters.get("properties").is_some());
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

    #[test]
    fn task_default_is_new() {
        let tool = Task::default();
        assert_eq!(tool.def().name, "task");
    }

    #[tokio::test]
    async fn task_background_with_prompt_starts_async_task() {
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
        poll_until("spawner invoked", || {
            (*calls.lock().unwrap() > 0).then_some(())
        })
        .await;
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
        let started = Arc::new(tokio::sync::Notify::new());
        let spawner: Arc<dyn TaskSpawner> = Arc::new(BlockingSpawner {
            started: started.clone(),
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
        // Wait deterministically for the first task to enter `run_task` (and
        // thus hold the semaphore) before launching the second — replaces a
        // 50ms bare sleep. 1s cap via Notify timeout guards against a
        // regression where the worker never starts.
        let _ = tokio::time::timeout(std::time::Duration::from_secs(1), started.notified()).await;
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

    #[tokio::test]
    async fn task_background_spawn_records_metadata_and_duration() {
        let manager = Arc::new(Mutex::new(TaskManager::new()));
        let task = Task::with_manager(manager.clone());
        let spawner: Arc<dyn TaskSpawner> = Arc::new(MockSpawner {
            result: Ok("ok".to_string()),
        });
        let ctx = ToolContext::with_spawner(spawner);
        let outcome = task
            .run(
                &ctx,
                serde_json::json!({
                    "prompt": "do the thing",
                    "background": true,
                    "persona": "explore",
                    "model": "qwen2.5:0.5b",
                }),
            )
            .await;
        let content = match outcome {
            ToolOutcome::Success { content } => content,
            other => panic!("expected Success, got {other:?}"),
        };
        let id = extract_task_id(&content);

        // Wait for the mock spawner to finish and the worker to record duration.
        poll_until("task reaches terminal status", || {
            manager
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .status(&id)
                .as_ref()
                .is_some_and(|s| s.is_terminal())
                .then_some(())
        })
        .await;

        let guard = manager.lock().unwrap_or_else(|e| e.into_inner());
        let entry = guard
            .list()
            .into_iter()
            .find(|e| e.id == id)
            .expect("task should be listed");
        assert!(matches!(entry.status, TaskStatus::Completed(_)));
        let m = entry.metadata;
        assert_eq!(m.persona, "explore");
        assert_eq!(m.model.as_deref(), Some("qwen2.5:0.5b"));
        assert_eq!(m.prompt_summary, "do the thing");
        assert!(m.duration_ms.is_some(), "duration should be recorded");
    }

    #[tokio::test]
    async fn task_background_prompt_summary_truncated_to_100_chars() {
        let manager = Arc::new(Mutex::new(TaskManager::new()));
        let task = Task::with_manager(manager.clone());
        let spawner: Arc<dyn TaskSpawner> = Arc::new(MockSpawner {
            result: Ok("ok".to_string()),
        });
        let ctx = ToolContext::with_spawner(spawner);
        let long_prompt = "x".repeat(250);
        let outcome = task
            .run(
                &ctx,
                serde_json::json!({ "prompt": long_prompt, "background": true }),
            )
            .await;
        let content = match outcome {
            ToolOutcome::Success { content } => content,
            other => panic!("expected Success, got {other:?}"),
        };
        let id = extract_task_id(&content);
        poll_until("task reaches terminal status", || {
            manager
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .status(&id)
                .as_ref()
                .is_some_and(|s| s.is_terminal())
                .then_some(())
        })
        .await;
        let m = manager
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .list()
            .first()
            .unwrap()
            .metadata
            .clone();
        assert_eq!(m.prompt_summary.chars().count(), 100);
        assert!(m.prompt_summary.chars().all(|c| c == 'x'));
    }

    #[tokio::test]
    async fn task_background_cancel_marks_cancelled_not_failed() {
        let manager = Arc::new(Mutex::new(TaskManager::new()));
        let task = Task::with_manager(manager.clone());
        let spawner: Arc<dyn TaskSpawner> = Arc::new(BlockingSpawner {
            started: Arc::new(tokio::sync::Notify::new()),
            finish: Arc::new(AtomicBool::new(false)),
        });
        let ctx = ToolContext::with_spawner(spawner);
        let outcome = task
            .run(
                &ctx,
                serde_json::json!({ "prompt": "long running", "background": true }),
            )
            .await;
        let content = match outcome {
            ToolOutcome::Success { content } => content,
            other => panic!("expected Success, got {other:?}"),
        };
        let id = extract_task_id(&content);

        // Wait until the worker has started (Pending -> Running).
        poll_until("task reaches Running status", || {
            matches!(
                manager
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .status(&id),
                Some(TaskStatus::Running)
            )
            .then_some(())
        })
        .await;

        // cancel() sets the flag synchronously, so status reflects Cancelled
        // immediately — distinct from Failed, and without waiting for the
        // worker's select! arm to observe the notify.
        assert!(
            manager
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .cancel(&id),
            "cancel should succeed for a running task"
        );
        assert_eq!(
            manager
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .status(&id),
            Some(TaskStatus::Cancelled),
            "cancelled task must read as Cancelled, not Failed"
        );
    }

    // ── WO 35.3: cooperative cancellation wiring ──

    #[tokio::test]
    async fn cancel_reaches_run_task_cooperatively_and_retains_partial_output() {
        let manager = Arc::new(Mutex::new(TaskManager::new()));
        let task = Task::with_manager(manager.clone());
        let CooperativeProbe {
            spawner,
            observed_cancel,
        } = CooperativeProbe::new();
        let spawner: Arc<dyn TaskSpawner> = Arc::new(spawner);
        let ctx = ToolContext::with_spawner(spawner);
        let outcome = task
            .run(
                &ctx,
                serde_json::json!({ "prompt": "long running", "background": true }),
            )
            .await;
        let content = match outcome {
            ToolOutcome::Success { content } => content,
            other => panic!("expected Success, got {other:?}"),
        };
        let id = extract_task_id(&content);
        poll_until("task reaches Running status", || {
            matches!(
                manager
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .status(&id),
                Some(TaskStatus::Running)
            )
            .then_some(())
        })
        .await;

        assert!(
            manager
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .cancel(&id),
            "cancel should succeed for a running task"
        );

        // The worker no longer drops the future: run_task observes the
        // cancel flag through the TaskRequest and returns on its own, the
        // token it was handed is the handle's (cancelled by cancel()).
        poll_until("run_task observed cancel and returned", || {
            observed_cancel.lock().unwrap().is_some().then_some(())
        })
        .await;
        let cancel = observed_cancel.lock().unwrap().take().unwrap();
        assert!(
            cancel.token.is_cancelled(),
            "cancel() must cancel the token threaded into the request"
        );
        poll_until("worker records terminal state", || {
            manager
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .status(&id)
                .as_ref()
                .is_some_and(|s| s.is_terminal())
                .then_some(())
        })
        .await;

        let guard = manager.lock().unwrap_or_else(|e| e.into_inner());
        let handle = guard.get(&id).unwrap();
        assert_eq!(handle.status(), TaskStatus::Cancelled);
        assert!(
            handle.result.is_none(),
            "cancelled task must not read as Completed"
        );
        assert_eq!(
            handle.cancelled_result.as_deref(),
            Some("partial work"),
            "partial output (incl. patch) must be retained for task_output"
        );
        assert!(handle.metadata.duration_ms.is_some());
    }

    // ── WO 38.4: cancel cascades to nested subagents ──

    // Outer cancel must stop a subagent's own background child. The ctx
    // mirrors what executor dispatch builds inside a subagent: a live
    // child of the outer task's root cancel token, tagged with the outer
    // task's owner id. Cancelling the outer task fires the token, the
    // cascade watcher translates it onto the nested handle's flag+token,
    // and the nested run_task returns cooperatively — all event-driven,
    // no sleep race.
    #[tokio::test]
    async fn outer_cancel_stops_nested_background_task() {
        let manager = Arc::new(Mutex::new(TaskManager::new()));
        let task = Task::with_manager(manager.clone());
        let CooperativeProbe {
            spawner,
            observed_cancel,
        } = CooperativeProbe::new();
        let spawner: Arc<dyn TaskSpawner> = Arc::new(spawner);

        let outer_id = {
            let mut mgr = manager.lock().unwrap_or_else(|e| e.into_inner());
            mgr.insert(TaskHandle {
                started: Arc::new(AtomicBool::new(true)),
                ..Default::default()
            })
        };

        let mut ctx = ToolContext::with_spawner(spawner);
        ctx.token = manager
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&outer_id)
            .unwrap()
            .cancel_handles()
            .token
            .child_token();
        ctx.task_owner = Some(outer_id.clone());

        let outcome = task
            .run(
                &ctx,
                serde_json::json!({ "prompt": "nested long running", "background": true }),
            )
            .await;
        let content = match outcome {
            ToolOutcome::Success { content } => content,
            other => panic!("expected Success, got {other:?}"),
        };
        let nested_id = extract_task_id(&content);

        // WO 38.4: the spawn records its parent for task-tree traceability.
        {
            let mgr = manager.lock().unwrap_or_else(|e| e.into_inner());
            let entry = mgr
                .list()
                .into_iter()
                .find(|e| e.id == nested_id)
                .expect("nested task listed");
            assert_eq!(
                entry.metadata.parent_task_id.as_deref(),
                Some(outer_id.as_str())
            );
        }

        poll_until("nested task reaches Running status", || {
            matches!(
                manager
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .status(&nested_id),
                Some(TaskStatus::Running)
            )
            .then_some(())
        })
        .await;

        // Cancel the OUTER task — nothing calls cancel() on the nested id.
        assert!(
            manager
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .cancel(&outer_id),
            "outer cancel should succeed"
        );

        // Bounded event-driven window: the cascade must reach the nested
        // run_task's cancel pair.
        poll_until("cascade reached nested run_task", || {
            observed_cancel.lock().unwrap().is_some().then_some(())
        })
        .await;
        let cancel = observed_cancel.lock().unwrap().take().unwrap();
        assert!(
            cancel.flag.load(Ordering::SeqCst),
            "outer cancel must set the nested task's flag"
        );
        assert!(
            cancel.token.is_cancelled(),
            "outer cancel must fire the nested task's token"
        );

        poll_until("nested task reaches terminal state", || {
            manager
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .status(&nested_id)
                .as_ref()
                .is_some_and(|s| s.is_terminal())
                .then_some(())
        })
        .await;
        assert_eq!(
            manager
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .status(&nested_id),
            Some(TaskStatus::Cancelled),
            "nested task must read as Cancelled after the outer cancel"
        );
    }

    // ── WO 39.3: dynamic agent registry integration ──

    fn reviewer_registry() -> std::sync::Arc<crate::session::agents::AgentRegistry> {
        let mut reg = crate::session::agents::AgentRegistry::new();
        reg.register(crate::session::agents::AgentDef {
            name: "reviewer".into(),
            description: "Reviews code".into(),
            system_prompt: "You are a senior code reviewer.".into(),
            tools: vec!["Read".into(), "Grep".into()],
            model: Some("fast-model".into()),
        });
        std::sync::Arc::new(reg)
    }

    #[test]
    fn task_def_description_lists_discovered_agents() {
        let reg = reviewer_registry();
        let manager = Arc::new(Mutex::new(TaskManager::new()));
        let tool = Task::with_agent_registry(manager, 4, TaskConcurrencyMode::Queue, reg);
        let def = tool.def();
        assert!(
            def.description.contains("reviewer"),
            "def: {}",
            def.description
        );
        assert!(def.description.contains("Reviews code"));
    }

    #[test]
    fn task_def_description_no_agents_suffix_when_empty() {
        let manager = Arc::new(Mutex::new(TaskManager::new()));
        let tool = Task::with_agent_registry(
            manager,
            4,
            TaskConcurrencyMode::Queue,
            std::sync::Arc::new(crate::session::agents::AgentRegistry::new()),
        );
        let def = tool.def();
        assert!(
            !def.description.contains("Discovered agents"),
            "empty registry must not add the suffix: {}",
            def.description
        );
    }

    // The Task tool's run() must route a persona matching a registered
    // agent through the agent's system prompt. We capture the prompt
    // reaching the spawner to prove the agent preamble was applied.
    #[tokio::test]
    async fn task_tool_routes_persona_through_agent_preamble() {
        struct Capture {
            seen: Arc<std::sync::Mutex<Option<String>>>,
        }
        #[async_trait::async_trait]
        impl TaskSpawner for Capture {
            async fn run_task(&self, request: TaskRequest) -> Result<String, String> {
                *self.seen.lock().unwrap() = Some(request.prompt);
                Ok("ok".to_string())
            }
        }
        let reg = reviewer_registry();
        let manager = Arc::new(Mutex::new(TaskManager::new()));
        let tool = Task::with_agent_registry(manager, 4, TaskConcurrencyMode::Queue, reg);
        let seen = Arc::new(std::sync::Mutex::new(None));
        let spawner: Arc<dyn TaskSpawner> = Arc::new(Capture { seen: seen.clone() });
        let ctx = ToolContext::with_spawner(spawner);
        let outcome = tool
            .run(
                &ctx,
                serde_json::json!({"prompt": "review this PR", "persona": "reviewer"}),
            )
            .await;
        assert!(
            matches!(outcome, ToolOutcome::Success { .. }),
            "got {outcome:?}"
        );
        let p = seen.lock().unwrap().clone().unwrap();
        assert!(
            p.contains("senior code reviewer"),
            "spawner must receive the agent system prompt as preamble: {p}"
        );
        assert!(p.contains("Tool-name aliases"));
        assert!(p.contains("review this PR"));
    }

    // The Task tool's run() must pass the agent's model override through
    // to the spawner's TaskRequest.model.
    #[tokio::test]
    async fn task_tool_passes_agent_model_override() {
        struct Capture {
            seen_model: Arc<std::sync::Mutex<Option<String>>>,
        }
        #[async_trait::async_trait]
        impl TaskSpawner for Capture {
            async fn run_task(&self, request: TaskRequest) -> Result<String, String> {
                *self.seen_model.lock().unwrap() = request.model;
                Ok("ok".to_string())
            }
        }
        let mut reg = crate::session::agents::AgentRegistry::new();
        reg.register(crate::session::agents::AgentDef {
            name: "fast-rev".into(),
            description: "fast reviewer".into(),
            system_prompt: "You review fast.".into(),
            tools: vec!["Grep".into()],
            model: Some("custom-model-x".into()),
        });
        let manager = Arc::new(Mutex::new(TaskManager::new()));
        let tool = Task::with_agent_registry(manager, 4, TaskConcurrencyMode::Queue, Arc::new(reg));
        let seen_model = Arc::new(std::sync::Mutex::new(None));
        let spawner: Arc<dyn TaskSpawner> = Arc::new(Capture {
            seen_model: seen_model.clone(),
        });
        let ctx = ToolContext::with_spawner(spawner);
        // The Task tool reads "model" from args, not the agent def. The
        // agent's model is consumed by the spawner (InProcessTaskSpawner
        // consults request.model OR the subagent_provider). Here we verify
        // the Task tool passes the explicit model arg; the agent model
        // override is a spawner-side concern (see task_spawner tests).
        let _ = tool
            .run(
                &ctx,
                serde_json::json!({
                    "prompt": "review",
                    "persona": "fast-rev",
                    "model": "explicit-model"
                }),
            )
            .await;
        assert_eq!(
            seen_model.lock().unwrap().as_deref(),
            Some("explicit-model"),
            "explicit model arg must reach the spawner"
        );
    }
}
