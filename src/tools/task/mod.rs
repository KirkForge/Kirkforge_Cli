use crate::shared::{ToolDef, ToolError, ToolOutcome};
use crate::tools::{Tool, ToolContext};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tokio_util::sync::CancellationToken;

// WO 45.20: the `task` module is split across sibling files. `persist`
// holds the durable-summary JSON layer (WO 41.5); `task_tool` holds the
// built-in `task` tool; `test_helpers` holds shared `#[cfg(test)]`
// spawner/poll helpers used by all three test modules. Everything is
// re-exported from here so consumers keep using `crate::tools::task::*`
// unchanged.
mod persist;
mod task_tool;
#[cfg(test)]
mod test_helpers;

// Inter-subagent messaging tools (send_message / list_agents / update_task).
// Each is a small struct holding the shared TaskManager Arc; they live in
// sibling files and are re-exported from here so consumers keep using
// `crate::tools::task::*` unchanged.
mod list_agents;
mod send_message;
mod update_task;

pub use list_agents::ListAgents;
pub(crate) use persist::persist_task_summary;
pub use persist::{load_persisted_tasks, PersistedTask};
pub use send_message::SendMessage;
pub use task_tool::Task;
pub use update_task::UpdateTask;

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
pub(crate) fn cascade_parent_cancel(
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
    /// Subagent nesting depth (0 = root session, 1 = first subagent).
    /// Set by the `task` tool from `ctx.subagent_depth + 1` so the
    /// spawner can thread it into the subagent executor.
    pub subagent_depth: usize,
    /// Inter-subagent message queue (WO 49 batch4): an `Arc<Mutex<Vec>>`
    /// shared with the spawning `TaskHandle` so `send_message` appends are
    /// visible to the spawner turn loop, which drains + clears it before
    /// each `run_turn_collecting` call and prepends the joined text to the
    /// turn input. `None` for callers that never receive messages
    /// (foreground tasks, workflow steps, orchestrator briefs).
    pub pending_messages: Option<Arc<Mutex<Vec<String>>>>,
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
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
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
    /// WO 45.1: AgentRun identity — the run_id of the parent session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_run_id: Option<String>,
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
            parent_run_id: None,
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
///
/// `pending_messages` (inter-subagent messaging): messages queued by the
/// `send_message` tool for the subagent's executor to drain at the start of
/// its next turn. Held in an `Arc<Mutex<..>>` so the `TaskManager`
/// (`send_message` appends from any task) and the spawner's turn loop
/// (drains + clears before each `run_turn_collecting`) share one queue —
/// the same pattern as the WO 35.3 cancel flag. `notes` (update_task tool)
/// is an append-only log a subagent can surface to itself or to
/// `list_agents` — distinct from `result` so a running task can record
/// progress without flipping terminal state.
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
    /// Inter-subagent messages queued by `send_message`; drained + injected
    /// into the subagent's next turn input by the spawner turn loop. Shared
    /// via `Arc<Mutex>` with the spawner so concurrent appends are visible.
    pub pending_messages: Arc<Mutex<Vec<String>>>,
    /// Append-only progress notes a subagent records via `update_task`;
    /// surfaced by `list_agents`. Distinct from `result` (terminal summary).
    /// WO 50.05 M4: `Arc<Mutex<..>>` so the handle stays thread-safe when
    /// cloned (the worker closure and `update_task` tool both mutate it).
    pub notes: Arc<Mutex<Vec<String>>>,
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
            pending_messages: Arc::new(Mutex::new(Vec::new())),
            notes: Arc::new(Mutex::new(Vec::new())),
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

    /// Drain + clear the `pending_messages` queue, returning the messages
    /// joined with blank-line separators. Called by the spawner turn loop
    /// before each `run_turn_collecting` so inter-subagent messages land
    /// as a system-level context addition prepended to the turn input.
    /// Returns an empty String when no messages are queued.
    pub fn drain_pending_messages(&self) -> String {
        let mut guard = self
            .pending_messages
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if guard.is_empty() {
            return String::new();
        }
        let joined = guard.join("\n\n");
        guard.clear();
        joined
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

// WO 39.3: registry-aware preamble. When the persona matches a dynamic
// agent (`.claude/agents/*.md`), the agent's system prompt + the Claude
// alias suffix replace the generic preamble. Falls back to
// [`build_task_prompt`] for built-in personas and unknown names with no
// agent. `registry` may be `None` (workflow/orchestrator callers that
// don't load agents) — equivalent to an empty registry.
pub(crate) fn build_task_prompt_with_agents(
    persona: &str,
    task: &str,
    registry: Option<&crate::session::agents::AgentRegistry>,
) -> String {
    if let Some(reg) = registry {
        if let Some(agent) = reg.get(persona) {
            return crate::session::agents::build_agent_prompt(agent, task);
        }
    }
    build_task_prompt(persona, task)
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
    /// but `PipelineOrchestrator` inserts then awaits `run_task` directly.
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

    /// Append a message to a task's `pending_messages` queue (inter-subagent
    /// messaging). Returns `false` if the task id is unknown or already
    /// terminal (a done/failed/cancelled task cannot receive messages — the
    /// spawner will never drain the queue). The spawner turn loop drains
    /// and clears the queue at the start of each turn.
    pub fn send_message(&mut self, id: &str, message: &str) -> bool {
        let Some(handle) = self.tasks.get_mut(id) else {
            return false;
        };
        if handle.is_terminal() {
            return false;
        }
        let mut guard = handle
            .pending_messages
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        guard.push(message.to_string());
        true
    }

    /// Append a progress note to a task's `notes` log (`update_task` tool).
    /// Returns `false` if the task id is unknown. Notes are append-only and
    /// surface in `list_agents`; they never flip terminal state.
    pub fn append_note(&mut self, id: &str, note: &str) -> bool {
        let Some(handle) = self.tasks.get_mut(id) else {
            return false;
        };
        let mut guard = handle.notes.lock().unwrap_or_else(|e| e.into_inner());
        guard.push(note.to_string());
        true
    }

    /// Force-set a task's terminal status (`update_task` tool). Returns
    /// `Err` if the task id is unknown or the status string is not one of
    /// the accepted labels. This is the one path that writes
    /// `result`/`error`/`cancel_requested` from outside the worker closure
    /// — used by a subagent to mark its own task Done/Failed before the
    /// loop exits, or to flip a Pending task to Running. The accepted
    /// status strings are the lowercase labels from [`TaskStatus::label`]
    /// ("pending" / "running" / "cancelled"). "completed" and "failed"
    /// are rejected here — use [`set_completed`] / [`set_failed`] so a
    /// summary/message payload is always supplied.
    ///
    /// WO 50.05 M2: transitions out of a terminal state are rejected so a
    /// `set_status("running")` from a peer's `update_task` can't resurrect
    /// a task the worker closure has already finalized (matches the
    /// `send_message` terminal guard at `:505-507`). The only legal
    /// transitions are Pending → Running, and the explicit `cancelled`
    /// arm (which is still allowed from non-terminal states only).
    pub fn set_status(&mut self, id: &str, status: &str) -> Result<(), String> {
        let Some(handle) = self.tasks.get_mut(id) else {
            return Err(format!("Unknown task id: {id}"));
        };
        if handle.is_terminal() {
            return Err(format!(
                "Task {id} is already in a terminal state; set_status cannot resurrect it"
            ));
        }
        match status.to_lowercase().as_str() {
            "pending" => {
                handle.result = None;
                handle.error = None;
                handle.cancel_requested.store(false, Ordering::SeqCst);
                handle.started.store(false, Ordering::SeqCst);
            }
            "running" => {
                handle.result = None;
                handle.error = None;
                handle.cancel_requested.store(false, Ordering::SeqCst);
                handle.started.store(true, Ordering::SeqCst);
            }
            "completed" => {
                return Err(
                    "use set_completed(id, summary) to mark a task completed with a summary"
                        .to_string(),
                );
            }
            "failed" => {
                return Err(
                    "use set_failed(id, message) to mark a task failed with a message".to_string(),
                );
            }
            "cancelled" => {
                handle.result = None;
                handle.error = None;
                handle.cancel_requested.store(true, Ordering::SeqCst);
            }
            other => {
                return Err(format!(
                    "unknown status '{other}', expected pending/running/completed/failed/cancelled"
                ));
            }
        }
        Ok(())
    }

    /// Mark a task completed with a summary (`update_task` tool,
    /// `status="completed"`). Returns `false` if the task id is unknown or
    /// the task is already terminal (WO 50.05 M1: idempotent — a worker
    /// closure that has already written `result`/`error` wins, and a
    /// second `set_completed` from a peer's `update_task` cannot clobber
    /// it).
    pub fn set_completed(&mut self, id: &str, summary: &str) -> bool {
        let Some(handle) = self.tasks.get_mut(id) else {
            return false;
        };
        if handle.is_terminal() {
            return false;
        }
        handle.result = Some(summary.to_string());
        handle.error = None;
        handle.cancel_requested.store(false, Ordering::SeqCst);
        handle.completed.notify_one();
        true
    }

    /// Mark a task failed with a message (`update_task` tool,
    /// `status="failed"`). Returns `false` if the task id is unknown or
    /// the task is already terminal (WO 50.05 M1: idempotent — same
    /// rationale as [`set_completed`]).
    pub fn set_failed(&mut self, id: &str, message: &str) -> bool {
        let Some(handle) = self.tasks.get_mut(id) else {
            return false;
        };
        if handle.is_terminal() {
            return false;
        }
        handle.error = Some(message.to_string());
        handle.result = None;
        handle.completed.notify_one();
        true
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

// ── WO 41.5: durable subagent summaries ───────────────────────────────
// On terminal state the worker closure persists a minimal summary of the
// TaskHandle to `<data_dir>/tasks/<id>.json` so `--resume` can show what
// subagents ran without the in-memory HashMap. Phase 1: write + read only;
// no live handle is rehydrated from disk. (Moved to `persist.rs` in WO 45.20.)

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

// Built-in `task` tool: run a prompt in an isolated subagent context and
// return the final assistant summary. (Moved to `task_tool.rs` in WO 45.20;
// re-exported from this module as `crate::tools::task::Task`.)

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
    // (Test moved to `task_tool.rs` in WO 45.20.)

    #[test]
    fn task_manager_generates_unique_ids() {
        let mut mgr = TaskManager::new();
        let id1 = mgr.insert(TaskHandle::default());
        let id2 = mgr.insert(TaskHandle::default());
        assert_ne!(id1, id2);
        assert!(mgr.get(&id1).is_some());
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
            subagent_depth: 1,
            pending_messages: None,
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
            subagent_depth: 1,
            pending_messages: None,
        };
        assert!(req.model.is_none());
        assert!(
            req.cancel.is_none(),
            "foreground requests are uncancellable"
        );
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
    fn task_request_debug_includes_fields() {
        let req = TaskRequest {
            prompt: "p".into(),
            persona: "coder".into(),
            model: Some("m".into()),
            max_turns: 3,
            cancel: None,
            owner: None,
            subagent_depth: 0,
            pending_messages: None,
        };
        let s = format!("{req:?}");
        assert!(s.contains("coder") && s.contains("p") && s.contains("m"));
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

    // ── WO 30.2: TaskManager lifecycle (status / metadata / cancel / list) ──

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

    // ── WO 41.5 Phase 1: persist + reload round-trip ──────────────
    // (Tests moved to `persist.rs` in WO 45.20.)

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

    // ── WO 38.4: cancel cascades to nested subagents ──
    // (Test moved to `task_tool.rs` in WO 45.20.)

    // ── WO 39.3: dynamic agent registry integration ──

    fn reviewer_registry() -> std::sync::Arc<crate::session::agents::AgentRegistry> {
        let mut reg = crate::session::agents::AgentRegistry::new();
        reg.register(crate::session::agents::AgentDef {
            name: "reviewer".into(),
            description: "Reviews code".into(),
            system_prompt: "You are a senior code reviewer.".into(),
            tools: vec!["Read".into(), "Grep".into()],
            model: Some("fast-model".into()),
            max_turns: None,
            isolation: crate::session::agents::AgentIsolation::None,
            background: false,
            permission_mode: None,
            hooks: None,
            mcp_servers: None,
            memory: None,
        });
        std::sync::Arc::new(reg)
    }

    #[test]
    fn build_task_prompt_with_agents_uses_agent_system_prompt() {
        let reg = reviewer_registry();
        let p = build_task_prompt_with_agents("reviewer", "review PR", Some(&reg));
        assert!(
            p.contains("senior code reviewer"),
            "agent system prompt must be the preamble: {p}"
        );
        assert!(p.contains("review PR"));
        assert!(
            p.contains("Tool-name aliases"),
            "alias suffix must be appended: {p}"
        );
    }

    #[test]
    fn build_task_prompt_with_agents_falls_back_for_unknown_persona() {
        let reg = reviewer_registry();
        let p = build_task_prompt_with_agents("mystery", "do thing", Some(&reg));
        assert!(
            p.contains("implementation assistant"),
            "unknown persona with no agent falls back to coder preamble: {p}"
        );
    }

    #[test]
    fn build_task_prompt_with_agents_falls_back_when_registry_none() {
        let p = build_task_prompt_with_agents("reviewer", "do thing", None);
        assert!(
            p.contains("implementation assistant"),
            "None registry falls back to generic preamble: {p}"
        );
    }

    #[test]
    fn build_task_prompt_with_agents_built_in_personas_not_shadowed() {
        let reg = reviewer_registry();
        let p = build_task_prompt_with_agents("explore", "search", Some(&reg));
        assert!(
            p.contains("research assistant"),
            "built-in explore persona must not be shadowed by registry: {p}"
        );
    }

    // ── WO 39.3: Task tool agent registry integration ──
    // (Tests moved to `task_tool.rs` in WO 45.20.)
}
