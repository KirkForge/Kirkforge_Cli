//! Execute a scheduled job and capture the result.
//!
//! Bash jobs reuse the existing `BashJobRegistry` and `bash_runner` safety
//! gate. Because scheduled jobs run unattended, commands that would normally
//! require interactive approval are rejected unless the user has added a
//! matching permission rule or enabled `scheduled_bash_auto_approve`.
//!
//! Workflow jobs dispatch through `kf_workflow::WorkflowExecutor` using a
//! `TaskSpawnerStepRunner`, the same path as the `WorkflowTool`.

use crate::jobs::schedule::{JobKind, JobRunSummary, RunStatus, ScheduledJob};
use crate::jobs::store::{JobStore, RunPaths};
use crate::session::access::access_from_config;
use crate::session::bash_jobs::{global_registry, JobStatus};
use crate::session::bash_runner::check_bash_command_str;
use crate::shared::permission::{evaluate, PermissionAction};
use crate::shared::Config;
use crate::tools::task::InProcessTaskSpawner;
use crate::tools::task::TaskSpawner;
use crate::tools::workflow::TaskSpawnerStepRunner;
use anyhow::{Context, Result};
use chrono::Utc;
use kf_workflow::WorkflowExecutor;
use std::io::Write;
use std::sync::Arc;
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
        JobKind::Workflow { template, vars } => {
            run_workflow_job(job, store, config, &template, &vars, started_at, paths).await
        }
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
    let default = if config.tools.scheduled_bash_auto_approve {
        PermissionAction::Allow
    } else {
        PermissionAction::Ask
    };
    match evaluate(&config.security.permission_rules, "bash", &args, default) {
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
        config.security.bash_sandbox_workdir,
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
            job.timeout.map(|d| d.as_secs()),
            &deny_list,
            &path_guard,
            config.security.bash_sandbox_workdir,
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

async fn run_workflow_job(
    job: &mut ScheduledJob,
    store: &JobStore,
    config: &Config,
    template: &str,
    vars: &std::collections::HashMap<String, String>,
    started_at: chrono::DateTime<Utc>,
    paths: RunPaths,
) -> Result<JobRunSummary> {
    // 1. Resolve the workflow template file.
    let wf_path = match kf_workflow::find_workflow_file(template) {
        Some(p) => p,
        None => {
            return record_failure(
                job,
                store,
                started_at,
                paths,
                format!("Workflow template '{template}' not found"),
            );
        }
    };

    // 2. Load and validate the workflow.
    let mut workflow = match kf_workflow::Workflow::from_file(&wf_path) {
        Ok(w) => w,
        Err(e) => {
            return record_failure(
                job,
                store,
                started_at,
                paths,
                format!("Failed to load workflow '{template}': {e:#}"),
            );
        }
    };

    // 3. Interpolate vars (reuse the same logic as WorkflowTool).
    crate::tools::workflow::interpolate_vars(&mut workflow, vars);

    // 4. Build a StepRunner from an InProcessTaskSpawner (same path as WorkflowTool).
    let spawner: Arc<dyn TaskSpawner> = Arc::new(InProcessTaskSpawner::new(
        config.clone(),
        config.model.default_model.clone(),
        config.model.ollama_host.clone(),
        None, // no undo stack for scheduled jobs
        config.security.computer_use.enabled,
    ));
    let (deny_list, path_guard, _read_gate) = crate::session::access::access_from_config(config);
    let runner = TaskSpawnerStepRunner {
        spawner,
        toolset: None,
        deny_list,
        path_guard,
        bash_sandbox_workdir: config.security.bash_sandbox_workdir,
        cancel_token: tokio_util::sync::CancellationToken::new(),
        dry_run: false,
    };

    // 5. Execute.
    let executor = WorkflowExecutor::new(workflow);
    match executor.run(std::sync::Arc::new(runner), None).await {
        Ok(summary) => {
            let finished_at = Utc::now();
            let stdout_content = crate::tools::workflow::summary_to_json(&summary);
            write_artifact(&paths.stdout_path, &stdout_content)
                .with_context(|| "writing workflow stdout")?;
            write_artifact(&paths.stderr_path, "")
                .with_context(|| "writing empty stderr for successful workflow run")?;
            let run = JobRunSummary {
                run_id: paths.run_id,
                started_at,
                finished_at,
                status: RunStatus::Success,
                exit_code: Some(0),
                stdout_path: paths.stdout_path,
                stderr_path: paths.stderr_path,
                summary: format!(
                    "Workflow '{}' completed ({} steps)",
                    summary.workflow_name,
                    summary.outputs.len()
                ),
            };
            store.record_run(job, &run)?;
            Ok(run)
        }
        Err(e) => record_failure(
            job,
            store,
            started_at,
            paths,
            format!("Workflow '{template}' failed: {e:#}"),
        ),
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

    // Persist alert so the failure is reviewable later, not just
    // ephemeral in the TUI notification.
    let (kind, cmd) = match &job.kind {
        JobKind::Bash { command } => ("bash", command.as_str()),
        JobKind::Workflow { template, .. } => ("workflow", template.as_str()),
    };
    if let Err(e) = crate::session::session_index::append_alert(&job.id, kind, cmd, &message) {
        tracing::warn!(job_id = %job.id, error = %e, "failed to persist alert for scheduled job failure");
    }

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
            tz: None,
            timeout: None,
            skip_if_empty: false,
            auto_write: false,
            auto_dirs: Vec::new(),
            files: Vec::new(),
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
        let mut config = Config::default();
        config.tools.scheduled_bash_auto_approve = true;
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
        let mut config = Config::default();
        config.tools.scheduled_bash_auto_approve = true;
        let run = run_job(&mut job, &store, &config).await.unwrap();
        assert_eq!(run.status, RunStatus::Failure);
        assert!(run.summary.contains("Safety gate") || run.summary.contains("dangerous"));
    }
}
