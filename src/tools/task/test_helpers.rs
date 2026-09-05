//! Shared test helpers for the `task` submodule. `#[cfg(test)]` only.
//!
//! `pub(super)` so the test modules in `mod.rs`, `persist.rs`, and
//! `task_tool.rs` can all `use super::test_helpers::*` (or `use
//! crate::tools::task::test_helpers::*` from sibling submodules).

use crate::tools::task::{TaskCancel, TaskRequest, TaskSpawner};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

// Poll a condition until it returns Some(T), with a bounded 1s total budget
// and a 10ms interval. Replaces `for _ in 0..50 { sleep(20ms) }` loops.
// Panics on timeout so a regression fails loudly instead of silently
// advancing to a flaky assertion.
pub(super) async fn poll_until<T, F>(label: &str, mut cond: F) -> T
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

pub(super) fn extract_task_id(content: &str) -> String {
    content
        .split_whitespace()
        .find(|w| w.starts_with("task-"))
        .map(|w| w.trim_end_matches('.'))
        .unwrap()
        .to_string()
}

pub(super) struct MockSpawner {
    pub result: Result<String, String>,
}

#[async_trait::async_trait]
impl TaskSpawner for MockSpawner {
    async fn run_task(&self, _request: TaskRequest) -> Result<String, String> {
        self.result.clone()
    }
}

pub(super) struct BlockingSpawner {
    pub started: Arc<tokio::sync::Notify>,
    pub finish: Arc<AtomicBool>,
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
pub(super) struct CooperativeSpawner {
    pub started: Arc<tokio::sync::Notify>,
    pub observed_cancel: Arc<Mutex<Option<TaskCancel>>>,
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

pub(super) struct CooperativeProbe {
    pub spawner: CooperativeSpawner,
    pub observed_cancel: Arc<Mutex<Option<TaskCancel>>>,
}

impl CooperativeProbe {
    pub(super) fn new() -> Self {
        let observed_cancel = Arc::new(Mutex::new(None));
        Self {
            spawner: CooperativeSpawner {
                started: Arc::new(tokio::sync::Notify::new()),
                observed_cancel: observed_cancel.clone(),
            },
            observed_cancel,
        }
    }
}
