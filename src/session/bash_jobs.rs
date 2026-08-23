/// Background bash jobs — long-running command registry.
///
/// Allows spawning bash commands that outlive a single tool call.
/// Jobs run as tokio tasks and their output is captured asynchronously.
/// The model or user can check job status, read output, or cancel jobs.
use crate::session::access::{DenyList, PathGuard};
use crate::session::bash_runner::{
    cap_to_string, check_bash_command_str, drain_capped, setup_rlimits, shell_program,
    MAX_BASH_OUTPUT_BYTES,
};
use crate::session::process_group::{
    kill_process_group, kill_process_group_by_pid, reap_child, setup_process_group,
};
use crate::shared::SandboxConfig;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::Write;
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

/// One line in the session-exit summary log (`bg-exits.ndjson`).
/// Persisted on session teardown for every still-Running job so
/// `--resume` can report "these jobs died with the session" (WO 43.10).
/// Jobs that already reached a terminal state are not written — only
/// the ones the process is about to kill.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BgJobExitSummary {
    pub id: u64,
    pub command: String,
    /// Human-readable status-at-exit. Always "died-with-session" for
    /// the summary written at teardown; the watcher never runs after
    /// the process exits, so the real exit code is unknowable.
    pub status_at_exit: String,
    pub session_id: String,
    pub started_at: chrono::DateTime<chrono::Local>,
}

/// A background bash job.
#[derive(Debug, Clone)]
pub struct BashJob {
    pub id: u64,
    pub command: String,
    /// Owning subagent task id (WO 36.2). `None` = main session — such
    /// jobs are never touched by task-cancel paths (`cancel_by_owner`
    /// only matches `Some(owner)`).
    pub owner: Option<String>,
    /// Child process id, recorded so cancel can kill the process group
    /// without the `Child` mutex (the watcher parks on that mutex inside
    /// `wait().await` for the job's whole lifetime). `None` until the
    /// child spawns / for synthesized test jobs.
    pub pid: Option<u32>,
    pub status: JobStatus,
    pub stdout: String,
    pub stderr: String,
    pub started_at: chrono::DateTime<chrono::Local>,
    pub finished_at: Option<chrono::DateTime<chrono::Local>>,
}

impl BashJob {
    fn new(id: u64, command: String, owner: Option<&str>) -> Self {
        Self {
            id,
            command,
            owner: owner.map(str::to_string),
            pid: None,
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

/// Returns `Ok(())` if a new job can be spawned, or `Err` with the
/// cap-exceeded message if `running_count` is at or above `MAX_JOBS`.
///
/// Pure extraction of the re-check rejection in `spawn()` so the cap logic
/// is unit-testable without spawning real subprocesses. The evict-oldest
/// pass that runs *before* this check mutates the map and is not pure, so
/// it stays inline.
fn check_job_cap(running_count: usize) -> Result<(), String> {
    if running_count >= MAX_JOBS {
        Err(format!(
            "Background job limit ({MAX_JOBS}) reached; wait for jobs to finish or cancel them."
        ))
    } else {
        Ok(())
    }
}

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
    ///
    /// When `sandbox` is `Some`, the same rlimits + (Linux) network namespace
    /// caps + filesystem landlock confinement as the foreground path are
    /// applied to the spawned shell (WO 27.5 R1 / H5 + WO 28.5).
    #[allow(clippy::too_many_arguments)]
    pub async fn spawn(
        &self,
        command: &str,
        workdir: Option<&str>,
        timeout: Option<Duration>,
        deny_list: &DenyList,
        path_guard: &PathGuard,
        bash_sandbox_workdir: bool,
        sandbox: Option<&SandboxConfig>,
        owner: Option<&str>,
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
        // and exceed MAX_JOBS. The insertion itself happens after the spawn
        // below (same lock discipline for the re-check + insert) so a failed
        // spawn cannot leave a Running record with no child behind.
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

        let mut proc = tokio::process::Command::new(shell_program());
        proc.args(["-c", command])
            .kill_on_drop(true)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        // WO 43.28: scrub credential-shaped env vars so a background job
        // cannot exfiltrate provider/session secrets via `printenv`. Mirrors
        // the foreground path (bash_runner/mod.rs). The scheduled-jobs runner
        // (jobs/runner.rs) routes through this same spawn, so this covers
        // background + scheduled in one place.
        crate::session::bash_runner::scrub_secrets_from_child_env(&mut proc);
        setup_process_group(&mut proc);

        // Resolve the working directory to a canonical absolute path before
        // fork: (a) the child's cwd is stable even if the parent's cwd
        // changes after spawn (F15), and (b) the landlock allow-list must
        // match the path the child will actually access (WO 28.5). When
        // workdir is None the child inherits the parent cwd, so we pass
        // that to landlock resolution and leave current_dir unset.
        let canonical_workdir = match workdir {
            Some(wd) => {
                let expanded = shellexpand::tilde(wd);
                let canonical = std::fs::canonicalize(expanded.as_ref())
                    .map_err(|e| anyhow::anyhow!("cannot resolve working directory '{wd}': {e}"))?;
                proc.current_dir(&canonical);
                Some(canonical)
            }
            None => None,
        };

        if let Some(cfg) = sandbox {
            // WO 28.5: landlock FS confinement mirrors the foreground path
            // (bash_runner/mod.rs:494-503). Resolve against the same canonical
            // workspace used for current_dir so the allow-list matches.
            let workspace = canonical_workdir
                .clone()
                .or_else(|| std::env::current_dir().ok())
                .unwrap_or_default();
            #[cfg(target_os = "linux")]
            let lp = crate::session::bash_runner::resolve_paths(&workspace, &[]);
            #[cfg(not(target_os = "linux"))]
            let lp: Option<()> = {
                let _ = workspace;
                None
            };
            setup_rlimits(&mut proc, cfg, lp);
        }

        // Spawn BEFORE inserting the registry record (WO 37.1): any earlier
        // failure (command build, workdir resolution, `?` above) must leave
        // no entry — the old insert-first order leaked a phantom Running
        // job with no child and no watcher that /jobs listed and the cap
        // counted.
        let mut child = proc.spawn()?;

        let pid = child.id();
        let mut job = BashJob::new(id, command.to_string(), owner);
        job.pid = pid;
        {
            let mut jobs = self.jobs.lock().await;
            // Re-check under the same lock before inserting; if another task
            // grabbed the last slot while we spawned, kill the just-spawned
            // child (never leak it) and reject.
            if let Err(e) = check_job_cap(jobs.len()) {
                kill_process_group(&mut child);
                reap_child(&mut child, Duration::from_secs(2)).await;
                return Err(anyhow::anyhow!(e));
            }
            jobs.insert(id, job);
        }

        // Store child handle for cancel(), wrapped so the watcher and
        // cancel()/clean()/remove() can share it. The job's pid is
        // recorded above so cancel can kill the process group without this
        // mutex when the watcher is parked on it (see cancel).
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
                // No child handle in the map — a racing cancel()/remove()
                // already took it and is killing/reaping the child itself
                // (spawn failure no longer reaches the watcher: the record
                // is inserted only after a successful spawn, WO 37.1).
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
            // cleanup. When the drain times out or the join fails, record a
            // marker instead of silently returning empty output (WO 43.8).
            let (stdout_buf, stdout_dropped): (Vec<u8>, u64) = match drain_stdout {
                Some(h) => match tokio::time::timeout(std::time::Duration::from_secs(2), h).await {
                    Ok(Ok(buf)) => buf,
                    Ok(Err(_)) => (b"[drain join error]".to_vec(), 0),
                    Err(_) => (b"[drain timeout]".to_vec(), 0),
                },
                None => (Vec::new(), 0),
            };
            let (stderr_buf, stderr_dropped): (Vec<u8>, u64) = match drain_stderr {
                Some(h) => match tokio::time::timeout(std::time::Duration::from_secs(2), h).await {
                    Ok(Ok(buf)) => buf,
                    Ok(Err(_)) => (b"[drain join error]".to_vec(), 0),
                    Err(_) => (b"[drain timeout]".to_vec(), 0),
                },
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
    /// Flips the status to `Cancelled` and kills the child process group.
    ///
    /// The status flip happens BEFORE the kill: the watcher preserves an
    /// already-`Cancelled` status, so the kill below cannot be overwritten
    /// with `Completed` even if the child exits concurrently. And the kill
    /// does not wait on the child mutex: the watcher parks on it inside
    /// `wait().await` for the job's whole lifetime, so a lock-based kill
    /// would serialize behind the process's natural exit and never fire
    /// for a long-running job. When the mutex is contended, the group is
    /// killed by pid instead; the watcher reaps when `wait()` returns.
    pub async fn cancel(&self, id: u64) -> bool {
        // Flip status first (also the read side for the pid fallback).
        let mut found = false;
        let pid = {
            let mut jobs = self.jobs.lock().await;
            match jobs.get_mut(&id) {
                Some(job) => {
                    if job.status == JobStatus::Running {
                        job.status = JobStatus::Cancelled;
                        job.finished_at = Some(chrono::Local::now());
                        found = true;
                    }
                    job.pid
                }
                None => None,
            }
        };

        // Take the child handle and kill it. The child stays in the map
        // until the watcher has reaped it (F5); try_lock avoids blocking
        // on a watcher parked in wait().
        {
            let child = {
                let mut children = self.children.lock().await;
                children.remove(&id)
            };
            if let Some(child) = child {
                match child.try_lock() {
                    Ok(mut child) => {
                        kill_process_group(&mut child);
                        reap_child(&mut child, Duration::from_secs(2)).await;
                    }
                    Err(_) => {
                        if let Some(pid) = pid {
                            kill_process_group_by_pid(pid);
                        }
                    }
                }
            }
        }

        found
    }

    /// Cancel every still-running job spawned by `owner` (WO 36.2).
    ///
    /// Kills each child exactly like [`cancel`](Self::cancel) does (same
    /// kill/reap/flip path, reused per id) and returns how many jobs were
    /// flipped to `Cancelled`. Jobs with a different owner — including
    /// main-session jobs (`owner: None`) — are never touched; an unknown
    /// owner cancels nothing and returns 0. Owner tags are unique
    /// process-wide (WO 37.1: TaskManager ids come from one global
    /// counter), so a cancel never crosses managers.
    pub async fn cancel_by_owner(&self, owner: &str) -> usize {
        let ids: Vec<u64> = {
            let jobs = self.jobs.lock().await;
            jobs.iter()
                .filter(|(_, j)| {
                    j.status == JobStatus::Running && j.owner.as_deref() == Some(owner)
                })
                .map(|(&id, _)| id)
                .collect()
        };
        let mut cancelled = 0;
        for id in ids {
            if self.cancel(id).await {
                cancelled += 1;
            }
        }
        cancelled
    }

    /// Session-teardown sweep (WO 43.23): persist exit summaries for
    /// still-running jobs (WO 43.10) and then cancel every running job,
    /// so a normal quit cannot leave live orphaned process groups
    /// behind. Called from the TUI shutdown and line-mode exit paths.
    /// Best-effort like the rest of teardown: `cancel` logs, never
    /// panics. Returns how many jobs were cancelled.
    pub async fn sweep_on_session_exit(&self, session_id: &str) -> usize {
        self.persist_exit_summaries(session_id).await;
        let ids: Vec<u64> = {
            let jobs = self.jobs.lock().await;
            jobs.iter()
                .filter(|(_, j)| j.status == JobStatus::Running)
                .map(|(&id, _)| id)
                .collect()
        };
        let mut cancelled = 0;
        for id in ids {
            if self.cancel(id).await {
                cancelled += 1;
            }
        }
        cancelled
    }

    /// Remove a job from the registry, killing a still-running child first.
    ///
    /// Semantics: remove KILLS the child (as it always has) — detaching
    /// would leave a live process with no registry entry: invisible to the
    /// cap, unreachable by cancel, identical to the phantom-job leak.
    ///
    /// Like `cancel`, the kill never waits on the child mutex: the watcher
    /// parks on it inside `wait().await` for the job's whole lifetime, so a
    /// lock-based kill would block remove() until the process exits
    /// naturally. On contention the group is killed by pid instead and the
    /// watcher reaps when `wait()` returns (WO 37.1).
    pub async fn remove(&self, id: u64) -> bool {
        {
            let child = {
                let mut children = self.children.lock().await;
                children.remove(&id)
            };
            if let Some(child) = child {
                match child.try_lock() {
                    Ok(mut child) => {
                        kill_process_group(&mut child);
                        reap_child(&mut child, Duration::from_secs(2)).await;
                    }
                    Err(_) => {
                        // Watcher holds the mutex in wait().await: kill by
                        // pid; the watcher's wait() then returns and reaps.
                        let pid = {
                            let jobs = self.jobs.lock().await;
                            jobs.get(&id).and_then(|j| j.pid)
                        };
                        if let Some(pid) = pid {
                            kill_process_group_by_pid(pid);
                        }
                    }
                }
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

    /// Persist a one-line NDJSON summary for every still-Running job to
    /// `<jobs_dir>/bg-exits.ndjson` so `--resume` can report "these jobs
    /// died with the session" (WO 43.10). Reuses the existing jobs-dir
    /// layout; no new subsystem. Best-effort: errors are logged, not
    /// propagated (teardown must not fail).
    ///
    /// Terminal-state jobs are skipped — only the ones the process is
    /// about to kill get a summary line.
    pub async fn persist_exit_summaries(&self, session_id: &str) {
        let running: Vec<BashJob> = {
            let jobs = self.jobs.lock().await;
            jobs.values()
                .filter(|j| j.status == JobStatus::Running)
                .cloned()
                .collect()
        };
        if running.is_empty() {
            return;
        }
        let dir = match crate::session::jobs_dir() {
            Ok(d) => d,
            Err(e) => {
                tracing::warn!(error = %e, "cannot resolve jobs dir for bg-exit summary");
                return;
            }
        };
        if let Err(e) = std::fs::create_dir_all(&dir) {
            tracing::warn!(error = %e, "cannot create jobs dir for bg-exit summary");
            return;
        }
        let path = dir.join("bg-exits.ndjson");
        let mut file = match std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
        {
            Ok(f) => f,
            Err(e) => {
                tracing::warn!(error = %e, path = %path.display(), "cannot open bg-exits.ndjson");
                return;
            }
        };
        for job in running {
            let summary = BgJobExitSummary {
                id: job.id,
                command: job.command,
                status_at_exit: "died-with-session".to_string(),
                session_id: session_id.to_string(),
                started_at: job.started_at,
            };
            match serde_json::to_string(&summary) {
                Ok(line) => {
                    if let Err(e) = writeln!(file, "{line}") {
                        tracing::warn!(error = %e, "failed to write bg-exit summary line");
                    }
                }
                Err(e) => tracing::warn!(error = %e, "failed to serialize bg-exit summary"),
            }
        }
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

    // Poll a job's status until it reaches a terminal state (Completed,
    // Failed, Cancelled), panicking if it hasn't within `timeout`. Replaces
    // blind wall-clock sleeps that race the watcher under a saturated runtime.
    async fn wait_for_job_done(reg: &BashJobRegistry, id: u64, timeout: Duration) {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let job = reg.get(id).await.unwrap();
            match job.status {
                JobStatus::Completed(_) | JobStatus::Failed(_) | JobStatus::Cancelled => return,
                JobStatus::Running => {}
            }
            if tokio::time::Instant::now() >= deadline {
                panic!("job {id} did not finish within {timeout:?}");
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    // ── check_job_cap: pure cap-rejection logic, no subprocess ──
    // These cover the rejection branch that test_job_cap_enforced_when_all_running
    // exercises only by spawning 64 real sleep 30 children. The cap check itself
    // is a pure HashMap length comparison, so it is unit-testable directly.

    #[test]
    fn check_job_cap_allows_below_max() {
        for n in 0..MAX_JOBS {
            assert!(
                check_job_cap(n).is_ok(),
                "check_job_cap({n}) should allow below MAX_JOBS ({MAX_JOBS})"
            );
        }
    }

    #[test]
    fn check_job_cap_rejects_at_max() {
        let err = check_job_cap(MAX_JOBS).expect_err("check_job_cap(MAX_JOBS) should reject");
        assert!(
            err.contains("Background job limit"),
            "expected cap error, got: {err}"
        );
    }

    #[test]
    fn check_job_cap_rejects_above_max() {
        let err = check_job_cap(100).expect_err("check_job_cap(100) should reject");
        assert!(
            err.contains("Background job limit"),
            "expected cap error, got: {err}"
        );
    }

    #[test]
    fn check_job_cap_error_message_includes_limit() {
        let err = check_job_cap(MAX_JOBS).expect_err("check_job_cap(MAX_JOBS) should reject");
        assert!(
            err.contains(&MAX_JOBS.to_string()),
            "error message should include the limit ({MAX_JOBS}), got: {err}"
        );
    }

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
                None,
                None,
            )
            .await
            .unwrap();
        assert!(id > 0);

        // Wait for completion by polling status, not a blind sleep.
        wait_for_job_done(&reg, id, Duration::from_secs(5)).await;

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
                None,
                None,
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
                None,
                None,
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
                None,
                None,
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
                None,
                None,
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
                None,
                None,
            )
            .await
            .unwrap();
        assert!(reg.remove(id).await);
        assert!(reg.get(id).await.is_none());
    }

    #[tokio::test]
    async fn test_clean_completed_jobs() {
        let reg = BashJobRegistry::new();
        let echo_id = reg
            .spawn(
                "echo a",
                None,
                None,
                &DenyList::default(),
                &PathGuard::default(),
                false,
                None,
                None,
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
                None,
                None,
            )
            .await
            .unwrap();

        // Wait for the echo job to finish before testing clean(), so the
        // only Running job is the sleep 5. Polling is deterministic; a blind
        // sleep races the watcher under a saturated runtime.
        wait_for_job_done(&reg, echo_id, Duration::from_secs(5)).await;

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
                None,
                None,
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

    /// WO 43.28: a secret-shaped env var set in the parent must NOT be
    /// visible inside a background bash job. Without the scrub in
    /// `spawn`, `echo "$VAR"` would surface the value to the model via
    /// `bash_status`. `echo` always exits 0, so the only signal is the
    /// stdout content — empty when the scrub removed the var, the secret
    /// value when it didn't.
    #[cfg(unix)]
    #[tokio::test]
    async fn test_spawn_scrubs_secret_env_var() {
        let _env = crate::shared::test_util::EnvGuard::set(
            "KF_WO43_TEST_API_KEY",
            "sk-should-never-leak-background",
        );
        let reg = BashJobRegistry::new();
        let id = reg
            .spawn(
                "echo \"$KF_WO43_TEST_API_KEY\"",
                None,
                None,
                &DenyList::default(),
                &PathGuard::default(),
                false,
                None,
                None,
            )
            .await
            .expect("spawn should succeed");

        wait_for_job_done(&reg, id, Duration::from_secs(10)).await;

        let job = reg.get(id).await.unwrap();
        assert_eq!(
            job.status,
            JobStatus::Completed(0),
            "echo should exit 0; stderr: {}",
            job.stderr
        );
        assert!(
            !job.stdout.contains("sk-should-never-leak-background"),
            "background job leaked secret env var: stdout was {:?}",
            job.stdout
        );
        let _ = reg.remove(id).await;
    }

    /// Once MAX_JOBS slots are filled with still-running jobs, further spawns
    /// must be rejected instead of growing the registry unboundedly.
    ///
    /// #[ignore]: spawns MAX_JOBS (64) real `sleep 30` subprocesses and then
    /// cancels+reaps each sequentially (~0.9s each), ~58s wall-clock. Genuine
    /// subprocess-management cost, not unnecessary setup. Run explicitly with
    /// `cargo test -- --ignored` when validating the job cap.
    ///
    /// WO 33.14 phase 3 DEFERRED: not replaced with a fake process. The cap
    /// bookkeeping (jobs.len() >= MAX_JOBS, evict-oldest, re-check-under-lock)
    /// is pure HashMap logic. The cap *rejection* check itself is now
    /// unit-tested by `check_job_cap_*` (no subprocess); this stress test
    /// validates the real process lifecycle (spawn 64, cancel 64, reap 64),
    /// NOT the cap check. A fake would need a ProcessSpawner trait abstracting
    /// tokio::process::Child lifecycle across the 96 direct callers of
    /// BashJobRegistry::spawn (CRITICAL blast radius, 18 modules) — that is
    /// the "full fake process framework" WO 33.14 explicitly scoped out as
    /// over-engineering. ponytail: ceiling — the correctness of the cap
    /// rejection is provable without subprocess via `check_job_cap`; this test
    /// guards the real process-management path (spawn/cancel/reap at scale).
    /// Upgrade path: add a ProcessSpawner trait + FakeSpawner if a correctness
    /// regression surfaces that the bookkeeping tests miss; keep the stress
    /// test gated nightly. Tracked in state.md pending.
    #[tokio::test]
    #[ignore = "spawns MAX_JOBS subprocesses; stress test, run with --ignored"]
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
                    None,
                    None,
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
                None,
                None,
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
    #[ignore = "spawns real subprocess + 6s timeout wait; run with --ignored"]
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
                None,
                None,
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
                None,
                None,
            )
            .await
            .unwrap();

        // Wait for pwd to finish by polling status, not a blind sleep.
        wait_for_job_done(&reg, id, Duration::from_secs(5)).await;

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
                    owner: None,
                    pid: None,
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
                    owner: None,
                    pid: None,
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
                None,
                None,
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

    // ── WO 36.2: owner tracking + cancel-by-owner ──

    /// Gate (a): a job spawned with owner X dies (killed + reaped inside
    /// the call, exactly like `cancel`) and its status flips to Cancelled
    /// when cancel_by_owner(X) runs.
    #[tokio::test]
    async fn test_cancel_by_owner_kills_owned_job() {
        let reg = BashJobRegistry::new();
        let id = reg
            .spawn(
                "sleep 30",
                None,
                None,
                &DenyList::default(),
                &PathGuard::default(),
                false,
                None,
                Some("task-7"),
            )
            .await
            .unwrap();
        assert_eq!(
            reg.get(id).await.unwrap().owner.as_deref(),
            Some("task-7"),
            "owner tag must be recorded on the job"
        );

        let cancelled = reg.cancel_by_owner("task-7").await;
        assert_eq!(cancelled, 1, "exactly the owned job should be cancelled");

        let job = reg.get(id).await.unwrap();
        assert_eq!(job.status, JobStatus::Cancelled);
        assert!(job.finished_at.is_some());
    }

    /// Gate (b) + invariant: main-session jobs (owner None) are NEVER
    /// cancelled by a subagent cancel — only the matching owner's job
    /// flips; the None-owner job keeps running untouched.
    #[tokio::test]
    async fn test_cancel_by_owner_never_touches_main_session_jobs() {
        let reg = BashJobRegistry::new();
        let main_id = reg
            .spawn(
                "sleep 30",
                None,
                None,
                &DenyList::default(),
                &PathGuard::default(),
                false,
                None,
                None,
            )
            .await
            .unwrap();
        let sub_id = reg
            .spawn(
                "sleep 30",
                None,
                None,
                &DenyList::default(),
                &PathGuard::default(),
                false,
                None,
                Some("task-9"),
            )
            .await
            .unwrap();

        let cancelled = reg.cancel_by_owner("task-9").await;
        assert_eq!(cancelled, 1);

        let sub = reg.get(sub_id).await.unwrap();
        assert_eq!(sub.status, JobStatus::Cancelled);
        let main_job = reg.get(main_id).await.unwrap();
        assert_eq!(
            main_job.status,
            JobStatus::Running,
            "owner-None (main session) job must survive a subagent cancel"
        );
        assert!(main_job.finished_at.is_none());

        // Cleanup: kill the survivor so the test leaves no 30s child.
        assert!(reg.cancel(main_id).await);
    }

    /// Gate (c): an unknown owner cancels nothing; the count is honest.
    #[tokio::test]
    async fn test_cancel_by_owner_unknown_owner_is_noop() {
        let reg = BashJobRegistry::new();
        let id = reg
            .spawn(
                "sleep 30",
                None,
                None,
                &DenyList::default(),
                &PathGuard::default(),
                false,
                None,
                Some("task-1"),
            )
            .await
            .unwrap();

        assert_eq!(reg.cancel_by_owner("task-999").await, 0);
        assert_eq!(
            reg.get(id).await.unwrap().status,
            JobStatus::Running,
            "unknown owner must not disturb the job"
        );
        assert!(reg.cancel(id).await);
    }

    // ── WO 37.1: bounded remove(), no phantom jobs ──

    /// Gate (b): remove() on a still-running job returns promptly (bounded,
    /// never parked behind the watcher's child mutex) with kill semantics:
    /// the record is gone and the child dies — the watcher only removes the
    /// children-map entry after reaping the killed process.
    #[tokio::test]
    async fn test_remove_running_job_is_bounded_and_kills() {
        let reg = BashJobRegistry::new();
        let id = reg
            .spawn(
                "sleep 30",
                None,
                None,
                &DenyList::default(),
                &PathGuard::default(),
                false,
                None,
                None,
            )
            .await
            .unwrap();

        // Bounded: the mutex-contention path kills by pid (the watcher is
        // parked on the child mutex inside wait().await); the uncontended
        // path still kills + reaps within its 2s reap bound. 3s covers both.
        let removed = tokio::time::timeout(Duration::from_secs(3), reg.remove(id))
            .await
            .expect("remove() on a running job must be bounded, not parked");
        assert!(removed, "remove should return true for a live job");
        assert!(reg.get(id).await.is_none(), "entry must be gone");

        // Kill semantics: poll (bounded) until the watcher has reaped the
        // killed child and dropped its handle — a detached child would keep
        // the children-map entry alive only until natural exit (30s here).
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        loop {
            let has_child = {
                let children = reg.children.lock().await;
                children.contains_key(&id)
            };
            if !has_child {
                break;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "child was not killed+reaped within 5s — remove() detached it"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    /// Gate (c), workdir-resolution failure: an unresolvable workdir errors
    /// AFTER the safety gate but must leave no registry entry (the old
    /// insert-before-spawn order left a phantom Running job).
    #[tokio::test]
    async fn test_spawn_failure_unresolvable_workdir_leaves_no_entry() {
        let reg = BashJobRegistry::new();
        let result = reg
            .spawn(
                "echo hi",
                Some("/nonexistent/kf-wo37-no-such-dir"),
                None,
                &DenyList::default(),
                &PathGuard::default(),
                false,
                None,
                None,
            )
            .await;
        let err = result.expect_err("unresolvable workdir must fail");
        assert!(
            err.to_string().contains("cannot resolve working directory"),
            "expected the canonicalize error, got: {err}"
        );
        assert!(
            reg.list().await.is_empty(),
            "failed spawn must leave no phantom job"
        );
        assert_eq!(reg.running_count().await, 0);
    }

    /// Gate (c), proc.spawn() failure: a workdir that resolves but cannot
    /// be chdir'd into (a regular file) makes the spawn itself fail — no
    /// registry entry survives.
    #[tokio::test]
    async fn test_spawn_failure_bad_workdir_leaves_no_entry() {
        let file = std::env::temp_dir().join(format!("kf-wo37-file-{}", std::process::id()));
        std::fs::write(&file, b"not a directory").unwrap();
        let reg = BashJobRegistry::new();
        let result = reg
            .spawn(
                "echo hi",
                Some(file.to_str().unwrap()),
                None,
                &DenyList::default(),
                &PathGuard::default(),
                false,
                None,
                None,
            )
            .await;
        std::fs::remove_file(&file).ok();
        assert!(
            result.is_err(),
            "spawn into a regular-file workdir must fail, got {result:?}"
        );
        assert!(
            reg.list().await.is_empty(),
            "failed spawn must leave no phantom job"
        );
        assert_eq!(reg.running_count().await, 0);
    }

    // ── WO 43.10: persist_exit_summaries ──

    /// A still-Running job gets a summary line in bg-exits.ndjson; a
    /// Completed job does not. The summary carries the id, command, and
    /// "died-with-session" status so --resume can report what died.
    #[tokio::test]
    async fn test_persist_exit_summaries_writes_running_only() {
        let temp = std::env::temp_dir().join(format!(
            "kf-wo43-10-bg-exit-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _dd = crate::session::DataDirGuard::set(temp.clone());

        let reg = BashJobRegistry::new();
        let running_id = reg
            .spawn(
                "sleep 30",
                None,
                None,
                &DenyList::default(),
                &PathGuard::default(),
                false,
                None,
                None,
            )
            .await
            .unwrap();
        let done_id = reg
            .spawn(
                "echo done",
                None,
                None,
                &DenyList::default(),
                &PathGuard::default(),
                false,
                None,
                None,
            )
            .await
            .unwrap();
        wait_for_job_done(&reg, done_id, Duration::from_secs(5)).await;

        reg.persist_exit_summaries("test-session-1").await;

        let path = crate::session::jobs_dir().unwrap().join("bg-exits.ndjson");
        let contents = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = contents.lines().filter(|l| !l.is_empty()).collect();
        assert_eq!(lines.len(), 1, "only the running job should get a line");

        let summary: BgJobExitSummary = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(summary.id, running_id);
        assert_eq!(summary.command, "sleep 30");
        assert_eq!(summary.status_at_exit, "died-with-session");
        assert_eq!(summary.session_id, "test-session-1");

        // Cleanup: cancel the running job so the test leaves no child.
        reg.cancel(running_id).await;
        crate::shared::test_util::remove_test_dir(&temp);
    }

    /// With no running jobs, persist_exit_summaries writes nothing
    /// (and does not even create the file).
    #[tokio::test]
    async fn test_persist_exit_summaries_noop_when_empty() {
        let temp = std::env::temp_dir().join(format!(
            "kf-wo43-10-bg-empty-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _dd = crate::session::DataDirGuard::set(temp.clone());

        let reg = BashJobRegistry::new();
        reg.persist_exit_summaries("test-session-2").await;

        let path = crate::session::jobs_dir().unwrap().join("bg-exits.ndjson");
        assert!(
            !path.exists(),
            "no running jobs means no file should be created"
        );
        crate::shared::test_util::remove_test_dir(&temp);
    }

    // ── WO 43.23: teardown sweep ──

    /// The exit sweep cancels still-running jobs (child process group
    /// killed — verified structurally via kill(pid, 0) → ESRCH) and
    /// writes the WO 43.10 exit summary for each, so --resume keeps its
    /// "died with the session" report.
    #[tokio::test]
    async fn test_sweep_on_session_exit_cancels_running_jobs() {
        let temp = std::env::temp_dir().join(format!(
            "kf-wo43-23-sweep-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _dd = crate::session::DataDirGuard::set(temp.clone());

        let reg = BashJobRegistry::new();
        let id = reg
            .spawn(
                "sleep 30",
                None,
                None,
                &DenyList::default(),
                &PathGuard::default(),
                false,
                None,
                None,
            )
            .await
            .unwrap();

        let cancelled = reg.sweep_on_session_exit("test-session-43-23").await;
        assert_eq!(cancelled, 1, "the running job should have been cancelled");

        let job = reg.get(id).await.unwrap();
        assert_eq!(job.status, JobStatus::Cancelled);
        let pid = job
            .pid
            .expect("spawned job should have a pid recorded");

        // WO 43.10 preserved: the summary line was written before the kill.
        let path = crate::session::jobs_dir().unwrap().join("bg-exits.ndjson");
        let contents = std::fs::read_to_string(&path).unwrap();
        let summary: BgJobExitSummary = serde_json::from_str(
            contents.lines().find(|l| !l.trim().is_empty()).expect("summary line"),
        )
        .unwrap();
        assert_eq!(summary.id, id);
        assert_eq!(summary.status_at_exit, "died-with-session");

        // The child process group is actually dead.
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        loop {
            let rc = unsafe { libc::kill(pid as i32, 0) };
            let gone = rc == -1 && std::io::Error::last_os_error().raw_os_error() == Some(3); // ESRCH
            if gone {
                break;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "swept job pid {pid} still alive after cancel"
            );
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        crate::shared::test_util::remove_test_dir(&temp);
    }
}
