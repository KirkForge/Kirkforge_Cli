//! Persistent, append-only store for scheduled jobs.
//!
//! Each job lives in its own directory under `~/.local/share/kirkforge/jobs/`:
//! - `job.json` — the current [`ScheduledJob`] record.
//! - `runs/` — one directory per run with `run.json`, `stdout`, and `stderr`.
//!
//! `job.json` is written atomically (temp file + rename). Run records are
//! append-only files. All artifacts are created with `0o600` permissions.

use crate::jobs::schedule::{JobRunSummary, ScheduledJob};
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

/// On-disk store for scheduled jobs.
#[derive(Debug, Clone)]
pub struct JobStore {
    root: PathBuf,
}

impl JobStore {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    /// Root directory containing all job directories.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Ensure the jobs root exists and is private.
    pub fn ensure_root(&self) -> Result<()> {
        if !self.root.exists() {
            fs::create_dir_all(&self.root)
                .with_context(|| format!("creating jobs directory {}", self.root.display()))?;
        }
        #[cfg(unix)]
        {
            let perms = fs::Permissions::from_mode(0o700);
            fs::set_permissions(&self.root, perms)
                .with_context(|| format!("setting permissions on {}", self.root.display()))?;
        }
        Ok(())
    }

    fn job_dir(&self, id: &str) -> PathBuf {
        self.root.join(id)
    }

    fn job_path(&self, id: &str) -> PathBuf {
        self.job_dir(id).join("job.json")
    }

    fn runs_dir(&self, id: &str) -> PathBuf {
        self.job_dir(id).join("runs")
    }

    /// Load a single job by id. Returns `Ok(None)` if the job directory does
    /// not exist.
    pub fn load(&self, id: &str) -> Result<Option<ScheduledJob>> {
        let path = self.job_path(id);
        if !path.exists() {
            return Ok(None);
        }
        let bytes =
            fs::read(&path).with_context(|| format!("reading job file {}", path.display()))?;
        let job: ScheduledJob = serde_json::from_slice(&bytes)
            .with_context(|| format!("parsing job file {}", path.display()))?;
        Ok(Some(job))
    }

    /// Save a job atomically, creating its directory if needed.
    pub fn save(&self, job: &ScheduledJob) -> Result<()> {
        let dir = self.job_dir(&job.id);
        if !dir.exists() {
            fs::create_dir_all(&dir)
                .with_context(|| format!("creating job directory {}", dir.display()))?;
            #[cfg(unix)]
            {
                let perms = fs::Permissions::from_mode(0o700);
                fs::set_permissions(&dir, perms)
                    .with_context(|| format!("setting permissions on {}", dir.display()))?;
            }
        }

        let path = self.job_path(&job.id);
        let temp = path.with_extension("json.tmp");
        {
            let file = fs::File::create(&temp)
                .with_context(|| format!("creating temporary job file {}", temp.display()))?;
            #[cfg(unix)]
            {
                let perms = fs::Permissions::from_mode(0o600);
                fs::set_permissions(&temp, perms)
                    .with_context(|| format!("setting permissions on {}", temp.display()))?;
            }
            serde_json::to_writer_pretty(file, job)
                .with_context(|| format!("serializing job {}", job.id))?;
        }
        fs::rename(&temp, &path).with_context(|| {
            format!(
                "renaming temporary job file {} to {}",
                temp.display(),
                path.display()
            )
        })?;
        Ok(())
    }

    /// List all jobs on disk, skipping entries whose `job.json` cannot be
    /// parsed. Returns jobs in creation order (lexicographic by id).
    pub fn list(&self) -> Vec<ScheduledJob> {
        let mut jobs = Vec::new();
        if let Ok(entries) = fs::read_dir(&self.root) {
            for entry in entries.flatten() {
                let name = entry.file_name();
                let id = name.to_string_lossy().to_string();
                if let Ok(Some(job)) = self.load(&id) {
                    jobs.push(job);
                }
            }
        }
        jobs.sort_by(|a, b| a.id.cmp(&b.id));
        jobs
    }

    /// Delete a job and all its run artifacts. Returns `true` if it existed.
    pub fn delete(&self, id: &str) -> Result<bool> {
        let dir = self.job_dir(id);
        if !dir.exists() {
            return Ok(false);
        }
        fs::remove_dir_all(&dir)
            .with_context(|| format!("removing job directory {}", dir.display()))?;
        Ok(true)
    }

    /// Allocate a new run directory and artifact paths for a job. Returns the
    /// run id, run directory, stdout path, and stderr path. The run directory
    /// is created with `0o700` and the artifact files with `0o600`.
    pub fn create_run(&self, job_id: &str, started_at: DateTime<Utc>) -> Result<RunPaths> {
        let runs_dir = self.runs_dir(job_id);
        if !runs_dir.exists() {
            fs::create_dir_all(&runs_dir)
                .with_context(|| format!("creating runs directory {}", runs_dir.display()))?;
            #[cfg(unix)]
            {
                let perms = fs::Permissions::from_mode(0o700);
                fs::set_permissions(&runs_dir, perms)
                    .with_context(|| format!("setting permissions on {}", runs_dir.display()))?;
            }
        }

        let run_id = format!("run-{}", started_at.timestamp());
        let run_dir = runs_dir.join(&run_id);
        fs::create_dir_all(&run_dir)
            .with_context(|| format!("creating run directory {}", run_dir.display()))?;
        #[cfg(unix)]
        {
            let perms = fs::Permissions::from_mode(0o700);
            fs::set_permissions(&run_dir, perms)
                .with_context(|| format!("setting permissions on {}", run_dir.display()))?;
        }

        let stdout_path = run_dir.join("stdout");
        let stderr_path = run_dir.join("stderr");
        let summary_path = run_dir.join("run.json");
        create_private_file(&stdout_path)?;
        create_private_file(&stderr_path)?;
        create_private_file(&summary_path)?;

        Ok(RunPaths {
            run_id,
            run_dir,
            stdout_path,
            stderr_path,
            summary_path,
        })
    }

    /// Append a run summary to the job's `runs/` directory and update the job's
    /// `last_run` field. The job is re-saved atomically.
    pub fn record_run(&self, job: &mut ScheduledJob, run: &JobRunSummary) -> Result<()> {
        let runs_dir = self.runs_dir(&job.id);
        let summary_path = runs_dir.join(&run.run_id).join("run.json");
        let temp = summary_path.with_extension("tmp");
        {
            let file = fs::File::create(&temp)
                .with_context(|| format!("creating temporary run summary {}", temp.display()))?;
            #[cfg(unix)]
            {
                let perms = fs::Permissions::from_mode(0o600);
                fs::set_permissions(&temp, perms)
                    .with_context(|| format!("setting permissions on {}", temp.display()))?;
            }
            serde_json::to_writer_pretty(file, run)
                .with_context(|| format!("serializing run {}", run.run_id))?;
        }
        fs::rename(&temp, &summary_path).with_context(|| {
            format!(
                "renaming temporary run summary {} to {}",
                temp.display(),
                summary_path.display()
            )
        })?;

        job.last_run = Some(run.clone());
        self.save(job)?;
        Ok(())
    }

    /// Load all run summaries for a job, newest first.
    pub fn list_runs(&self, job_id: &str) -> Vec<JobRunSummary> {
        let runs_dir = self.runs_dir(job_id);
        let mut runs = Vec::new();
        if let Ok(entries) = fs::read_dir(runs_dir) {
            for entry in entries.flatten() {
                let summary = entry.path().join("run.json");
                if let Ok(bytes) = fs::read(&summary) {
                    if let Ok(run) = serde_json::from_slice::<JobRunSummary>(&bytes) {
                        runs.push(run);
                    }
                }
            }
        }
        runs.sort_by(|a, b| b.started_at.cmp(&a.started_at));
        runs
    }
}

/// Paths returned when a new run is created.
#[derive(Debug, Clone)]
pub struct RunPaths {
    pub run_id: String,
    pub run_dir: PathBuf,
    pub stdout_path: PathBuf,
    pub stderr_path: PathBuf,
    pub summary_path: PathBuf,
}

#[cfg(unix)]
fn create_private_file(path: &Path) -> Result<()> {
    let mut file = fs::File::create(path)
        .with_context(|| format!("creating artifact file {}", path.display()))?;
    file.write_all(b"")
        .with_context(|| format!("initialising artifact file {}", path.display()))?;
    let perms = fs::Permissions::from_mode(0o600);
    fs::set_permissions(path, perms)
        .with_context(|| format!("setting permissions on {}", path.display()))?;
    Ok(())
}

#[cfg(not(unix))]
fn create_private_file(path: &Path) -> Result<()> {
    let mut file = fs::File::create(path)
        .with_context(|| format!("creating artifact file {}", path.display()))?;
    file.write_all(b"")
        .with_context(|| format!("initialising artifact file {}", path.display()))?;
    Ok(())
}

/// A serialisable, disk-friendly summary of a scheduled job used for the
/// store's top-level list. This mirrors most of [`ScheduledJob`] but omits
/// internal state not needed for quick listing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobListEntry {
    pub id: String,
    pub created_at: DateTime<Utc>,
    pub schedule_kind: String,
    pub kind: String,
    pub enabled: bool,
    pub next_run: Option<DateTime<Utc>>,
    pub last_run_status: Option<String>,
}

impl From<&ScheduledJob> for JobListEntry {
    fn from(job: &ScheduledJob) -> Self {
        Self {
            id: job.id.clone(),
            created_at: job.created_at,
            schedule_kind: match job.schedule {
                crate::jobs::schedule::ScheduleSpec::Cron(_) => "cron".into(),
                crate::jobs::schedule::ScheduleSpec::Once(_) => "once".into(),
                crate::jobs::schedule::ScheduleSpec::Restart => "restart".into(),
            },
            kind: match job.kind {
                crate::jobs::schedule::JobKind::Bash { .. } => "bash".into(),
                crate::jobs::schedule::JobKind::Skill { .. } => "skill".into(),
            },
            enabled: job.enabled,
            next_run: job.next_run,
            last_run_status: job.last_run.as_ref().map(|r| r.status.label().to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jobs::schedule::{JobKind, ScheduleSpec};
    use chrono::Duration;

    fn tmp_store() -> (tempfile::TempDir, JobStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = JobStore::new(dir.path().to_path_buf());
        store.ensure_root().unwrap();
        (dir, store)
    }

    fn sample_job(id: &str) -> ScheduledJob {
        ScheduledJob {
            id: id.into(),
            created_at: Utc::now(),
            schedule: ScheduleSpec::Once(Utc::now() + Duration::hours(1)),
            kind: JobKind::Bash {
                command: "echo hi".into(),
            },
            enabled: true,
            last_run: None,
            next_run: None,
        }
    }

    #[test]
    fn save_and_load_round_trip() {
        let (_tmp, store) = tmp_store();
        let job = sample_job("job-20260716-001");
        store.save(&job).unwrap();
        let loaded = store.load("job-20260716-001").unwrap().unwrap();
        assert_eq!(loaded.id, job.id);
        assert_eq!(loaded.kind, job.kind);
        assert!(loaded.enabled);
    }

    #[test]
    fn load_missing_returns_none() {
        let (_tmp, store) = tmp_store();
        assert!(store.load("nope").unwrap().is_none());
    }

    #[test]
    fn list_sorted_and_skips_garbage() {
        let (_tmp, store) = tmp_store();
        store.save(&sample_job("job-20260716-001")).unwrap();
        store.save(&sample_job("job-20260716-002")).unwrap();
        // A directory with no job.json should be ignored.
        fs::create_dir_all(store.job_dir("job-20260716-XXX")).unwrap();
        let list = store.list();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].id, "job-20260716-001");
        assert_eq!(list[1].id, "job-20260716-002");
    }

    #[test]
    fn delete_removes_job() {
        let (_tmp, store) = tmp_store();
        store.save(&sample_job("job-20260716-001")).unwrap();
        assert!(store.delete("job-20260716-001").unwrap());
        assert!(store.load("job-20260716-001").unwrap().is_none());
        assert!(!store.delete("job-20260716-001").unwrap());
    }

    #[test]
    fn create_run_makes_private_files() {
        let (_tmp, store) = tmp_store();
        let job = sample_job("job-20260716-001");
        store.save(&job).unwrap();
        let paths = store.create_run(&job.id, Utc::now()).unwrap();
        assert!(paths.stdout_path.exists());
        assert!(paths.stderr_path.exists());
        assert!(paths.run_dir.exists());

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let meta = fs::metadata(&paths.stdout_path).unwrap();
            assert_eq!(meta.permissions().mode() & 0o777, 0o600);
        }
    }

    #[test]
    fn record_run_updates_last_run_and_lists() {
        let (_tmp, store) = tmp_store();
        let mut job = sample_job("job-20260716-001");
        store.save(&job).unwrap();

        let paths = store.create_run(&job.id, Utc::now()).unwrap();
        let run = JobRunSummary {
            run_id: paths.run_id.clone(),
            started_at: Utc::now(),
            finished_at: Utc::now(),
            status: crate::jobs::schedule::RunStatus::Success,
            exit_code: Some(0),
            stdout_path: paths.stdout_path,
            stderr_path: paths.stderr_path,
            summary: "ok".into(),
        };
        store.record_run(&mut job, &run).unwrap();

        let loaded = store.load(&job.id).unwrap().unwrap();
        assert!(loaded.last_run.is_some());
        let runs = store.list_runs(&job.id);
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].run_id, paths.run_id);
    }
}
