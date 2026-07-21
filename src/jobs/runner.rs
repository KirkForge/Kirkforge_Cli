//! Execute a scheduled job and capture the result.
//!
//! Bash jobs reuse the existing `BashJobRegistry` and `bash_runner` safety
//! gate. Because scheduled jobs run unattended, commands that would normally
//! require interactive approval are rejected unless the user has added a
//! matching permission rule or enabled `scheduled_bash_auto_approve`.
//!
//! Skill jobs are accepted by the data model but are intentionally not
//! executable yet; attempting to run one records a clear failure.

use crate::jobs::schedule::{JobKind, JobRunSummary, RunStatus, ScheduledJob};
use crate::jobs::store::{JobStore, RunPaths};
use crate::session::access::access_from_config;
use crate::session::bash_jobs::{global_registry, JobStatus};
use crate::session::bash_runner::check_bash_command_str;
use crate::shared::permission::{evaluate, PermissionAction};
use crate::shared::Config;
use anyhow::{Context, Result};
use chrono::Utc;
use std::io::Write;
use std::time::Duration;

/// Run a single scheduled job, recording its stdout/stderr artifacts and
/// returning a [`JobRunSummary`].
pub async fn run_job(
    job: &mut ScheduledJob,
    store: &JobStore,
    config: &Config,
) -> Result<JobRunSummary> {
    let started_at = Utc::now();
    let paths = store
        .create_run(&job.id, started_at)
        .with_context(|| format!("creating run artifacts for scheduled job {}", job.id))?;

    match job.kind.clone() {
        JobKind::Bash { command } => {
            run_bash_job(job, store, config, &command, started_at, paths).await
        }
        JobKind::Skill { name, .. } => record_failure(
            job,
            store,
            started_at,
            paths,
            format!(
                "Skill scheduled jobs are not yet implemented (skill={name}). Run with `/jobs run-now` once supported."
            ),
        ),
    }
}

async fn run_bash_job(
    job: &mut ScheduledJob,
    store: &JobStore,
    config: &Config,
    command: &str,
    started_at: chrono::DateTime<Utc>,
    paths: RunPaths,
) -> Result<JobRunSummary> {
    // 1. Permission / approval gate.
    let (deny_list, path_guard, _read_gate) = access_from_config(config);
    let args = serde_json::json!({"command": command});
    let default = if config.scheduled_bash_auto_approve {
        PermissionAction::Allow
    } else {
        PermissionAction::Ask
    };
    match evaluate(&config.permission_rules, "bash", &args, default) {
        PermissionAction::Deny => {
            return record_failure(
                job,
                store,
                started_at,
                paths,
                "Command denied by permission rules".into(),
            );
        }
        PermissionAction::Ask => {
            return record_failure(
                job,
                store,
                started_at,
                paths,
                "Command requires interactive approval. Add a permission rule or set scheduled_bash_auto_approve=true to run unattended.".into(),
            );
        }
        PermissionAction::Allow => {}
    }

    // 2. Safety gate (dangerous patterns, deny-list, sandbox workdir).
    if let Some(denied) = check_bash_command_str(
        command,
        None,
        &deny_list,
        &path_guard,
        config.bash_sandbox_workdir,
    ) {
        return record_failure(
            job,
            store,
            started_at,
            paths,
            format!("Safety gate blocked scheduled bash job: {denied}"),
        );
    }

    // 3. Execute via the global background registry and wait.
    let registry = global_registry();
    let id = match registry
        .spawn(
            command,
            None,
            None, // no timeout for scheduled jobs
            &deny_list,
            &path_guard,
            config.bash_sandbox_workdir,
        )
        .await
    {
        Ok(id) => id,
        Err(e) => {
            return record_failure(
                job,
                store,
                started_at,
                paths,
                format!("Failed to spawn scheduled bash job: {e:#}"),
            );
        }
    };

    let poll_interval = Duration::from_millis(250);
    loop {
        tokio::time::sleep(poll_interval).await;
        match registry.get(id).await {
            Some(j) if j.status != JobStatus::Running => {
                let finished_at = Utc::now();
                let (status, exit_code, summary) = match j.status {
                    JobStatus::Completed(code) => {
                        let summary = if code == 0 {
                            "Completed successfully".into()
                        } else {
                            format!("Completed with exit code {code}")
                        };
                        (RunStatus::Success, Some(code), summary)
                    }
                    JobStatus::Failed(ref msg) => {
                        (RunStatus::Failure, None, format!("Failed: {msg}"))
                    }
                    JobStatus::Cancelled => (RunStatus::Cancelled, None, "Cancelled".into()),
                    JobStatus::Running => unreachable!(),
                };

                write_artifact(&paths.stdout_path, &j.stdout)
                    .with_context(|| "writing scheduled job stdout")?;
                write_artifact(&paths.stderr_path, &j.stderr)
                    .with_context(|| "writing scheduled job stderr")?;

                let run = JobRunSummary {
                    run_id: paths.run_id,
                    started_at,
                    finished_at,
                    status,
                    exit_code,
                    stdout_path: paths.stdout_path,
                    stderr_path: paths.stderr_path,
                    summary,
                };
                store.record_run(job, &run)?;
                let _ = registry.remove(id).await;
                return Ok(run);
            }
            None => {
                // Job disappeared (e.g. registry evicted it). Record failure.
                let run = record_failure(
                    job,
                    store,
                    started_at,
                    paths,
                    "Job record disappeared while running".into(),
                )?;
                let _ = registry.remove(id).await;
                return Ok(run);
            }
            _ => continue,
        }
    }
}

fn write_artifact(path: &std::path::Path, content: &str) -> Result<()> {
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(path)
        .with_context(|| format!("opening artifact {}", path.display()))?;
    file.write_all(content.as_bytes())
        .with_context(|| format!("writing artifact {}", path.display()))?;
    Ok(())
}

fn record_failure(
    job: &mut ScheduledJob,
    store: &JobStore,
    started_at: chrono::DateTime<Utc>,
    paths: RunPaths,
    message: String,
) -> Result<JobRunSummary> {
    let finished_at = Utc::now();
    write_artifact(&paths.stdout_path, "")
        .with_context(|| "writing empty stdout for failed run")?;
    write_artifact(&paths.stderr_path, &message)
        .with_context(|| "writing stderr for failed run")?;
    let run = JobRunSummary {
        run_id: paths.run_id,
        started_at,
        finished_at,
        status: RunStatus::Failure,
        exit_code: None,
        stdout_path: paths.stdout_path,
        stderr_path: paths.stderr_path,
        summary: message.clone(),
    };
    store.record_run(job, &run)?;
    Ok(run)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jobs::schedule::{RunStatus, ScheduleSpec};
    use crate::jobs::store::JobStore;
    use crate::shared::Config;

    fn tmp_store() -> (tempfile::TempDir, JobStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = JobStore::new(dir.path().to_path_buf());
        store.ensure_root().unwrap();
        (dir, store)
    }

    fn bash_job(command: &str) -> ScheduledJob {
        ScheduledJob {
            id: "job-test-001".into(),
            created_at: Utc::now(),
            schedule: ScheduleSpec::Once(Utc::now()),
            kind: JobKind::Bash {
                command: command.into(),
            },
            enabled: true,
            last_run: None,
            next_run: None,
        }
    }

    #[tokio::test]
    async fn bash_job_without_approval_fails() {
        let (_tmp, store) = tmp_store();
        let mut job = bash_job("echo hi");
        let config = Config::default();
        let run = run_job(&mut job, &store, &config).await.unwrap();
        assert_eq!(run.status, RunStatus::Failure);
        assert!(run.summary.contains("interactive approval"));
    }

    #[tokio::test]
    async fn bash_job_with_auto_approve_succeeds() {
        let (_tmp, store) = tmp_store();
        let mut job = bash_job("echo hello-scheduled");
        let config = Config {
            scheduled_bash_auto_approve: true,
            seed: None,
            ..Default::default()
        };
        let run = run_job(&mut job, &store, &config).await.unwrap();
        assert_eq!(run.status, RunStatus::Success);
        assert_eq!(run.exit_code, Some(0));
        let stdout = std::fs::read_to_string(&run.stdout_path).unwrap();
        assert!(stdout.contains("hello-scheduled"));
    }

    #[tokio::test]
    async fn dangerous_bash_job_rejected_even_with_auto_approve() {
        let (_tmp, store) = tmp_store();
        let mut job = bash_job("rm -rf /");
        let config = Config {
            scheduled_bash_auto_approve: true,
            seed: None,
            ..Default::default()
        };
        let run = run_job(&mut job, &store, &config).await.unwrap();
        assert_eq!(run.status, RunStatus::Failure);
        assert!(run.summary.contains("Safety gate") || run.summary.contains("dangerous"));
    }

    #[tokio::test]
    async fn skill_job_records_not_implemented() {
        let (_tmp, store) = tmp_store();
        let mut job = ScheduledJob {
            id: "job-test-002".into(),
            created_at: Utc::now(),
            schedule: ScheduleSpec::Once(Utc::now()),
            kind: JobKind::Skill {
                name: "summarize-prs".into(),
                args: vec![],
            },
            enabled: true,
            last_run: None,
            next_run: None,
        };
        let config = Config::default();
        let run = run_job(&mut job, &store, &config).await.unwrap();
        assert_eq!(run.status, RunStatus::Failure);
        assert!(run.summary.contains("not yet implemented"));
    }
}
