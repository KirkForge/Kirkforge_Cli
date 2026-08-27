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
use crate::session::task_spawner::InProcessTaskSpawner;
use crate::shared::permission::{evaluate, PermissionAction};
use crate::shared::Config;
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
///
/// `parent_run_id` is the canonical run_id of the session that triggered
/// the run (WO 45.1/46.14). `None` when the job was spawned by the daemon
/// with no owning session — see `src/jobs/schedule.rs` `JobRunSummary`.
/// For workflow jobs, it is also threaded into the `WorkflowExecutor` so
/// every `StepOutput` carries the same identity.
pub async fn run_job(
    job: &mut ScheduledJob,
    store: &JobStore,
    config: &Config,
    parent_run_id: Option<String>,
) -> Result<JobRunSummary> {
    let started_at = Utc::now();
    let paths = store
        .create_run(&job.id, started_at)
        .with_context(|| format!("creating run artifacts for scheduled job {}", job.id))?;

    match job.kind.clone() {
        JobKind::Bash { command } => {
            run_bash_job(
                job,
                store,
                config,
                &command,
                started_at,
                paths,
                parent_run_id,
            )
            .await
        }
        JobKind::Workflow { template, vars } => {
            run_workflow_job(
                job,
                store,
                config,
                &template,
                &vars,
                started_at,
                paths,
                parent_run_id,
            )
            .await
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
    parent_run_id: Option<String>,
) -> Result<JobRunSummary> {
    // 1. Permission / approval gate.
    let (deny_list, path_guard, _read_gate) = access_from_config(config);
    let args = serde_json::json!({"command": command});
    let default = if config.tools.scheduled_bash_auto_approve {
        PermissionAction::Allow
    } else {
        PermissionAction::Ask
    };
    match evaluate(&config.security.permission_rules, "bash", &args, default).0 {
        PermissionAction::Deny => {
            return record_failure(
                job,
                store,
                started_at,
                paths,
                "Command denied by permission rules".into(),
                parent_run_id.clone(),
            );
        }
        PermissionAction::Ask => {
            return record_failure(
                job,
                store,
                started_at,
                paths,
                "Command requires interactive approval. Add a permission rule or set scheduled_bash_auto_approve=true to run unattended.".into(),
                parent_run_id.clone(),
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
            parent_run_id.clone(),
        );
    }

    // 3. Execute via the global background registry and wait.
    let registry = global_registry();
    let id = match registry
        .spawn(
            command,
            None,
            job.timeout,
            &deny_list,
            &path_guard,
            config.security.bash_sandbox_workdir,
            Some(&config.security.sandbox),
            // Daemon-scheduled jobs belong to the main session.
            None,
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
                parent_run_id.clone(),
            );
        }
    };

    let poll_interval = Duration::from_millis(250);
    // cancel() flips the registry status to Cancelled BEFORE killing the
    // child; the watcher then completes the record (drained output,
    // finished_at) within its reap (2s) and drain-join (2s) caps.
    // Completed/Failed are set in the same lock acquisition as their
    // output — born final — so only Cancelled needs a settle window
    // before the snapshot is recorded (WO 47.30).
    const CANCEL_SETTLE_WINDOW: Duration = Duration::from_secs(5);
    let mut cancel_seen: Option<tokio::time::Instant> = None;
    loop {
        tokio::time::sleep(poll_interval).await;
        let j = match registry.get(id).await {
            Some(j) => j,
            None => {
                // Job disappeared (e.g. registry evicted it). Record failure.
                return record_failure(
                    job,
                    store,
                    started_at,
                    paths,
                    "Job record disappeared while running".into(),
                    parent_run_id.clone(),
                );
            }
        };
        match j.status {
            JobStatus::Running => continue,
            JobStatus::Cancelled => {
                let seen = cancel_seen.get_or_insert_with(tokio::time::Instant::now);
                if tokio::time::Instant::now() - *seen < CANCEL_SETTLE_WINDOW {
                    continue;
                }
            }
            JobStatus::Completed(_) | JobStatus::Failed(_) => {}
        }

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
            JobStatus::Failed(ref msg) => (RunStatus::Failure, None, format!("Failed: {msg}")),
            JobStatus::Cancelled => (RunStatus::Cancelled, None, "Cancelled".into()),
            JobStatus::Running => unreachable!(),
        };

        write_artifact(&paths.stdout_path, &j.stdout)
            .with_context(|| "writing scheduled job stdout")?;
        write_artifact(&paths.stderr_path, &j.stderr)
            .with_context(|| "writing scheduled job stderr")?;

        let run = JobRunSummary {
            run_id: paths.run_id,
            parent_run_id: parent_run_id.clone(),
            started_at,
            finished_at,
            status,
            exit_code,
            stdout_path: paths.stdout_path,
            stderr_path: paths.stderr_path,
            summary,
        };
        store.record_run(job, &run)?;
        // The terminal entry stays in the registry on purpose: remove()
        // kills a still-live child, racing cancel()'s kill and the
        // watcher's reap (WO 47.30). Terminal entries follow the standard
        // registry lifecycle (MAX_JOBS cap eviction in spawn(), /jobs
        // clean), same as interactive background jobs.
        return Ok(run);
    }
}

// 8 params after WO 46.14 added parent_run_id; grouped refactor tracked
// in WO 45.54 (too_many_arguments audit).
#[allow(clippy::too_many_arguments)]
async fn run_workflow_job(
    job: &mut ScheduledJob,
    store: &JobStore,
    config: &Config,
    template: &str,
    vars: &std::collections::HashMap<String, String>,
    started_at: chrono::DateTime<Utc>,
    paths: RunPaths,
    parent_run_id: Option<String>,
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
                parent_run_id.clone(),
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
                parent_run_id.clone(),
            );
        }
    };

    // 3. Interpolate vars (reuse the same logic as WorkflowTool).
    crate::tools::workflow::interpolate_vars(&mut workflow, vars);

    // 4. Build a StepRunner from an InProcessTaskSpawner (same path as WorkflowTool).
    let shared_cfg: crate::shared::SharedConfig =
        std::sync::Arc::new(std::sync::RwLock::new(config.clone()));
    let spawner: Arc<dyn TaskSpawner> = Arc::new(InProcessTaskSpawner::new(
        shared_cfg,
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
        // WO 47.25: sandbox workflow bash steps + condition evals in
        // unattended jobs too (same config the foreground bash tool uses).
        sandbox_config: config.security.sandbox.clone(),
        landlock_extra_paths: config
            .security
            .landlock_extra_paths
            .iter()
            .map(std::path::PathBuf::from)
            .collect(),
        cancel_token: tokio_util::sync::CancellationToken::new(),
        dry_run: false,
    };

    // 5. Execute with optional timeout (same pattern as run_bash_job).
    let executor = WorkflowExecutor::new(workflow).with_run_id(parent_run_id.clone());
    let cancel_token = runner.cancel_token.clone();
    let run_future = executor.run(std::sync::Arc::new(runner), None);
    let result = match job.timeout {
        Some(dur) => match tokio::time::timeout(dur, run_future).await {
            Ok(r) => r,
            Err(_) => {
                cancel_token.cancel();
                return record_failure(
                    job,
                    store,
                    started_at,
                    paths,
                    format!("Workflow '{}' timed out after {}s", template, dur.as_secs()),
                    parent_run_id.clone(),
                );
            }
        },
        None => run_future.await,
    };
    match result {
        Ok(summary) => {
            let finished_at = Utc::now();
            let stdout_content = crate::tools::workflow::summary_to_json(&summary);
            write_artifact(&paths.stdout_path, &stdout_content)
                .with_context(|| "writing workflow stdout")?;
            write_artifact(&paths.stderr_path, "")
                .with_context(|| "writing empty stderr for successful workflow run")?;
            let run = JobRunSummary {
                run_id: paths.run_id,
                parent_run_id: parent_run_id.clone(),
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
            parent_run_id.clone(),
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
    parent_run_id: Option<String>,
) -> Result<JobRunSummary> {
    let finished_at = Utc::now();
    write_artifact(&paths.stdout_path, "")
        .with_context(|| "writing empty stdout for failed run")?;
    write_artifact(&paths.stderr_path, &message)
        .with_context(|| "writing stderr for failed run")?;
    let run = JobRunSummary {
        run_id: paths.run_id,
        parent_run_id,
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
        let run = run_job(&mut job, &store, &config, None).await.unwrap();
        assert_eq!(run.status, RunStatus::Failure);
        assert!(run.summary.contains("interactive approval"));
    }

    #[tokio::test]
    async fn bash_job_with_auto_approve_succeeds() {
        let (_tmp, store) = tmp_store();
        let mut job = bash_job("echo hello-scheduled");
        let mut config = Config::default();
        config.tools.scheduled_bash_auto_approve = true;
        let run = run_job(&mut job, &store, &config, None).await.unwrap();
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
        let run = run_job(&mut job, &store, &config, None).await.unwrap();
        assert_eq!(run.status, RunStatus::Failure);
        assert!(run.summary.contains("Safety gate") || run.summary.contains("dangerous"));
    }

    /// A bash job with a timeout must not run longer than the timeout.
    /// Regression test for H6: timeout was None even when job.timeout was set.
    #[tokio::test]
    #[ignore = "spawns real sleep 30 subprocess + 2s timeout wait; run with --ignored"]
    async fn bash_job_timeout_is_enforced() {
        let (_tmp, store) = tmp_store();
        let mut job = bash_job("sleep 30");
        job.timeout = Some(std::time::Duration::from_secs(2));
        let mut config = Config::default();
        config.tools.scheduled_bash_auto_approve = true;
        let start = std::time::Instant::now();
        let run = run_job(&mut job, &store, &config, None).await.unwrap();
        let elapsed = start.elapsed();
        assert!(
            elapsed < std::time::Duration::from_secs(10),
            "job should be killed by timeout, ran for {elapsed:?}"
        );
        assert_eq!(run.status, RunStatus::Failure);
        assert!(
            run.summary.to_lowercase().contains("timeout")
                || run.summary.to_lowercase().contains("timed out"),
            "summary should mention timeout: {}",
            run.summary
        );
    }

    // WO 19.5: jobs lifecycle — exit code and stdout are captured correctly.
    #[tokio::test]
    async fn bash_job_captures_exit_code_and_output() {
        let (_tmp, store) = tmp_store();
        let mut job = bash_job("echo lifecycle-test && exit 42");
        let mut config = Config::default();
        config.tools.scheduled_bash_auto_approve = true;
        let run = run_job(&mut job, &store, &config, None).await.unwrap();
        assert_eq!(run.exit_code, Some(42), "exit code should be captured");
        let stdout = std::fs::read_to_string(&run.stdout_path).unwrap();
        assert!(
            stdout.contains("lifecycle-test"),
            "stdout should contain output: {stdout}"
        );
    }

    // WO 47.30: cancel() flips the registry status before the child dies;
    // the watcher completes the record (drained output) a bounded time
    // later. run_bash_job must wait out that window so a cancelled run's
    // artifacts carry the drained output instead of an empty mid-cancel
    // snapshot — and must not remove() the entry (which would kill the
    // child racing the watcher's reap).
    #[tokio::test]
    async fn cancelled_bash_job_records_drained_output() {
        let (dir, store) = tmp_store();
        let marker = "cancel-settle-marker";
        // The sentinel file proves the child already executed the first
        // echo before we cancel — otherwise a loaded machine can kill the
        // child before it writes anything, and the empty stdout is correct
        // rather than a recording race.
        let sentinel = dir.path().join("sentinel");
        let command = format!("echo {marker}; touch {sentinel:?}; sleep 30");
        let mut job = bash_job(&command);
        let mut config = Config::default();
        config.tools.scheduled_bash_auto_approve = true;

        let run_handle =
            tokio::spawn(async move { run_job(&mut job, &store, &config, None).await.unwrap() });

        // Find the spawned registry job and cancel it once it has produced
        // output (sentinel exists).
        let registry = global_registry();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
        let id = 'find: loop {
            if !sentinel.exists() {
                assert!(
                    tokio::time::Instant::now() < deadline,
                    "scheduled job never produced output"
                );
                tokio::time::sleep(Duration::from_millis(20)).await;
                continue;
            }
            for j in registry.list().await {
                if j.command == command && j.status == JobStatus::Running {
                    break 'find j.id;
                }
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "scheduled job never appeared in registry"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        };
        assert!(registry.cancel(id).await, "job should still be running");

        let run = run_handle.await.unwrap();
        assert_eq!(run.status, RunStatus::Cancelled);
        let stdout = std::fs::read_to_string(&run.stdout_path).unwrap();
        assert!(
            stdout.contains(marker),
            "cancelled run lost drained output: {stdout:?}"
        );
    }

    // WO 19.5: workflow job with nonexistent template is rejected.
    #[tokio::test]
    async fn workflow_job_rejects_missing_template() {
        let (_tmp, store) = tmp_store();
        let mut job = ScheduledJob {
            id: "job-wf-missing".into(),
            created_at: Utc::now(),
            schedule: ScheduleSpec::Once(Utc::now()),
            kind: JobKind::Workflow {
                template: "nonexistent_workflow".into(),
                vars: std::collections::HashMap::new(),
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
        };
        let config = Config::default();
        let run = run_job(&mut job, &store, &config, None).await.unwrap();
        assert_eq!(run.status, RunStatus::Failure);
        assert!(
            run.summary.contains("not found"),
            "summary should mention missing template: {}",
            run.summary
        );
    }
}
