use crate::shared::{ToolDef, ToolError, ToolOutcome};
use crate::tools::{Tool, ToolContext};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;
use tokio_util::sync::CancellationToken;

/// Process-global task id counter shared by every TaskManager (WO 37.1):
/// the task tool, the orchestrator, and each subagent executor each own a
/// manager, so per-manager counters could mint colliding `task-N` owner
/// tags and a cancel reached another manager's jobs. One counter makes
/// collisions impossible by construction.
static NEXT_TASK_ID: AtomicU64 = AtomicU64::new(1);

/// Mint a process-unique number from the global counter (WO 38.4): the
/// same source that owns task ids, so filesystem identities derived from
/// it (temp dirs, worktree tags) can never collide on same-millisecond
/// spawns the way a pid+millis clock tag could.
pub(crate) fn next_unique_id() -> u64 {
    NEXT_TASK_ID.fetch_add(1, Ordering::SeqCst)
}

/// Link a parent executor's live cancel token to a nested task's cancel
/// pair (WO 38.4). Inside a subagent executor, `ctx.token` is a live
/// child of that executor's attached root token — when the outer task is
/// cancelled the child fires, and this watcher translates that into the
/// nested handle's flag + token so the child stops cooperatively instead
/// of outliving its parent. The second select arm retires the watcher
/// when the nested task finishes normally, so watchers don't pile up.
fn cascade_parent_cancel(
    parent: CancellationToken,
    child: TaskCancel,
    done: Arc<tokio::sync::Notify>,
) {
    tokio::spawn(async move {
        tokio::select! {
            _ = parent.cancelled() => {
                child.flag.store(true, Ordering::SeqCst);
                child.token.cancel();
            }
            _ = done.notified() => {}
        }
    });
}

/// Request to spawn a subagent task.
#[derive(Debug, Clone)]
pub struct TaskRequest {
    pub prompt: String,
    pub persona: String,
    pub model: Option<String>,
    pub max_turns: usize,
    /// Cooperative cancel handles (WO 35.3). The flag drives the
    /// subagent's turn loop (the executor's existing `AtomicBool`
    /// machinery); the token kills in-flight tool work (bash process
    /// groups) instead of waiting out `tool_timeout_secs`. `None` =
    /// uncancellable (foreground `task` calls, workflow steps).
    pub cancel: Option<TaskCancel>,
    /// Owning task id (WO 36.2). Set by callers that register the task in
    /// a `TaskManager` (background `task`, orchestrator roles) so the
    /// subagent's background bash jobs are tagged with it and die on
    /// cancel. `None` = uncancellable callers (foreground, workflows).
    pub owner: Option<String>,
}

/// The two cooperative-cancel primitives a running subagent observes,
/// sourced from one [`TaskHandle`].
#[derive(Debug, Clone)]
pub struct TaskCancel {
    pub flag: Arc<AtomicBool>,
    pub token: CancellationToken,
}

/// Lifecycle state of a background subagent task.
///
/// `Pending` = inserted but the worker has not entered `run_task` yet;
/// `Running` = `run_task` is in flight. `TimedOut` is defined for API
/// completeness — no per-task timeout is wired into `run_task` yet, so the
/// current code paths never produce it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TaskStatus {
    Pending,
    Running,
    Completed(String),
    Cancelled,
    Failed(String),
    TimedOut,
}

impl TaskStatus {
    /// One-word status label for compact display (TUI `/jobs`, logs).
    pub fn label(&self) -> &'static str {
        match self {
            TaskStatus::Pending => "pending",
            TaskStatus::Running => "running",
            TaskStatus::Completed(_) => "completed",
            TaskStatus::Cancelled => "cancelled",
            TaskStatus::Failed(_) => "failed",
            TaskStatus::TimedOut => "timed out",
        }
    }

    /// True once the task can no longer change state.
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            TaskStatus::Completed(_)
                | TaskStatus::Cancelled
                | TaskStatus::Failed(_)
                | TaskStatus::TimedOut
        )
    }
}

/// Per-task metadata recorded at spawn and updated on completion.
#[derive(Debug, Clone)]
pub struct TaskMetadata {
    pub model: Option<String>,
    pub persona: String,
    /// First 100 chars of the prompt — enough to identify a task in a list
    /// without dumping the whole (potentially long) prompt into the TUI.
    pub prompt_summary: String,
    pub started_at: chrono::DateTime<chrono::Local>,
    /// Wall-clock duration; `None` until the task reaches a terminal state.
    pub duration_ms: Option<u64>,
    /// Token usage, if the executor surfaces it. Currently always `None` —
    /// `TaskSpawner::run_task` returns only a summary string.
    pub token_estimate: Option<u64>,
    /// Parent task id for subagent trees. `None` for top-level tasks.
    pub parent_task_id: Option<String>,
}

impl Default for TaskMetadata {
    fn default() -> Self {
        Self {
            model: None,
            persona: String::new(),
            prompt_summary: String::new(),
            started_at: chrono::Local::now(),
            duration_ms: None,
            token_estimate: None,
            parent_task_id: None,
        }
    }
}

/// A row returned by [`TaskManager::list`] for TUI display.
#[derive(Debug, Clone)]
pub struct TaskListEntry {
    pub id: String,
    pub status: TaskStatus,
    pub metadata: TaskMetadata,
}

/// Format a [`TaskListEntry`] as a single display line for the TUI `/jobs`
/// view (mirrors `format_job_status` for bash jobs). Pub so the deferred
/// `/jobs` wiring can render subagent tasks alongside bash jobs.
pub fn format_task_entry(entry: &TaskListEntry) -> String {
    let icon = match &entry.status {
        TaskStatus::Pending => "⏳",
        TaskStatus::Running => "▶️",
        TaskStatus::Completed(_) => "✅",
        TaskStatus::Cancelled => "🚫",
        TaskStatus::Failed(_) => "❌",
        TaskStatus::TimedOut => "⏰",
    };
    let model = entry.metadata.model.as_deref().unwrap_or("default");
    let dur = entry
        .metadata
        .duration_ms
        .map(format_duration_ms)
        .unwrap_or_else(|| "—".to_string());
    format!(
        "{} {} {} [{}] persona={} {} — {}",
        icon,
        entry.id,
        entry.status.label(),
        model,
        entry.metadata.persona,
        dur,
        entry.metadata.prompt_summary,
    )
}

/// Compact `<n>ms` / `<n>.<dd>s` / `<n>m<dd>s` formatter for task durations.
fn format_duration_ms(ms: u64) -> String {
    if ms < 1_000 {
        format!("{ms}ms")
    } else if ms < 60_000 {
        format!("{:.2}s", ms as f64 / 1_000.0)
    } else {
        format!("{}m{:02}s", ms / 60_000, (ms % 60_000) / 1_000)
    }
}

/// Handle returned for a background task.
///
/// `result` / `error` remain the source of truth for completion/failure (so
/// the existing `task_output` tool is unchanged); [`TaskStatus`] is derived
/// from them plus the `started` / `cancel_requested` flags — no second copy
/// of completion state to drift. `cancelled_result` (WO 35.3) retains the
/// partial summary of a cooperatively-cancelled task — including any
/// worktree patch — without flipping the derived status off `Cancelled`.
#[derive(Debug, Clone)]
pub struct TaskHandle {
    pub result: Option<String>,
    pub error: Option<String>,
    /// Partial output of a cancelled task; `task_output` surfaces it.
    pub cancelled_result: Option<String>,
    pub completed: Arc<tokio::sync::Notify>,
    pub metadata: TaskMetadata,
    started: Arc<AtomicBool>,
    pub(crate) cancel_requested: Arc<AtomicBool>,
    cancel_signal: Arc<tokio::sync::Notify>,
    /// Cooperative-cancel token: cancelled together with the flag so
    /// in-flight tool work (bash children) dies promptly (WO 35.3).
    pub(crate) cancel_token: CancellationToken,
}

impl Default for TaskHandle {
    fn default() -> Self {
        Self {
            result: None,
            error: None,
            cancelled_result: None,
            completed: Arc::new(tokio::sync::Notify::new()),
            metadata: TaskMetadata::default(),
            started: Arc::new(AtomicBool::new(false)),
            cancel_requested: Arc::new(AtomicBool::new(false)),
            cancel_signal: Arc::new(tokio::sync::Notify::new()),
            cancel_token: CancellationToken::new(),
        }
    }
}

impl TaskHandle {
    /// The cancel handles to thread into a `TaskRequest` for this task.
    pub(crate) fn cancel_handles(&self) -> TaskCancel {
        TaskCancel {
            flag: Arc::clone(&self.cancel_requested),
            token: self.cancel_token.clone(),
        }
    }

    /// Derived lifecycle state. See [`TaskStatus`].
    pub fn status(&self) -> TaskStatus {
        if let Some(r) = &self.result {
            TaskStatus::Completed(r.clone())
        } else if let Some(e) = &self.error {
            TaskStatus::Failed(e.clone())
        } else if self.cancel_requested.load(Ordering::SeqCst) {
            TaskStatus::Cancelled
        } else if self.started.load(Ordering::SeqCst) {
            TaskStatus::Running
        } else {
            TaskStatus::Pending
        }
    }

    /// True if the task has reached a terminal state (no further updates).
    pub fn is_terminal(&self) -> bool {
        self.result.is_some()
            || self.error.is_some()
            || self.cancel_requested.load(Ordering::SeqCst)
    }
}

/// Trait for an object that can spawn isolated subagent tasks.
///
/// WO 35.1 contract: `request.prompt` is used verbatim as the subagent's
/// first-turn input — the spawner does NOT add a persona preamble. Callers
/// passing a raw user/workflow prompt apply [`build_task_prompt`] first;
/// callers with their own role prompt (the parallel orchestrator) pass it
/// as-is. One wrapper, never two.
#[async_trait::async_trait]
// ponytail: single impl, dyn dispatch for test injection; inline if MockSpawner ever removed
pub trait TaskSpawner: Send + Sync {
    /// Spawn and run a task synchronously, returning its summary.
    async fn run_task(&self, request: TaskRequest) -> Result<String, String>;
}

// Persona preamble wrapper for raw prompts. Born in tools::task (WO 28.1
// moved it to task_spawner when the spawner did the wrapping; WO 35.1 moved
// it back — callers own the preamble now, and tools must not reach into the
// session layer for it).
pub(crate) fn build_task_prompt(persona: &str, task: &str) -> String {
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

/// Per-session background task manager. Task ids come from the
/// process-global [`NEXT_TASK_ID`] counter, so ids (and the owner tags
/// derived from them) are unique across all managers (WO 37.1).
pub struct TaskManager {
    tasks: HashMap<String, TaskHandle>,
}

impl TaskManager {
    pub fn new() -> Self {
        Self {
            tasks: HashMap::new(),
        }
    }

    pub fn insert(&mut self, handle: TaskHandle) -> String {
        let id = format!("task-{}", NEXT_TASK_ID.fetch_add(1, Ordering::SeqCst));
        self.tasks.insert(id.clone(), handle);
        id
    }

    pub fn get(&self, id: &str) -> Option<&TaskHandle> {
        self.tasks.get(id)
    }

    /// Mutable handle lookup — needed by the parallel orchestrator (WO 32.5)
    /// to record terminal results on a handle inserted before the subagent
    /// completed. The `task` tool's background path doesn't need this because
    /// it inserts the handle and updates it inside the spawned worker closure,
    /// but `ParallelOrchestrator` inserts then awaits `run_task` directly.
    pub fn get_mut(&mut self, id: &str) -> Option<&mut TaskHandle> {
        self.tasks.get_mut(id)
    }

    /// Current lifecycle state of a task, or `None` if the id is unknown.
    pub fn status(&self, id: &str) -> Option<TaskStatus> {
        self.tasks.get(id).map(|h| h.status())
    }

    /// Request cooperative cancellation of a running task (WO 35.3): sets
    /// the per-task cancel flag AND cancels the task's token. The subagent
    /// turn loop observes the flag between steps, in-flight tool calls
    /// observe the token (a running bash's process group is killed within
    /// milliseconds instead of at `tool_timeout_secs`), and `run_task`
    /// runs its own cleanup (temp-dir guard, worktree patch capture)
    /// before returning. Returns `false` if the task is unknown or already
    /// terminal.
    ///
    /// WO 36.2: after the cooperative exit, background bash jobs the task
    /// spawned (tagged with this task's id at spawn) are killed via
    /// `BashJobRegistry::cancel_by_owner`. Main-session jobs (owner None)
    /// and other tasks' jobs are never touched. The kill is async (the
    /// registry locks are tokio Mutexes) while cancel() is sync, so it is
    /// fired as a detached task when a runtime is up; outside one (sync
    /// unit tests) the jobs stay manually cancellable via `bash_cancel`.
    ///
    /// ceiling: the in-flight *model stream* is not aborted mid-request —
    /// it ends at the next stream event or the adapter timeout.
    /// (WO 37.1 resolved the old owner-tag collision ceiling: ids are
    /// minted from a process-global counter, so a cancel reaches exactly
    /// this manager's jobs.)
    pub fn cancel(&self, id: &str) -> bool {
        let Some(handle) = self.tasks.get(id) else {
            return false;
        };
        if handle.is_terminal() {
            return false;
        }
        handle.cancel_requested.store(true, Ordering::SeqCst);
        handle.cancel_token.cancel();
        handle.cancel_signal.notify_one();
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            let registry = crate::session::bash_jobs::global_registry();
            let owner = id.to_string();
            runtime.spawn(async move {
                registry.cancel_by_owner(&owner).await;
            });
        }
        true
    }

    /// All tasks with their status + metadata, ordered by task id (numeric)
    /// so `task-2` precedes `task-10`, for the TUI `/jobs` display.
    pub fn list(&self) -> Vec<TaskListEntry> {
        let mut entries: Vec<TaskListEntry> = self
            .tasks
            .iter()
            .map(|(id, h)| TaskListEntry {
                id: id.clone(),
                status: h.status(),
                metadata: h.metadata.clone(),
            })
            .collect();
        entries.sort_by_key(|e| task_id_rank(&e.id));
        entries
    }
}

/// Numeric sort key for `task-<n>` ids so `task-2` sorts before `task-10`.
fn task_id_rank(id: &str) -> usize {
    id.strip_prefix("task-")
        .and_then(|s| s.parse().ok())
        .unwrap_or(usize::MAX)
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
        // WO 35.1: apply the persona preamble here — run_task uses the
        // prompt verbatim. prompt_summary below stays the raw user prompt.
        let full_prompt = build_task_prompt(&persona, &prompt);

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
                    notify.notify_one();
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
            done.notify_one();
            match result {
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
        guard.get(id).is_some_and(|h| h.is_terminal())
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
            // WO 35.3: a cancelled task is terminal — surface its retained
            // partial output (incl. worktree patch) instead of "running".
            Some(handle) if handle.cancel_requested.load(Ordering::SeqCst) => {
                ToolOutcome::Success {
                    content: handle
                        .cancelled_result
                        .clone()
                        .unwrap_or_else(|| format!("Task {id} was cancelled.")),
                }
            }
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
    use std::sync::atomic::AtomicBool;

    // WO 28.1: these followed the spawner to task_spawner; WO 35.1 moved
    // them back with build_task_prompt (callers own the preamble now).
    #[test]
    fn build_task_prompt_for_coder_persona_mentions_implementation() {
        let p = build_task_prompt("coder", "do X");
        assert!(p.contains("implementation") && p.contains("do X"));
    }

    #[test]
    fn build_task_prompt_for_explore_persona_mentions_research() {
        let p = build_task_prompt("explore", "explore Y");
        assert!(p.contains("research") && p.contains("explore Y"));
    }

    #[test]
    fn build_task_prompt_for_plan_persona_mentions_architect() {
        let p = build_task_prompt("plan", "plan Z");
        assert!(p.contains("architect") && p.contains("Plan Complete"));
    }

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

    // Poll a condition until it returns Some(T), with a bounded 1s total budget
    // and a 10ms interval. Replaces `for _ in 0..50 { sleep(20ms) }` loops.
    // Panics on timeout so a regression fails loudly instead of silently
    // advancing to a flaky assertion.
    async fn poll_until<T, F>(label: &str, mut cond: F) -> T
    where
        F: FnMut() -> Option<T>,
    {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
        loop {
            if let Some(v) = cond() {
                return v;
            }
            if std::time::Instant::now() >= deadline {
                panic!("{label}: condition never met within 1s");
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }

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
            // Block forever. No test sets `finish` today — the worker is
            // cancelled by drop/abort, not by this flag. Park cheaply instead
            // of a 10ms busy-wait sleep loop.
            // ponytail: finish flag kept for struct-construction parity; if a
            // future test needs graceful completion, swap to Notify-wait.
            let _ = &self.finish;
            std::future::pending::<()>().await;
            Ok("done".to_string())
        }
    }

    // WO 35.3: stands in for InProcessTaskSpawner to prove the wiring —
    // the worker must thread the handle's cancel pair into the TaskRequest
    // and await run_task to completion instead of dropping it.
    struct CooperativeSpawner {
        started: Arc<tokio::sync::Notify>,
        observed_cancel: Arc<std::sync::Mutex<Option<TaskCancel>>>,
    }

    #[async_trait::async_trait]
    impl TaskSpawner for CooperativeSpawner {
        async fn run_task(&self, request: TaskRequest) -> Result<String, String> {
            self.started.notify_one();
            let cancel = request
                .cancel
                .clone()
                .ok_or_else(|| "no cancel handle in request".to_string())?;
            // The cooperative shape: keep working until the flag fires,
            // then return (cleanup "ran" — observable via the flag).
            while !cancel.flag.load(Ordering::SeqCst) {
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
            *self.observed_cancel.lock().unwrap() = Some(cancel);
            Ok("partial work".to_string())
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
        let id1 = mgr.insert(TaskHandle::default());
        let id2 = mgr.insert(TaskHandle::default());
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
                ..Default::default()
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
            cancel: None,
            owner: None,
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
            cancel: None,
            owner: None,
        };
        assert!(req.model.is_none());
        assert!(
            req.cancel.is_none(),
            "foreground requests are uncancellable"
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
            mgr.insert(TaskHandle::default())
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
                error: Some("task blew up".to_string()),
                ..Default::default()
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
            ..Default::default()
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
            cancel: None,
            owner: None,
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

    // ── WO 30.2: TaskManager lifecycle (status / metadata / cancel / list) ──

    fn extract_task_id(content: &str) -> String {
        content
            .split_whitespace()
            .find(|w| w.starts_with("task-"))
            .map(|w| w.trim_end_matches('.'))
            .unwrap()
            .to_string()
    }

    #[test]
    fn task_handle_status_pending_by_default() {
        let h = TaskHandle::default();
        assert_eq!(h.status(), TaskStatus::Pending);
        assert!(!h.is_terminal());
    }

    #[test]
    fn task_handle_status_running_when_started() {
        let h = TaskHandle {
            started: Arc::new(AtomicBool::new(true)),
            ..Default::default()
        };
        assert_eq!(h.status(), TaskStatus::Running);
        assert!(!h.is_terminal());
    }

    #[test]
    fn task_handle_status_completed_carries_result() {
        let h = TaskHandle {
            result: Some("summary".to_string()),
            ..Default::default()
        };
        assert_eq!(h.status(), TaskStatus::Completed("summary".to_string()));
        assert!(h.is_terminal());
    }

    #[test]
    fn task_handle_status_failed_carries_error() {
        let h = TaskHandle {
            error: Some("boom".to_string()),
            ..Default::default()
        };
        assert_eq!(h.status(), TaskStatus::Failed("boom".to_string()));
        assert!(h.is_terminal());
    }

    #[test]
    fn task_handle_status_cancelled_when_requested_and_not_terminal() {
        // Cancel wins the race: no result/error, cancel flag set.
        let h = TaskHandle {
            cancel_requested: Arc::new(AtomicBool::new(true)),
            ..Default::default()
        };
        assert_eq!(h.status(), TaskStatus::Cancelled);
        assert!(h.is_terminal());
    }

    #[test]
    fn task_handle_status_prefers_result_over_cancel_race() {
        // If the task completed before cancel took effect, Completed wins.
        let h = TaskHandle {
            result: Some("done".to_string()),
            cancel_requested: Arc::new(AtomicBool::new(true)),
            ..Default::default()
        };
        assert_eq!(h.status(), TaskStatus::Completed("done".to_string()));
    }

    #[test]
    fn task_status_label_and_terminal_cover_all_variants() {
        assert!(!TaskStatus::Pending.is_terminal());
        assert!(!TaskStatus::Running.is_terminal());
        assert!(TaskStatus::Cancelled.is_terminal());
        assert!(TaskStatus::TimedOut.is_terminal());
        assert!(TaskStatus::Completed("x".into()).is_terminal());
        assert!(TaskStatus::Failed("e".into()).is_terminal());
        assert_eq!(TaskStatus::TimedOut.label(), "timed out");
        assert_eq!(TaskStatus::Running.label(), "running");
    }

    #[test]
    fn task_manager_status_unknown_id_is_none() {
        let mgr = TaskManager::new();
        assert!(mgr.status("task-99").is_none());
    }

    #[test]
    fn task_manager_cancel_unknown_id_returns_false() {
        let mgr = TaskManager::new();
        assert!(!mgr.cancel("task-99"));
    }

    #[test]
    fn task_manager_cancel_terminal_returns_false() {
        let mut mgr = TaskManager::new();
        let id = mgr.insert(TaskHandle {
            result: Some("done".to_string()),
            ..Default::default()
        });
        assert!(!mgr.cancel(&id));
    }

    #[test]
    fn task_manager_cancel_running_marks_cancelled() {
        let mut mgr = TaskManager::new();
        let id = mgr.insert(TaskHandle {
            started: Arc::new(AtomicBool::new(true)),
            ..Default::default()
        });
        assert!(mgr.cancel(&id));
        assert_eq!(mgr.status(&id), Some(TaskStatus::Cancelled));
        // Second cancel is a no-op (now terminal).
        assert!(!mgr.cancel(&id));
    }

    #[test]
    fn task_manager_list_sorted_by_numeric_id() {
        let mut mgr = TaskManager::new();
        for _ in 0..12 {
            mgr.insert(TaskHandle::default());
        }
        let entries = mgr.list();
        // Ids come from the process-global counter (WO 37.1), so the test
        // asserts numeric ordering by rank rather than absolute "task-N"
        // literals (other tests in the same process may have minted first).
        let ranks: Vec<usize> = entries.iter().map(|e| task_id_rank(&e.id)).collect();
        let mut sorted = ranks.clone();
        sorted.sort_unstable();
        assert_eq!(ranks, sorted, "list must be in numeric id order");
        // The lexicographic trap is pinned directly: task-2 < task-10.
        assert!(task_id_rank("task-2") < task_id_rank("task-10"));
    }

    // WO 37.1 gate (a): two managers minting in the same sequence produce
    // disjoint owner tags, so cancel_by_owner reaches only its own
    // manager's jobs — never the other manager's same-sequence-number id.
    #[tokio::test]
    async fn two_managers_mint_disjoint_owner_tags() {
        use crate::session::access::{DenyList, PathGuard};
        use crate::session::bash_jobs::{global_registry, JobStatus};

        let registry = global_registry();
        let mut mgr_a = TaskManager::new();
        let mut mgr_b = TaskManager::new();
        let id_a = mgr_a.insert(TaskHandle {
            started: Arc::new(AtomicBool::new(true)),
            ..Default::default()
        });
        let id_b = mgr_b.insert(TaskHandle {
            started: Arc::new(AtomicBool::new(true)),
            ..Default::default()
        });
        assert_ne!(id_a, id_b, "same-sequence ids from two managers collided");

        async fn spawn_sleep(
            registry: &crate::session::bash_jobs::BashJobRegistry,
            owner: &str,
        ) -> u64 {
            registry
                .spawn(
                    "sleep 30",
                    None,
                    None,
                    &DenyList::default(),
                    &PathGuard::default(),
                    false,
                    None,
                    Some(owner),
                )
                .await
                .unwrap()
        }
        let job_a = spawn_sleep(&registry, &id_a).await;
        let job_b = spawn_sleep(&registry, &id_b).await;

        assert_eq!(
            registry.cancel_by_owner(&id_a).await,
            1,
            "cancel reaches exactly manager A's job"
        );
        assert_eq!(
            registry.get(job_a).await.unwrap().status,
            JobStatus::Cancelled
        );
        assert_eq!(
            registry.get(job_b).await.unwrap().status,
            JobStatus::Running,
            "manager B's same-sequence job must survive A's cancel"
        );

        // Cleanup: kill the survivor so the test leaves no 30s child.
        registry.cancel(job_b).await;
    }

    #[test]
    fn format_duration_ms_thresholds() {
        assert_eq!(format_duration_ms(0), "0ms");
        assert_eq!(format_duration_ms(999), "999ms");
        assert_eq!(format_duration_ms(1_000), "1.00s");
        assert_eq!(format_duration_ms(1_500), "1.50s");
        assert_eq!(format_duration_ms(60_000), "1m00s");
        assert_eq!(format_duration_ms(125_000), "2m05s");
    }

    #[test]
    fn format_task_entry_renders_icon_id_status_and_summary() {
        let entry = TaskListEntry {
            id: "task-3".to_string(),
            status: TaskStatus::Completed("ignored by display".to_string()),
            metadata: TaskMetadata {
                model: Some("qwen".to_string()),
                persona: "explore".to_string(),
                prompt_summary: "scan the repo".to_string(),
                duration_ms: Some(1_500),
                ..Default::default()
            },
        };
        let s = format_task_entry(&entry);
        assert!(s.contains("task-3"), "got: {s}");
        assert!(s.contains("completed"), "got: {s}");
        assert!(s.contains("[qwen]"), "got: {s}");
        assert!(s.contains("persona=explore"), "got: {s}");
        assert!(s.contains("1.50s"), "got: {s}");
        assert!(s.contains("scan the repo"), "got: {s}");
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

    #[tokio::test]
    async fn task_output_surfaces_cancelled_partial_result() {
        let manager = Arc::new(Mutex::new(TaskManager::new()));
        let id = {
            let mut mgr = manager.lock().unwrap_or_else(|e| e.into_inner());
            mgr.insert(TaskHandle {
                cancelled_result: Some("partial + patch".to_string()),
                ..Default::default()
            })
        };
        // Simulate cancel() having fired (flag set).
        {
            let mgr = manager.lock().unwrap_or_else(|e| e.into_inner());
            mgr.cancel(&id);
        }
        let tool = TaskOutput::new(manager);
        let outcome = tool
            .run(&ToolContext::new(), serde_json::json!({ "id": id }))
            .await;
        match outcome {
            ToolOutcome::Success { content } => assert_eq!(content, "partial + patch"),
            other => panic!("expected Success with retained output, got {other:?}"),
        }
    }

    #[test]
    fn task_cancel_cancels_handle_token() {
        let mut mgr = TaskManager::new();
        let id = mgr.insert(TaskHandle::default());
        let token = mgr.get(&id).unwrap().cancel_handles().token;
        assert!(mgr.cancel(&id));
        assert!(
            token.is_cancelled(),
            "cancel() must cancel the shared token"
        );
    }

    // WO 36.2: cancelling a task kills exactly the background bash jobs it
    // spawned (owner-tagged) and never main-session (owner-None) jobs.
    // Uses the global registry because cancel()'s kill path goes through
    // global_registry(); nextest isolates per-process, and no other test in
    // this binary spawns owner-tagged global-registry jobs.
    #[tokio::test]
    async fn task_cancel_kills_owned_bash_jobs_spares_main_session() {
        use crate::session::bash_jobs::JobStatus;

        async fn spawn_job(
            registry: &crate::session::bash_jobs::BashJobRegistry,
            owner: &str,
        ) -> u64 {
            registry
                .spawn(
                    "sleep 30",
                    None,
                    None,
                    &crate::session::access::DenyList::default(),
                    &crate::session::access::PathGuard::default(),
                    false,
                    None,
                    Some(owner),
                )
                .await
                .unwrap()
        }

        let registry = crate::session::bash_jobs::global_registry();
        let mut mgr = TaskManager::new();
        let id = mgr.insert(TaskHandle {
            started: Arc::new(AtomicBool::new(true)),
            ..Default::default()
        });

        let owned = spawn_job(&registry, &id).await;
        let other = spawn_job(&registry, "task-999").await;
        let main_job = registry
            .spawn(
                "sleep 30",
                None,
                None,
                &crate::session::access::DenyList::default(),
                &crate::session::access::PathGuard::default(),
                false,
                None,
                None,
            )
            .await
            .unwrap();

        assert!(mgr.cancel(&id), "cancel should succeed for a running task");

        // cancel() fires the registry kill as a detached task; poll (with
        // a bounded deadline, like the bash_jobs tests) until it lands.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while registry.get(owned).await.unwrap().status == JobStatus::Running {
            assert!(
                std::time::Instant::now() < deadline,
                "owned job was not cancelled within 5s"
            );
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert_eq!(
            registry.get(owned).await.unwrap().status,
            JobStatus::Cancelled
        );

        assert_eq!(
            registry.get(other).await.unwrap().status,
            JobStatus::Running,
            "job owned by a different task id must survive"
        );
        assert_eq!(
            registry.get(main_job).await.unwrap().status,
            JobStatus::Running,
            "main-session (owner-None) job must survive a task cancel"
        );

        // Cleanup: kill the survivors so the test leaves no 30s children.
        registry.cancel(other).await;
        registry.cancel(main_job).await;
    }

    struct CooperativeProbe {
        spawner: CooperativeSpawner,
        observed_cancel: Arc<std::sync::Mutex<Option<TaskCancel>>>,
    }

    impl CooperativeProbe {
        fn new() -> Self {
            let observed_cancel = Arc::new(std::sync::Mutex::new(None));
            Self {
                spawner: CooperativeSpawner {
                    started: Arc::new(tokio::sync::Notify::new()),
                    observed_cancel: observed_cancel.clone(),
                },
                observed_cancel,
            }
        }
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
}
