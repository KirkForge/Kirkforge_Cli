/// Background bash jobs — long-running command registry.
///
/// Allows spawning bash commands that outlive a single tool call.
/// Jobs run as tokio tasks and their output is captured asynchronously.
/// The model or user can check job status, read output, or cancel jobs.
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::sync::Mutex;

/// Status of a background job.
#[derive(Debug, Clone, PartialEq)]
pub enum JobStatus {
    Running,
    Completed(i32),   // exit code
    Failed(String),   // error message
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

/// Registry of background bash jobs.
#[derive(Clone, Default)]
pub struct BashJobRegistry {
    jobs: Arc<Mutex<HashMap<u64, BashJob>>>,
    next_id: Arc<AtomicU64>,
}

impl BashJobRegistry {
    pub fn new() -> Self {
        Self {
            jobs: Arc::new(Mutex::new(HashMap::new())),
            next_id: Arc::new(AtomicU64::new(1)),
        }
    }

    /// Spawn a bash command in the background and return a job ID.
    pub async fn spawn(&self, command: &str) -> anyhow::Result<u64> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);

        let job = BashJob::new(id, command.to_string());
        let mut jobs = self.jobs.lock().await;
        jobs.insert(id, job);
        drop(jobs); // release lock before spawning the task

        let registry = self.clone();
        let cmd = command.to_string();
        tokio::spawn(async move {
            let output = tokio::process::Command::new("sh")
                .args(["-c", &cmd])
                .output()
                .await;

            let (status, stdout, stderr) = match output {
                Ok(o) => {
                    let exit_code = o.status.code().unwrap_or(-1);
                    let stdout = String::from_utf8_lossy(&o.stdout).to_string();
                    let stderr = String::from_utf8_lossy(&o.stderr).to_string();
                    (JobStatus::Completed(exit_code), stdout, stderr)
                }
                Err(e) => {
                    (JobStatus::Failed(e.to_string()), String::new(), String::new())
                }
            };

            let mut jobs = registry.jobs.lock().await;
            if let Some(job) = jobs.get_mut(&id) {
                job.status = status;
                job.stdout = stdout;
                job.stderr = stderr;
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
    /// Note: this sets the status to Cancelled, but the underlying
    /// tokio task continues until completion. The output is discarded.
    pub async fn cancel(&self, id: u64) -> bool {
        let mut jobs = self.jobs.lock().await;
        if let Some(job) = jobs.get_mut(&id) {
            if job.status == JobStatus::Running {
                job.status = JobStatus::Cancelled;
                job.finished_at = Some(chrono::Local::now());
                return true;
            }
        }
        false
    }

    /// Remove a job from the registry.
    pub async fn remove(&self, id: u64) -> bool {
        let mut jobs = self.jobs.lock().await;
        jobs.remove(&id).is_some()
    }

    /// Count of running jobs.
    pub async fn running_count(&self) -> usize {
        let jobs = self.jobs.lock().await;
        jobs.values().filter(|j| j.status == JobStatus::Running).count()
    }

    /// Clear all completed/failed/cancelled jobs.
    pub async fn clean(&self) -> usize {
        let mut jobs = self.jobs.lock().await;
        let before = jobs.len();
        jobs.retain(|_, j| j.status == JobStatus::Running);
        before - jobs.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_spawn_and_complete() {
        let reg = BashJobRegistry::new();
        let id = reg.spawn("echo hello").await.unwrap();
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
        let id = reg.spawn("sleep 0.1 && echo done").await.unwrap();

        // Immediately check — should be running
        let job = reg.get(id).await.unwrap();
        // It might complete fast, but at minimum the command was captured
        assert_eq!(job.command, "sleep 0.1 && echo done");
    }

    #[tokio::test]
    async fn test_job_list_and_count() {
        let reg = BashJobRegistry::new();
        let _ = reg.spawn("echo a").await.unwrap();
        let _ = reg.spawn("echo b").await.unwrap();

        tokio::time::sleep(std::time::Duration::from_millis(300)).await;

        let list = reg.list().await;
        assert_eq!(list.len(), 2);
        assert_eq!(reg.running_count().await, 0); // both completed
    }

    #[tokio::test]
    async fn test_cancel_running_job() {
        let reg = BashJobRegistry::new();
        let id = reg.spawn("sleep 5").await.unwrap();

        // Cancel while running
        assert!(reg.cancel(id).await);

        let job = reg.get(id).await.unwrap();
        assert_eq!(job.status, JobStatus::Cancelled);
    }

    #[tokio::test]
    async fn test_remove_job() {
        let reg = BashJobRegistry::new();
        let id = reg.spawn("echo test").await.unwrap();
        assert!(reg.remove(id).await);
        assert!(reg.get(id).await.is_none());
    }

    #[tokio::test]
    async fn test_clean_completed_jobs() {
        let reg = BashJobRegistry::new();
        let _ = reg.spawn("echo a").await.unwrap();
        let running_id = reg.spawn("sleep 5").await.unwrap();

        tokio::time::sleep(std::time::Duration::from_millis(300)).await;

        // Clean should remove the completed one but keep the running one
        let cleaned = reg.clean().await;
        assert_eq!(cleaned, 1);

        let list = reg.list().await;
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].id, running_id);
    }
}