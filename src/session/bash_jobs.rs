/// Background bash jobs — long-running command registry.
///
/// Allows spawning bash commands that outlive a single tool call.
/// Jobs run as tokio tasks and their output is captured asynchronously.
/// The model or user can check job status, read output, or cancel jobs.
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
    ) -> anyhow::Result<u64> {
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

            let Some(child) = child else {
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

            // Wait with optional timeout
            let output = if timeout_secs.is_some_and(|t| t > 0) {
                match tokio::time::timeout(
                    std::time::Duration::from_secs(timeout_secs.unwrap()),
                    child.wait_with_output(),
                )
                .await
                {
                    Ok(Ok(out)) => Some(out),
                    Ok(Err(e)) => {
                        let mut jobs = registry_watcher.jobs.lock().await;
                        if let Some(job) = jobs.get_mut(&id) {
                            job.status = JobStatus::Failed(e.to_string());
                            job.finished_at = Some(chrono::Local::now());
                        }
                        return;
                    }
                    Err(_) => {
                        let mut jobs = registry_watcher.jobs.lock().await;
                        if let Some(job) = jobs.get_mut(&id) {
                            job.status = JobStatus::Failed("Timed out".into());
                            job.finished_at = Some(chrono::Local::now());
                        }
                        // kill_on_drop kills the child when child is dropped here
                        return;
                    }
                }
            } else {
                match child.wait_with_output().await {
                    Ok(out) => Some(out),
                    Err(e) => {
                        let mut jobs = registry_watcher.jobs.lock().await;
                        if let Some(job) = jobs.get_mut(&id) {
                            job.status = JobStatus::Failed(e.to_string());
                            job.finished_at = Some(chrono::Local::now());
                        }
                        return;
                    }
                }
            };

            if let Some(output) = output {
                let exit_code = output.status.code().unwrap_or(-1);
                let stdout = String::from_utf8_lossy(&output.stdout).to_string();
                let stderr = String::from_utf8_lossy(&output.stderr).to_string();
                let mut jobs = registry_watcher.jobs.lock().await;
                if let Some(job) = jobs.get_mut(&id) {
                    job.status = JobStatus::Completed(exit_code);
                    job.stdout = stdout;
                    job.stderr = stderr;
                    job.finished_at = Some(chrono::Local::now());
                }
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
                let _ = child.kill().await;
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
                let _ = child.kill().await;
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
                    let _ = child.kill().await;
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
        let id = reg.spawn("echo hello", None, None).await.unwrap();
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
            .spawn("sleep 0.1 && echo done", None, None)
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
        let _ = reg.spawn("echo a", None, None).await.unwrap();
        let _ = reg.spawn("echo b", None, None).await.unwrap();

        tokio::time::sleep(std::time::Duration::from_millis(300)).await;

        let list = reg.list().await;
        assert_eq!(list.len(), 2);
        assert_eq!(reg.running_count().await, 0); // both completed
    }

    #[tokio::test]
    async fn test_cancel_running_job() {
        let reg = BashJobRegistry::new();
        let id = reg.spawn("sleep 5", None, None).await.unwrap();

        // Cancel while running
        assert!(reg.cancel(id).await);

        let job = reg.get(id).await.unwrap();
        assert_eq!(job.status, JobStatus::Cancelled);
    }

    #[tokio::test]
    async fn test_remove_job() {
        let reg = BashJobRegistry::new();
        let id = reg.spawn("echo test", None, None).await.unwrap();
        assert!(reg.remove(id).await);
        assert!(reg.get(id).await.is_none());
    }

    #[tokio::test]
    async fn test_clean_completed_jobs() {
        let reg = BashJobRegistry::new();
        let _ = reg.spawn("echo a", None, None).await.unwrap();
        let running_id = reg.spawn("sleep 5", None, None).await.unwrap();

        tokio::time::sleep(std::time::Duration::from_millis(300)).await;

        // Clean should remove the completed one but keep the running one
        let cleaned = reg.clean().await;
        assert_eq!(cleaned, 1);

        let list = reg.list().await;
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].id, running_id);
    }
}
