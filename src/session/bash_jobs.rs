// Public/future surface in a binary crate: suppress dead-code warnings for pub items.
#![allow(dead_code)]

/// Background bash jobs — long-running command registry.
///
/// Allows spawning bash commands that outlive a single tool call.
/// Jobs run as tokio tasks and their output is captured asynchronously.
/// The model or user can check job status, read output, or cancel jobs.
use crate::session::access::{DenyList, PathGuard};
use crate::session::process_group::{kill_process_group, setup_process_group};
use crate::tools::bash::{cap_to_string, check_bash_command_str, drain_capped, MAX_BASH_OUTPUT_BYTES};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::sync::OnceLock;
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
    /// Child process handles stored separately (Child is not Clone).
    children: Arc<Mutex<HashMap<u64, Child>>>,
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
    /// Optionally accepts a working directory and timeout (seconds, 0 = no timeout).
    ///
    /// The child process handle is stored so that cancel() can kill it.
    /// Completed/failed jobs are evicted oldest-first when the registry
    /// reaches MAX_JOBS (64).
    pub async fn spawn(
        &self,
        command: &str,
        workdir: Option<&str>,
        timeout_secs: Option<u64>,
        deny_list: &DenyList,
        path_guard: &PathGuard,
        bash_sandbox_workdir: bool,
    ) -> anyhow::Result<u64> {
        // Safety gate: every background bash command must pass the same
        // deny-list, dangerous-pattern, and sandbox-workdir checks as
        // foreground bash. Without this, `bash(background: true)` is a
        // trivial bypass around `check_bash_command_str`.
        if let Some(denied) =
            check_bash_command_str(command, workdir, deny_list, path_guard, bash_sandbox_workdir)
        {
            return Err(anyhow::anyhow!(denied));
        }

        let id = self.next_id.fetch_add(1, Ordering::SeqCst);

        // ── Job cap: evict oldest completed jobs if at limit ──
        {
            let mut jobs = self.jobs.lock().await;
            if jobs.len() >= MAX_JOBS {
                let to_remove: Vec<u64> = jobs
                    .iter()
                    .filter(|(_, j)| j.status != JobStatus::Running)
                    .map(|(&id, _)| id)
                    .collect();
                for rid in to_remove {
                    jobs.remove(&rid);
                }
            }
        }

        let job = BashJob::new(id, command.to_string());
        {
            let mut jobs = self.jobs.lock().await;
            jobs.insert(id, job);
        }

        let mut proc = tokio::process::Command::new("sh");
        proc.args(["-c", command])
            .kill_on_drop(true)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        setup_process_group(&mut proc);
        if let Some(ref wd) = workdir {
            proc.current_dir(wd);
        }

        let child = proc.spawn()?;

        // Store child handle for cancel()
        {
            let mut children = self.children.lock().await;
            children.insert(id, child);
        }

        // Spawn watcher: wait for output, update job record, remove child handle
        let registry_watcher = self.clone();
        tokio::spawn(async move {
            let child = {
                let mut children = registry_watcher.children.lock().await;
                children.remove(&id)
            };

            let Some(mut child) = child else {
                // Child handle was taken by cancel() — update status
                let mut jobs = registry_watcher.jobs.lock().await;
                if let Some(job) = jobs.get_mut(&id) {
                    if job.status == JobStatus::Running {
                        job.status = JobStatus::Cancelled;
                    }
                    job.finished_at = Some(chrono::Local::now());
                }
                return;
            };

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
            let status_result: Result<std::process::ExitStatus, String> = if let Some(t) =
                timeout_secs.filter(|t| *t > 0)
            {
                match tokio::time::timeout(std::time::Duration::from_secs(t), child.wait()).await {
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
                let _ = tokio::time::timeout(std::time::Duration::from_secs(2), child.wait()).await;
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

            let mut jobs = registry_watcher.jobs.lock().await;
            if let Some(job) = jobs.get_mut(&id) {
                if let Some(status) = status {
                    job.status = JobStatus::Completed(status.code().unwrap_or(-1));
                } else {
                    job.status =
                        JobStatus::Failed(error_msg.take().unwrap_or_else(|| "Failed".into()));
                }
                job.stdout = cap_to_string(stdout_buf, stdout_dropped);
                job.stderr = cap_to_string(stderr_buf, stderr_dropped);
                job.finished_at = Some(chrono::Local::now());
            }
        });

        Ok(id)
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
        // Take the child handle and kill it
        {
            let mut children = self.children.lock().await;
            if let Some(mut child) = children.remove(&id) {
                kill_process_group(&mut child);
                let _ = child.wait().await;
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
            let mut children = self.children.lock().await;
            if let Some(mut child) = children.remove(&id) {
                kill_process_group(&mut child);
                let _ = child.wait().await;
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
                if let Some(mut child) = children.remove(id) {
                    kill_process_group(&mut child);
                    let _ = child.wait().await;
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
        let id = reg.spawn("echo hello", None, None, &DenyList::default(), &PathGuard::default(), false).await.unwrap();
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
            .spawn("sleep 0.1 && echo done", None, None, &DenyList::default(), &PathGuard::default(), false)
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
        let _ = reg.spawn("echo a", None, None, &DenyList::default(), &PathGuard::default(), false).await.unwrap();
        let _ = reg.spawn("echo b", None, None, &DenyList::default(), &PathGuard::default(), false).await.unwrap();

        tokio::time::sleep(std::time::Duration::from_millis(300)).await;

        let list = reg.list().await;
        assert_eq!(list.len(), 2);
        assert_eq!(reg.running_count().await, 0); // both completed
    }

    #[tokio::test]
    async fn test_cancel_running_job() {
        let reg = BashJobRegistry::new();
        let id = reg.spawn("sleep 5", None, None, &DenyList::default(), &PathGuard::default(), false).await.unwrap();

        // Cancel while running
        assert!(reg.cancel(id).await);

        let job = reg.get(id).await.unwrap();
        assert_eq!(job.status, JobStatus::Cancelled);
    }

    #[tokio::test]
    async fn test_remove_job() {
        let reg = BashJobRegistry::new();
        let id = reg.spawn("echo test", None, None, &DenyList::default(), &PathGuard::default(), false).await.unwrap();
        assert!(reg.remove(id).await);
        assert!(reg.get(id).await.is_none());
    }

    #[tokio::test]
    async fn test_clean_completed_jobs() {
        let reg = BashJobRegistry::new();
        let _ = reg.spawn("echo a", None, None, &DenyList::default(), &PathGuard::default(), false).await.unwrap();
        let running_id = reg.spawn("sleep 5", None, None, &DenyList::default(), &PathGuard::default(), false).await.unwrap();

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
            .spawn("rm -rf /", None, None, &DenyList::default(), &PathGuard::default(), false)
            .await;
        assert!(
            result.is_err(),
            "dangerous background command should be rejected, got {:?}",
            result
        );
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("dangerous pattern"),
            "expected dangerous-pattern denial, got: {}",
            err
        );
    }

    /// A job that exceeds its timeout is killed, reaped, and still retains
    /// the partial output it produced before the timeout.
    #[tokio::test]
    async fn test_timeout_reaps_child_and_preserves_partial_output() {
        let reg = BashJobRegistry::new();
        let id = reg
            .spawn("echo partial; sleep 30", None, Some(1), &DenyList::default(), &PathGuard::default(), false)
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
}
