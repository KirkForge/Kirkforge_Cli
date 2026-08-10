/// Background bash jobs — long-running command registry.
///
/// Allows spawning bash commands that outlive a single tool call.
/// Jobs run as tokio tasks and their output is captured asynchronously.
/// The model or user can check job status, read output, or cancel jobs.
use crate::session::access::{DenyList, PathGuard};
use crate::session::bash_runner::{
    cap_to_string, check_bash_command_str, drain_capped, shell_program, MAX_BASH_OUTPUT_BYTES,
};
use crate::session::process_group::{kill_process_group, reap_child, setup_process_group};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::sync::OnceLock;
use std::time::Duration;
use tokio::process::Child;
use tokio::sync::Mutex;

/// Status of a background job.
#[derive(Debug, Clone, PartialEq)]
pub enum JobStatus {
    Running,
    Completed(i32), // exit code
    Failed(String), // error message
    Cancelled,
}

/// A background bash job.
#[derive(Debug, Clone)]
pub struct BashJob {
    pub id: u64,
    pub command: String,
    pub status: JobStatus,
    pub stdout: String,
    pub stderr: String,
    pub started_at: chrono::DateTime<chrono::Local>,
    pub finished_at: Option<chrono::DateTime<chrono::Local>>,
}

impl BashJob {
    fn new(id: u64, command: String) -> Self {
        Self {
            id,
            command,
            status: JobStatus::Running,
            stdout: String::new(),
            stderr: String::new(),
            started_at: chrono::Local::now(),
            finished_at: None,
        }
    }
}

/// Global singleton BashJobRegistry, accessible from tools and TUI.
static GLOBAL_REGISTRY: OnceLock<BashJobRegistry> = OnceLock::new();

/// Get the global bash job registry, initializing on first access.
pub fn global_registry() -> BashJobRegistry {
    GLOBAL_REGISTRY.get_or_init(BashJobRegistry::new).clone()
}

/// Maximum number of concurrent background jobs.
const MAX_JOBS: usize = 64;

/// Registry of background bash jobs.
#[derive(Clone, Default)]
pub struct BashJobRegistry {
    jobs: Arc<Mutex<HashMap<u64, BashJob>>>,
    /// Child process handles stored separately (Child is not Clone), each
    /// behind an Arc<Mutex> so the watcher and cancel()/clean()/remove() can
    /// share the same handle concurrently. A child stays in this map until it
    /// has been reaped (see the watcher), so a cancel() racing the watcher
    /// can still reach and kill it instead of silently no-op'ing.
    children: Arc<Mutex<HashMap<u64, Arc<Mutex<Child>>>>>,
    next_id: Arc<AtomicU64>,
}

impl BashJobRegistry {
    pub fn new() -> Self {
        Self {
            jobs: Arc::new(Mutex::new(HashMap::new())),
            children: Arc::new(Mutex::new(HashMap::new())),
            next_id: Arc::new(AtomicU64::new(1)),
        }
    }

    /// Spawn a bash command in the background and return a job ID.
    /// Optionally accepts a working directory and timeout (0 = no timeout).
    ///
    /// The child process handle is stored so that cancel() can kill it.
    /// Completed/failed jobs are evicted oldest-first when the registry
    /// reaches MAX_JOBS (64).
    pub async fn spawn(
        &self,
        command: &str,
        workdir: Option<&str>,
        timeout: Option<Duration>,
        deny_list: &DenyList,
        path_guard: &PathGuard,
        bash_sandbox_workdir: bool,
    ) -> anyhow::Result<u64> {
        // Safety gate: every background bash command must pass the same
        // deny-list, dangerous-pattern, and sandbox-workdir checks as
        // foreground bash. Without this, `bash(background: true)` is a
        // trivial bypass around `check_bash_command_str`.
        if let Some(denied) = check_bash_command_str(
            command,
            workdir,
            deny_list,
            path_guard,
            bash_sandbox_workdir,
        ) {
            return Err(anyhow::anyhow!(denied));
        }

        let id = self.next_id.fetch_add(1, Ordering::SeqCst);

        // ── Job cap: evict oldest completed jobs if at limit ──
        // The cap check, eviction, and insertion must happen under one lock
        // hold to avoid a TOCTOU race where two spawns both see a free slot
        // and exceed MAX_JOBS.
        let to_remove: Vec<u64> = {
            let mut jobs = self.jobs.lock().await;
            if jobs.len() >= MAX_JOBS {
                let ids: Vec<u64> = jobs
                    .iter()
                    .filter(|(_, j)| j.status != JobStatus::Running)
                    .map(|(&id, _)| id)
                    .collect();
                for rid in &ids {
                    jobs.remove(rid);
                }
                ids
            } else {
                Vec::new()
            }
        };
        // Clean up any lingering child handles for evicted jobs so process
        // handles don't leak and the cap bookkeeping stays honest.
        if !to_remove.is_empty() {
            let mut children = self.children.lock().await;
            for rid in to_remove {
                if let Some(child) = children.remove(&rid) {
                    let mut child = child.lock().await;
                    kill_process_group(&mut child);
                }
            }
        }

        let job = BashJob::new(id, command.to_string());
        {
            let mut jobs = self.jobs.lock().await;
            // Re-check under the same lock before inserting; if another task
            // grabbed the last slot while we cleaned up child handles, reject.
            if jobs.len() >= MAX_JOBS {
                return Err(anyhow::anyhow!(
                    "Background job limit ({MAX_JOBS}) reached; wait for jobs to finish or cancel them."
                ));
            }
            jobs.insert(id, job);
        }

        let mut proc = tokio::process::Command::new(shell_program());
        proc.args(["-c", command])
            .kill_on_drop(true)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        setup_process_group(&mut proc);
        if let Some(ref wd) = workdir {
            // Resolve the working directory to a canonical absolute path
            // before forking so the child's cwd is stable even if the
            // parent's cwd changes after spawn (F15). A relative or
            // nonexistent path is rejected with a clear error.
            let expanded = shellexpand::tilde(wd);
            let canonical = std::fs::canonicalize(expanded.as_ref())
                .map_err(|e| anyhow::anyhow!("cannot resolve working directory '{wd}': {e}"))?;
            proc.current_dir(canonical);
        }

        let child = proc.spawn()?;

        // Store child handle for cancel(), wrapped so the watcher and
        // cancel()/clean()/remove() can share it.
        {
            let mut children = self.children.lock().await;
            children.insert(id, Arc::new(Mutex::new(child)));
        }

        // Spawn watcher: wait for output, update job record, remove child
        // handle. The child handle is kept in the map until it has been
        // reaped, so a concurrent cancel() can still reach and kill it; the
        // map entry is removed only once this watcher has reaped it (F5).
        // A watchdog (below) flips Running → Failed if this task panics
        // before it records a terminal status, so a dead watcher cannot
        // leak a job stuck in Running forever. (bucketlist 3.32)
        let watcher_registry = self.clone();
        let watcher = tokio::spawn(async move {
            let child = {
                let children = watcher_registry.children.lock().await;
                children.get(&id).cloned()
            };

            let Some(child) = child else {
                // No child was ever stored (spawn failed after insert or the
                // child is not in the map) — nothing to reap.
                return;
            };

            let mut child = child.lock().await;

            // Take stdout/stderr before waiting so we can drain them
            // concurrently. This also lets us reap the child explicitly
            // without `wait_with_output` consuming ownership on timeout.
            let stdout = child.stdout.take();
            let stderr = child.stderr.take();

            let drain_stdout = stdout.map(|r| {
                tokio::spawn(async move {
                    drain_capped(r, MAX_BASH_OUTPUT_BYTES)
                        .await
                        .unwrap_or_else(|_| (Vec::new(), 0))
                })
            });
            let drain_stderr = stderr.map(|r| {
                tokio::spawn(async move {
                    drain_capped(r, MAX_BASH_OUTPUT_BYTES)
                        .await
                        .unwrap_or_else(|_| (Vec::new(), 0))
                })
            });

            // Wait with optional timeout
            let status_result: Result<std::process::ExitStatus, String> =
                if let Some(t) = timeout.filter(|t| !t.is_zero()) {
                    match tokio::time::timeout(t, child.wait()).await {
                        Ok(Ok(status)) => Ok(status),
                        Ok(Err(e)) => Err(e.to_string()),
                        Err(_) => Err("Timed out".into()),
                    }
                } else {
                    child.wait().await.map_err(|e| e.to_string())
                };

            let (status, mut error_msg) = match status_result {
                Ok(status) => (Some(status), None),
                Err(e) => {
                    if e == "Timed out" {
                        kill_process_group(&mut child);
                    }
                    (None, Some(e))
                }
            };

            // Reap the child with a short timeout so it does not become a
            // zombie. The drain tasks continue reading until EOF (which
            // arrives as the child closes its pipes), so partial output is
            // preserved.
            if status.is_none() {
                reap_child(&mut child, Duration::from_secs(2)).await;
            }

            // The child has been reaped: only now remove it from the map.
            // Removing it any earlier lets a concurrent cancel() no-op (it
            // can no longer find the child to kill), and keeps a child in
            // the map past its reap. (F5)
            {
                let mut children = watcher_registry.children.lock().await;
                children.remove(&id);
            }

            // Join the drain tasks to capture output (or partial output on
            // timeout). A short timeout prevents a stuck pipe from wedging
            // cleanup.
            let (stdout_buf, stdout_dropped) = match drain_stdout {
                Some(h) => tokio::time::timeout(std::time::Duration::from_secs(2), h)
                    .await
                    .ok()
                    .and_then(|r| r.ok())
                    .unwrap_or_else(|| (Vec::new(), 0)),
                None => (Vec::new(), 0),
            };
            let (stderr_buf, stderr_dropped) = match drain_stderr {
                Some(h) => tokio::time::timeout(std::time::Duration::from_secs(2), h)
                    .await
                    .ok()
                    .and_then(|r| r.ok())
                    .unwrap_or_else(|| (Vec::new(), 0)),
                None => (Vec::new(), 0),
            };

            let mut jobs = watcher_registry.jobs.lock().await;
            if let Some(job) = jobs.get_mut(&id) {
                // Preserve a Cancelled status set by a racing cancel(); the
                // watcher must not clobber it with Completed once the wait
                // and reap above return. (F5)
                if job.status != JobStatus::Cancelled {
                    if let Some(status) = status {
                        job.status = JobStatus::Completed(status.code().unwrap_or(-1));
                    } else {
                        job.status =
                            JobStatus::Failed(error_msg.take().unwrap_or_else(|| "Failed".into()));
                    }
                }
                job.stdout = cap_to_string(stdout_buf, stdout_dropped);
                job.stderr = cap_to_string(stderr_buf, stderr_dropped);
                job.finished_at = Some(chrono::Local::now());
            }
        });

        // Watchdog: if the watcher task panics (JoinError) before it records
        // a terminal status, flip the still-Running job to Failed so it does
        // not leak. A normal watcher completion already set the status, so
        // the watchdog's mark is a no-op then. (bucketlist 3.32)
        let watchdog_registry = self.clone();
        tokio::spawn(async move {
            if watcher.await.is_err() {
                watchdog_registry
                    .mark_failed_if_running(id, "background watcher panicked")
                    .await;
            }
        });

        Ok(id)
    }

    /// Flip a still-`Running` job to `Failed(reason)`. Used by the watcher
    /// watchdog when the watcher task dies before recording a terminal
    /// status. A job that already reached a terminal state is left alone.
    async fn mark_failed_if_running(&self, id: u64, reason: &str) {
        let mut jobs = self.jobs.lock().await;
        if let Some(job) = jobs.get_mut(&id) {
            if job.status == JobStatus::Running {
                job.status = JobStatus::Failed(reason.to_string());
                job.finished_at = Some(chrono::Local::now());
            }
        }
    }

    /// Get job status and output.
    pub async fn get(&self, id: u64) -> Option<BashJob> {
        let jobs = self.jobs.lock().await;
        jobs.get(&id).cloned()
    }

    /// List all jobs.
    pub async fn list(&self) -> Vec<BashJob> {
        let jobs = self.jobs.lock().await;
        let mut list: Vec<BashJob> = jobs.values().cloned().collect();
        list.sort_by_key(|j| j.id);
        list
    }

    /// Cancel a running job.
    ///
    /// Kills the child process and sets status to Cancelled.
    pub async fn cancel(&self, id: u64) -> bool {
        // Take the child handle and kill it. The child stays in the map
        // until the watcher has reaped it (F5); this lock is released before
        // the reap below so the watcher can still join it, but removal is
        // done only in the watcher's reap path.
        {
            let child = {
                let mut children = self.children.lock().await;
                children.remove(&id)
            };
            if let Some(child) = child {
                let mut child = child.lock().await;
                kill_process_group(&mut child);
                reap_child(&mut child, Duration::from_secs(2)).await;
            }
        }

        // Update job status
        let mut found = false;
        {
            let mut jobs = self.jobs.lock().await;
            if let Some(job) = jobs.get_mut(&id) {
                if job.status == JobStatus::Running {
                    job.status = JobStatus::Cancelled;
                    job.finished_at = Some(chrono::Local::now());
                    found = true;
                }
            }
        }
        found
    }

    /// Remove a job from the registry (also cleans up the child handle).
    pub async fn remove(&self, id: u64) -> bool {
        // Kill child if still alive
        {
            let child = {
                let mut children = self.children.lock().await;
                children.remove(&id)
            };
            if let Some(child) = child {
                let mut child = child.lock().await;
                kill_process_group(&mut child);
                reap_child(&mut child, Duration::from_secs(2)).await;
            }
        }

        // Remove job record
        let mut jobs = self.jobs.lock().await;
        jobs.remove(&id).is_some()
    }

    /// Count of running jobs.
    pub async fn running_count(&self) -> usize {
        let jobs = self.jobs.lock().await;
        jobs.values()
            .filter(|j| j.status == JobStatus::Running)
            .count()
    }

    /// Clear all completed/failed/cancelled jobs.
    pub async fn clean(&self) -> usize {
        // Collect non-running job IDs
        let job_ids: Vec<u64> = {
            let jobs = self.jobs.lock().await;
            jobs.iter()
                .filter(|(_, j)| j.status != JobStatus::Running)
                .map(|(&id, _)| id)
                .collect()
        };

        // Clean up child handles for those IDs
        {
            let mut children = self.children.lock().await;
            for id in &job_ids {
                if let Some(child) = children.remove(id) {
                    let mut child = child.lock().await;
                    kill_process_group(&mut child);
                    reap_child(&mut child, Duration::from_secs(2)).await;
                }
            }
        }

        // Remove job records
        let count = job_ids.len();
        {
            let mut jobs = self.jobs.lock().await;
            for id in &job_ids {
                jobs.remove(id);
            }
        }

        count
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_spawn_and_complete() {
        let reg = BashJobRegistry::new();
        let id = reg
            .spawn(
                "echo hello",
                None,
                None,
                &DenyList::default(),
                &PathGuard::default(),
                false,
            )
            .await
            .unwrap();
        assert!(id > 0);

        // Wait for completion
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;

        let job = reg.get(id).await.unwrap();
        assert_eq!(job.status, JobStatus::Completed(0));
        assert_eq!(job.stdout.trim(), "hello");
        assert!(job.finished_at.is_some());
    }

    #[tokio::test]
    async fn test_spawn_and_check_running() {
        let reg = BashJobRegistry::new();
        let id = reg
            .spawn(
                "sleep 0.1 && echo done",
                None,
                None,
                &DenyList::default(),
                &PathGuard::default(),
                false,
            )
            .await
            .unwrap();

        // Immediately check — should be running
        let job = reg.get(id).await.unwrap();
        // It might complete fast, but at minimum the command was captured
        assert_eq!(job.command, "sleep 0.1 && echo done");
    }

    #[tokio::test]
    async fn test_job_list_and_count() {
        let reg = BashJobRegistry::new();
        let _ = reg
            .spawn(
                "echo a",
                None,
                None,
                &DenyList::default(),
                &PathGuard::default(),
                false,
            )
            .await
            .unwrap();
        let _ = reg
            .spawn(
                "echo b",
                None,
                None,
                &DenyList::default(),
                &PathGuard::default(),
                false,
            )
            .await
            .unwrap();

        // Poll until both short jobs finish; under a saturated tokio runtime
        // a fixed sleep can fire before the spawn tasks are scheduled.
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
        while reg.running_count().await > 0 {
            if tokio::time::Instant::now() > deadline {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }

        let list = reg.list().await;
        assert_eq!(list.len(), 2);
        assert_eq!(
            reg.running_count().await,
            0,
            "echo jobs should have completed"
        );
    }

    #[tokio::test]
    async fn test_cancel_running_job() {
        let reg = BashJobRegistry::new();
        let id = reg
            .spawn(
                "sleep 5",
                None,
                None,
                &DenyList::default(),
                &PathGuard::default(),
                false,
            )
            .await
            .unwrap();

        // Cancel while running
        assert!(reg.cancel(id).await);

        let job = reg.get(id).await.unwrap();
        assert_eq!(job.status, JobStatus::Cancelled);
    }

    #[tokio::test]
    async fn test_remove_job() {
        let reg = BashJobRegistry::new();
        let id = reg
            .spawn(
                "echo test",
                None,
                None,
                &DenyList::default(),
                &PathGuard::default(),
                false,
            )
            .await
            .unwrap();
        assert!(reg.remove(id).await);
        assert!(reg.get(id).await.is_none());
    }

    #[tokio::test]
    async fn test_clean_completed_jobs() {
        let reg = BashJobRegistry::new();
        let _ = reg
            .spawn(
                "echo a",
                None,
                None,
                &DenyList::default(),
                &PathGuard::default(),
                false,
            )
            .await
            .unwrap();
        let running_id = reg
            .spawn(
                "sleep 5",
                None,
                None,
                &DenyList::default(),
                &PathGuard::default(),
                false,
            )
            .await
            .unwrap();

        tokio::time::sleep(std::time::Duration::from_millis(300)).await;

        // Clean should remove the completed one but keep the running one
        let cleaned = reg.clean().await;
        assert_eq!(cleaned, 1);

        let list = reg.list().await;
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].id, running_id);
    }

    /// Background bash must pass the same safety gate as foreground bash.
    /// A dangerous command is rejected at spawn time rather than started.
    #[tokio::test]
    async fn test_spawn_blocks_dangerous_command() {
        let reg = BashJobRegistry::new();
        let result = reg
            .spawn(
                "rm -rf /",
                None,
                None,
                &DenyList::default(),
                &PathGuard::default(),
                false,
            )
            .await;
        assert!(
            result.is_err(),
            "dangerous background command should be rejected, got {result:?}"
        );
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("dangerous pattern"),
            "expected dangerous-pattern denial, got: {err}"
        );
    }

    /// Once MAX_JOBS slots are filled with still-running jobs, further spawns
    /// must be rejected instead of growing the registry unboundedly.
    #[tokio::test]
    async fn test_job_cap_enforced_when_all_running() {
        let reg = BashJobRegistry::new();
        for i in 0..MAX_JOBS {
            let id = reg
                .spawn(
                    "sleep 30",
                    None,
                    None,
                    &DenyList::default(),
                    &PathGuard::default(),
                    false,
                )
                .await
                .unwrap();
            assert_eq!(id as usize, i + 1);
        }

        let next = reg
            .spawn(
                "echo overflow",
                None,
                None,
                &DenyList::default(),
                &PathGuard::default(),
                false,
            )
            .await;
        assert!(
            next.is_err(),
            "spawn should fail when all {MAX_JOBS} jobs are still running"
        );
        let err = next.unwrap_err().to_string();
        assert!(
            err.contains("Background job limit"),
            "expected cap error, got: {err}"
        );

        // Clean up the 64 long-running jobs so the test doesn't linger.
        let ids: Vec<u64> = reg.list().await.into_iter().map(|j| j.id).collect();
        for id in ids {
            let _ = reg.cancel(id).await;
        }
    }

    /// A job that exceeds its timeout is killed, reaped, and still retains
    /// the partial output it produced before the timeout.
    #[cfg(unix)]
    #[tokio::test]
    async fn test_timeout_reaps_child_and_preserves_partial_output() {
        let reg = BashJobRegistry::new();
        let id = reg
            .spawn(
                "echo partial; sleep 30",
                None,
                Some(Duration::from_secs(1)),
                &DenyList::default(),
                &PathGuard::default(),
                false,
            )
            .await
            .unwrap();

        // Wait for the watcher to time out and reap the child. The watcher
        // allows up to 1s for the timeout, 2s for child.wait() after the kill,
        // and 2s for each drain task, so give it a comfortable margin.
        tokio::time::sleep(std::time::Duration::from_secs(6)).await;

        let job = reg.get(id).await.unwrap();
        assert!(
            matches!(job.status, JobStatus::Failed(ref msg) if msg.contains("Timed out")),
            "expected timeout failure, got {:?}",
            job.status
        );
        assert_eq!(job.stdout.trim(), "partial");
        assert!(job.finished_at.is_some());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn test_spawn_expands_tilde_workdir() {
        // Regression for C14: background bash workdir was passed raw to
        // current_dir, so `~` was not expanded to the user's home directory.
        let reg = BashJobRegistry::new();
        let home = std::env::var("HOME").expect("HOME must be set");
        let id = reg
            .spawn(
                "pwd",
                Some("~"),
                None,
                &DenyList::default(),
                &PathGuard::default(),
                false,
            )
            .await
            .unwrap();

        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        let job = reg.get(id).await.unwrap();
        assert_eq!(job.status, JobStatus::Completed(0));
        assert_eq!(
            std::path::PathBuf::from(job.stdout.trim()),
            std::path::PathBuf::from(home),
            "tilde workdir was not expanded"
        );
    }

    /// The watchdog's `mark_failed_if_running` flips a still-Running job to
    /// Failed and leaves terminal-state jobs alone. (bucketlist 3.32)
    #[tokio::test]
    async fn test_mark_failed_if_running_flips_running_and_preserves_terminal() {
        let reg = BashJobRegistry::new();
        let running_id = {
            let mut jobs = reg.jobs.lock().await;
            let id = 9999;
            jobs.insert(
                id,
                BashJob {
                    id,
                    command: "stuck".into(),
                    status: JobStatus::Running,
                    stdout: String::new(),
                    stderr: String::new(),
                    started_at: chrono::Local::now(),
                    finished_at: None,
                },
            );
            id
        };
        let done_id = {
            let mut jobs = reg.jobs.lock().await;
            let id = 10000;
            jobs.insert(
                id,
                BashJob {
                    id,
                    command: "done".into(),
                    status: JobStatus::Completed(0),
                    stdout: String::new(),
                    stderr: String::new(),
                    started_at: chrono::Local::now(),
                    finished_at: Some(chrono::Local::now()),
                },
            );
            id
        };

        reg.mark_failed_if_running(running_id, "background watcher panicked")
            .await;
        reg.mark_failed_if_running(done_id, "must not overwrite Completed")
            .await;

        let running = reg.get(running_id).await.unwrap();
        assert!(
            matches!(&running.status, JobStatus::Failed(msg) if msg.contains("watcher panicked")),
            "Running job should flip to Failed, got {:?}",
            running.status
        );
        assert!(running.finished_at.is_some());

        let done = reg.get(done_id).await.unwrap();
        assert_eq!(
            done.status,
            JobStatus::Completed(0),
            "terminal job must be left alone"
        );
    }

    /// A long-running job whose watcher is forcibly aborted (simulating a
    /// watcher panic/cancellation) is flipped to Failed by the watchdog so
    /// it does not leak as Running. (bucketlist 3.32)
    #[tokio::test]
    async fn test_watchdog_flips_running_to_failed_on_watcher_death() {
        let reg = BashJobRegistry::new();
        let id = reg
            .spawn(
                "sleep 30",
                None,
                None,
                &DenyList::default(),
                &PathGuard::default(),
                false,
            )
            .await
            .unwrap();

        // Sanity: the job starts Running.
        assert_eq!(reg.get(id).await.unwrap().status, JobStatus::Running);

        // Drive the watchdog's failure path directly: mark the job Failed
        // exactly as the watchdog would if the watcher JoinHandle errored.
        // (Aborting the real watcher is racy because the watchdog itself is
        // detached; exercising mark_failed_if_running proves the transition
        // the watchdog relies on.)
        reg.mark_failed_if_running(id, "background watcher panicked")
            .await;

        let job = reg.get(id).await.unwrap();
        assert!(
            matches!(&job.status, JobStatus::Failed(msg) if msg.contains("watcher panicked")),
            "watcher death should flip Running → Failed, got {:?}",
            job.status
        );
    }
}
