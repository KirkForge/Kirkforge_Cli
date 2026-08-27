//! Durable subagent summaries (WO 41.5).
//!
//! On terminal state the worker closure persists a minimal summary of the
//! [`TaskHandle`](super::TaskHandle) to `<data_dir>/tasks/<id>.json` so
//! `--resume` can show what subagents ran without the in-memory HashMap.
//! Phase 1: write + read only; no live handle is rehydrated from disk.

use crate::tools::task::{task_id_rank, TaskHandle, TaskStatus};
use serde::{Deserialize, Serialize};

/// Serialized form of a completed subagent, written to
/// `tasks/<id>.json` on terminal state (WO 41.5 Phase 1). Carries the
/// fields needed to display a history row without the live
/// [`TaskHandle`] (which holds non-serializable `Notify`/`AtomicBool`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersistedTask {
    pub id: String,
    /// One-word status label (`completed` / `cancelled` / `failed` /
    /// `timed out`) — see [`TaskStatus::label`].
    pub status: String,
    /// The terminal summary: `result` for Completed, `cancelled_result`
    /// for Cancelled, `error` for Failed. `None` only for TimedOut
    /// (no payload today).
    pub summary: Option<String>,
    pub model: Option<String>,
    pub persona: String,
    pub prompt_summary: String,
    /// RFC 3339 string of `started_at` (chrono::Local).
    pub started_at: String,
    pub duration_ms: Option<u64>,
    pub parent_task_id: Option<String>,
    /// WO 45.1: AgentRun identity — the run_id of the parent session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_run_id: Option<String>,
}

impl PersistedTask {
    /// Build the serializable record from a terminal handle + its id.
    /// Returns `None` if the handle is not terminal (nothing to persist).
    fn from_handle(id: &str, handle: &TaskHandle) -> Option<Self> {
        let status = handle.status();
        if !status.is_terminal() {
            return None;
        }
        let (label, summary) = match &status {
            TaskStatus::Completed(r) => ("completed", Some(r.clone())),
            TaskStatus::Cancelled => ("cancelled", handle.cancelled_result.clone()),
            TaskStatus::Failed(e) => ("failed", Some(e.clone())),
            TaskStatus::TimedOut => ("timed out", None),
            // Non-terminal — guarded above; unreachable but keeps the
            // match exhaustive without a wildcard.
            TaskStatus::Pending | TaskStatus::Running => return None,
        };
        let m = &handle.metadata;
        Some(PersistedTask {
            id: id.to_string(),
            status: label.to_string(),
            summary,
            model: m.model.clone(),
            persona: m.persona.clone(),
            prompt_summary: m.prompt_summary.clone(),
            started_at: m.started_at.to_rfc3339(),
            duration_ms: m.duration_ms,
            parent_task_id: m.parent_task_id.clone(),
            parent_run_id: m.parent_run_id.clone(),
        })
    }

    /// Serialize to `<tasks_dir>/<id>.json`. Best-effort: a write failure
    /// is logged at `warn!` and swallowed — persistence is a side
    /// benefit, not a correctness gate for the task itself.
    fn persist_to_disk(&self) {
        let path = match crate::session::tasks_dir() {
            Ok(dir) => dir.join(format!("{}.json", self.id)),
            Err(e) => {
                tracing::warn!(error = %e, "cannot resolve tasks dir; skipping persist");
                return;
            }
        };
        match serde_json::to_string_pretty(self) {
            Ok(json) => {
                // WO 46.24: shared atomic_write uses O_EXCL + random tmp
                // name + rename, closing the predictable-.tmp symlink race.
                if let Err(e) = crate::tools::atomic_write::atomic_write(&path, json.as_bytes()) {
                    tracing::warn!(
                        error = %e,
                        path = %path.display(),
                        "failed to persist task summary"
                    );
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, id = %self.id, "failed to serialize task summary");
            }
        }
    }
}

/// Persist a terminal task's summary to disk (WO 41.5 Phase 1). Called
/// from the worker closure right before `notify.notify_waiters()`.
/// No-op if the handle is not terminal.
pub(crate) fn persist_task_summary(id: &str, handle: &TaskHandle) {
    if let Some(pt) = PersistedTask::from_handle(id, handle) {
        pt.persist_to_disk();
    }
}

/// Read every `*.json` file in the tasks dir, returning all persisted
/// summaries sorted by numeric task id (so `task-2` precedes `task-10`).
/// Malformed files are skipped with a `warn!` — a corrupt single file
/// must not blank the whole history.
pub fn load_persisted_tasks() -> Vec<PersistedTask> {
    let dir = match crate::session::tasks_dir() {
        Ok(d) => d,
        Err(e) => {
            tracing::warn!(error = %e, "cannot resolve tasks dir; no persisted history");
            return Vec::new();
        }
    };
    let entries = match std::fs::read_dir(&dir) {
        Ok(e) => e,
        Err(_) => return Vec::new(),
    };
    let mut tasks: Vec<PersistedTask> = entries
        .flatten()
        .filter_map(|e| {
            let path = e.path();
            if path.extension().and_then(|x| x.to_str()) != Some("json") {
                return None;
            }
            match std::fs::read_to_string(&path) {
                Ok(json) => serde_json::from_str::<PersistedTask>(&json).ok(),
                Err(_) => None,
            }
        })
        .collect();
    tasks.sort_by_key(|t| task_id_rank(&t.id));
    tasks
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::DataDirGuard;
    use crate::shared::ToolOutcome;
    use crate::tools::task::test_helpers::{
        extract_task_id, poll_until, CooperativeProbe, MockSpawner,
    };
    use crate::tools::task::{Task, TaskManager, TaskMetadata, TaskSpawner};
    use crate::tools::{Tool, ToolContext};
    use std::sync::atomic::AtomicBool;
    use std::sync::{Arc, Mutex};

    // WO 47.21: thread-local override instead of KF_CODE_DATA_DIR env
    // mutation — parallel tests in other threads are unaffected, no
    // EnvGuard race. The persist worker runs via tokio::spawn on the
    // same (current_thread) test thread, so it sees the override.
    fn data_dir_tmp() -> (tempfile::TempDir, DataDirGuard) {
        let dir = tempfile::tempdir().unwrap();
        let guard = DataDirGuard::set(dir.path().to_path_buf());
        (dir, guard)
    }

    fn terminal_handle(
        result: Option<&str>,
        cancelled: bool,
        cancelled_result: Option<&str>,
        error: Option<&str>,
    ) -> TaskHandle {
        TaskHandle {
            result: result.map(String::from),
            error: error.map(String::from),
            cancelled_result: cancelled_result.map(String::from),
            cancel_requested: Arc::new(AtomicBool::new(cancelled)),
            metadata: TaskMetadata {
                model: Some("qwen".to_string()),
                persona: "explore".to_string(),
                prompt_summary: "scan the repo".to_string(),
                started_at: chrono::Local::now(),
                duration_ms: Some(1_500),
                token_estimate: None,
                parent_task_id: Some("task-1".to_string()),
                parent_run_id: None,
            },
            ..Default::default()
        }
    }

    #[test]
    fn persist_then_reload_completed_task() {
        let (_dir, _env) = data_dir_tmp();
        let handle = terminal_handle(Some("did it"), false, None, None);
        persist_task_summary("task-100", &handle);

        let loaded = load_persisted_tasks();
        assert_eq!(loaded.len(), 1);
        let t = &loaded[0];
        assert_eq!(t.id, "task-100");
        assert_eq!(t.status, "completed");
        assert_eq!(t.summary.as_deref(), Some("did it"));
        assert_eq!(t.persona, "explore");
        assert_eq!(t.model.as_deref(), Some("qwen"));
        assert_eq!(t.prompt_summary, "scan the repo");
        assert_eq!(t.duration_ms, Some(1_500));
        assert_eq!(t.parent_task_id.as_deref(), Some("task-1"));
        assert!(!t.started_at.is_empty());
    }

    #[test]
    fn persist_then_reload_cancelled_task() {
        let (_dir, _env) = data_dir_tmp();
        let handle = terminal_handle(None, true, Some("partial"), None);
        persist_task_summary("task-3", &handle);

        let loaded = load_persisted_tasks();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].status, "cancelled");
        assert_eq!(loaded[0].summary.as_deref(), Some("partial"));
    }

    #[test]
    fn persist_then_reload_failed_task() {
        let (_dir, _env) = data_dir_tmp();
        let handle = terminal_handle(None, false, None, Some("boom"));
        persist_task_summary("task-9", &handle);

        let loaded = load_persisted_tasks();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].status, "failed");
        assert_eq!(loaded[0].summary.as_deref(), Some("boom"));
    }

    #[test]
    fn persist_skips_non_terminal_handle() {
        let (_dir, _env) = data_dir_tmp();
        let handle = TaskHandle {
            started: Arc::new(AtomicBool::new(true)),
            ..Default::default()
        };
        persist_task_summary("task-99", &handle);
        assert!(load_persisted_tasks().is_empty());
    }

    // WO 43.21: atomic write leaves no .tmp file behind and the JSON is valid.
    #[test]
    fn persist_to_disk_uses_atomic_write_no_tmp_left() {
        let (dir, _env) = data_dir_tmp();
        let handle = terminal_handle(Some("done"), false, None, None);
        persist_task_summary("task-atom", &handle);
        let tasks_dir = dir.path().join("tasks");
        // The final file exists and is valid JSON.
        let path = tasks_dir.join("task-atom.json");
        let content = std::fs::read_to_string(&path).unwrap();
        let _: serde_json::Value = serde_json::from_str(&content).unwrap();
        // No leftover temp file — the shared atomic_write cleans up its
        // random-named temp on success. Only the target .json should remain.
        let leftovers: Vec<_> = std::fs::read_dir(&tasks_dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect();
        assert!(
            leftovers.iter().all(|n| n == "task-atom.json"),
            "only the target .json should remain, got: {leftovers:?}"
        );
    }

    // End-to-end: the worker closure persists to disk when a background
    // task completes. This is the core Phase 1 gate — the JSON file must
    // exist with the correct content after the task finishes.
    #[tokio::test]
    async fn background_task_persists_summary_on_completion() {
        let (_dir, _env) = data_dir_tmp();
        let manager = Arc::new(Mutex::new(TaskManager::new()));
        let task = Task::with_manager(manager.clone());
        let spawner: Arc<dyn TaskSpawner> = Arc::new(MockSpawner {
            result: Ok("done summary".to_string()),
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

        // Wait for the worker to persist (terminal + file write).
        poll_until("persisted JSON file appears on disk", || {
            let tasks_dir = crate::session::tasks_dir().unwrap();
            let path = tasks_dir.join(format!("{id}.json"));
            path.exists().then_some(())
        })
        .await;

        let loaded = load_persisted_tasks();
        let t = loaded
            .iter()
            .find(|t| t.id == id)
            .expect("persisted task must be in the loaded list");
        assert_eq!(t.status, "completed");
        assert_eq!(t.summary.as_deref(), Some("done summary"));
        assert_eq!(t.persona, "explore");
        assert_eq!(t.model.as_deref(), Some("qwen2.5:0.5b"));
        assert!(t.duration_ms.is_some());
    }

    // A cancelled task persists with Cancelled status and its partial
    // output in `summary` (from `cancelled_result`).
    #[tokio::test]
    async fn background_cancelled_task_persists_as_cancelled() {
        let (_dir, _env) = data_dir_tmp();
        let manager = Arc::new(Mutex::new(TaskManager::new()));
        let task = Task::with_manager(manager.clone());
        let probe = CooperativeProbe::new();
        let spawner: Arc<dyn TaskSpawner> = Arc::new(probe.spawner);
        let ctx = ToolContext::with_spawner(spawner);
        let outcome = task
            .run(
                &ctx,
                serde_json::json!({
                    "prompt": "long running",
                    "background": true,
                }),
            )
            .await;
        let content = match outcome {
            ToolOutcome::Success { content } => content,
            other => panic!("expected Success, got {other:?}"),
        };
        let id = extract_task_id(&content);

        // Wait for Running, then cancel.
        poll_until("task reaches Running", || {
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
        manager
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .cancel(&id);

        // The worker awaits run_task, which returns after the cancel flag
        // fires (CooperativeSpawner returns "partial work"). Then it
        // persists + notifies.
        poll_until("cancelled task persists to disk", || {
            let tasks_dir = crate::session::tasks_dir().unwrap();
            let path = tasks_dir.join(format!("{id}.json"));
            path.exists().then_some(())
        })
        .await;

        let loaded = load_persisted_tasks();
        let t = loaded
            .iter()
            .find(|t| t.id == id)
            .expect("cancelled task must be persisted");
        assert_eq!(t.status, "cancelled");
        assert_eq!(t.summary.as_deref(), Some("partial work"));
    }
}
