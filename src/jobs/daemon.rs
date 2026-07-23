//! Scheduler daemon — runs scheduled jobs at their next run time.
//!
//! The daemon is a separate background process (like the session daemon) that:
//! - loads all persisted jobs on startup and recomputes their next run times,
//! - sleeps until the nearest scheduled job,
//! - wakes on `SIGINT`/`SIGTERM`/`SIGHUP` or a socket command (`ping`, `reload`, `shutdown`),
//! - runs due bash jobs (skill jobs are stored but not executed yet),
//! - records stdout/stderr artifacts and updates each job's `last_run` / `next_run`.
//!
//! Communication is line-delimited JSON over `~/.local/share/kirkforge/jobd.sock`.

use crate::daemon::{read_line_limited, Request, Response};
use crate::jobs::runner::run_job;
use crate::jobs::schedule::{compute_next_run, ScheduleSpec, ScheduledJob};
use crate::jobs::store::JobStore;
use crate::session::config::load_config;
use anyhow::{Context, Result};
use chrono::Utc;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::io::AsyncWriteExt;
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{Mutex, Notify, Semaphore};
use tokio::time::{sleep_until, Instant};

/// Run the scheduled-job daemon.
///
/// This is the production entry point. It resolves the canonical socket/pid
/// paths, optionally backgrounds itself, and then runs the event loop.
pub async fn run_job_daemon(foreground: bool, stop: bool) -> Result<()> {
    let socket_path = jobd_socket_path()?;
    let pid_path = jobd_pid_path()?;

    if stop {
        return stop_job_daemon(&socket_path, &pid_path).await;
    }

    if !foreground {
        crate::daemon::daemonize(["jobd", "--foreground"])?;
    }

    run_job_daemon_at(socket_path, pid_path).await
}

/// Run the scheduler event loop on the supplied paths. Public so tests can
/// spin it up in a temporary directory.
pub async fn run_job_daemon_at(socket_path: PathBuf, pid_path: PathBuf) -> Result<()> {
    if let Some(parent) = socket_path.parent() {
        std::fs::create_dir_all(parent).context("create jobs daemon data directory")?;
    }

    // Remove stale socket from a previous crash.
    if let Err(e) = std::fs::remove_file(&socket_path) {
        if e.kind() != std::io::ErrorKind::NotFound {
            tracing::warn!(error = %e, path = %socket_path.display(), "Failed to remove stale jobd socket");
        }
    }

    // Write PID file.
    let pid = std::process::id();
    if let Err(e) = std::fs::write(&pid_path, format!("{pid}\n")) {
        tracing::warn!(error = %e, path = %pid_path.display(), "Failed to write jobd PID file");
    }

    let listener = UnixListener::bind(&socket_path)
        .with_context(|| format!("bind jobd socket at {}", socket_path.display()))?;

    let store = Arc::new(Mutex::new(JobStore::new(crate::session::jobs_dir()?)));
    store.lock().await.ensure_root()?;

    let shutdown = Arc::new(Notify::new());
    let reload = Arc::new(Notify::new());

    // Signal handlers.
    spawn_signal_handlers(shutdown.clone());

    // Socket command handler.
    let socket_shutdown = shutdown.clone();
    let socket_reload = reload.clone();
    tokio::spawn(async move {
        loop {
            let shutdown = socket_shutdown.clone();
            let reload = socket_reload.clone();
            match listener.accept().await {
                Ok((stream, _)) => {
                    tokio::spawn(async move {
                        handle_client(stream, shutdown, reload).await;
                    });
                }
                Err(e) => {
                    tracing::warn!(error = %e, "jobd accept failed");
                }
            }
        }
    });

    // Main scheduling loop. `last_check` is the wall-clock time at which we
    // last evaluated the schedule. Using it as the lower bound lets us catch
    // one-shot jobs whose exact time falls inside the sleep window; using the
    // current `now` as the upper bound prevents stale past jobs from running.
    let mut last_check = Utc::now();
    loop {
        // Load fresh config each pass so live edits take effect.
        let (config, _warning) = load_config();
        let max_concurrent = config.tools.max_concurrent_scheduled_jobs.max(1);
        let semaphore = Arc::new(Semaphore::new(max_concurrent));

        // Reload jobs from disk (picks up new/cancelled jobs written by the TUI).
        let jobs = {
            let store = store.lock().await;
            store.list()
        };

        // Find jobs whose next run falls within [last_check, now].
        let now = Utc::now();
        let due_jobs: Vec<ScheduledJob> = jobs
            .iter()
            .filter(|j| {
                j.enabled && compute_next_run(&j.schedule, last_check).is_some_and(|t| t <= now)
            })
            .cloned()
            .collect();

        if !due_jobs.is_empty() {
            let mut handles = Vec::new();
            for mut job in due_jobs {
                let store = store.clone();
                let sem = semaphore.clone();
                let config = config.clone();
                handles.push(tokio::spawn(async move {
                    let _permit = sem.acquire().await;
                    tracing::info!(job_id = %job.id, "running scheduled job");
                    {
                        let store_guard = store.lock().await;
                        match run_job(&mut job, &store_guard, &config).await {
                            Ok(run) => {
                                tracing::info!(
                                    job_id = %job.id,
                                    status = %run.status.label(),
                                    "scheduled job finished"
                                );
                            }
                            Err(e) => {
                                tracing::error!(job_id = %job.id, error = %e, "scheduled job failed to run");
                            }
                        }
                    }
                    // Recompute next run after execution. For one-shot and
                    // restart jobs, disable after the first execution so they
                    // don't fire repeatedly.
                    let now = Utc::now();
                    job.next_run = compute_next_run(&job.schedule, now);
                    if matches!(job.schedule, ScheduleSpec::Once(_) | ScheduleSpec::Restart) {
                        job.enabled = false;
                    }
                    {
                        let store_guard = store.lock().await;
                        if let Err(e) = store_guard.save(&job) {
                            tracing::error!(job_id = %job.id, error = %e, "failed to save job after run");
                        }
                    }
                }));
            }
            // Wait for this pass to finish before recomputing sleep time.
            for h in handles {
                let _ = h.await;
            }
            last_check = now;
            continue;
        }

        // No due jobs: sleep until the next scheduled time.
        let next_time = jobs
            .iter()
            .filter(|j| j.enabled)
            .filter_map(|j| compute_next_run(&j.schedule, now))
            .min();

        let sleep_deadline = match next_time {
            Some(t) if t > now => {
                let duration = (t - now)
                    .to_std()
                    .unwrap_or(std::time::Duration::from_secs(1));
                Instant::now() + duration
            }
            _ => {
                // No enabled jobs or no future time; sleep for a long while and
                // wake on reload/signal.
                Instant::now() + std::time::Duration::from_secs(3600)
            }
        };

        last_check = now;
        tokio::select! {
            _ = shutdown.notified() => {
                tracing::info!("jobd shutting down gracefully");
                break;
            }
            _ = reload.notified() => {
                tracing::info!("jobd reloading jobs");
                continue;
            }
            _ = sleep_until(sleep_deadline) => {
                continue;
            }
        }
    }

    // Best-effort cleanup.
    let _ = std::fs::remove_file(&socket_path);
    let _ = std::fs::remove_file(&pid_path);
    Ok(())
}

/// Handle one socket client.
async fn handle_client(stream: UnixStream, shutdown: Arc<Notify>, reload: Arc<Notify>) {
    let mut stream = tokio::io::BufStream::new(stream);
    let mut line = String::new();
    loop {
        line.clear();
        match read_line_limited(&mut stream, &mut line).await {
            Ok(0) => break,
            Ok(_) => {}
            Err(e) => {
                tracing::warn!(error = %e, "jobd client read failed");
                break;
            }
        }

        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let req: Request = match serde_json::from_str(trimmed) {
            Ok(r) => r,
            Err(e) => {
                let resp = Response::error(format!("invalid request: {e}"));
                let _ = send_response(&mut stream, &resp).await;
                continue;
            }
        };

        match req {
            Request::Ping => {
                let _ = send_response(&mut stream, &Response::ok_empty()).await;
            }
            Request::Shutdown => {
                let _ = send_response(&mut stream, &Response::ok_empty()).await;
                shutdown.notify_one();
                break;
            }
            // Treat List/Resolve/Touch as reload requests (they are session-daemon
            // requests and don't apply here) so the loop rescans jobs.
            Request::List | Request::Resolve { .. } | Request::Touch { .. } => {
                let _ = send_response(&mut stream, &Response::ok_empty()).await;
                reload.notify_one();
            }
        }
    }
}

async fn send_response(
    stream: &mut tokio::io::BufStream<UnixStream>,
    resp: &Response,
) -> Result<()> {
    let line = serde_json::to_string(resp).context("serialise response")?;
    stream.write_all(line.as_bytes()).await?;
    stream.write_all(b"\n").await?;
    stream.flush().await?;
    Ok(())
}

fn spawn_signal_handlers(shutdown: Arc<Notify>) {
    let ctrl_c_shutdown = shutdown.clone();
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            tracing::info!("jobd received SIGINT; shutting down");
            ctrl_c_shutdown.notify_one();
        }
    });

    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};

        let hup_shutdown = shutdown.clone();
        tokio::spawn(async move {
            match signal(SignalKind::hangup()) {
                Ok(mut hup) => {
                    if hup.recv().await.is_some() {
                        tracing::info!("jobd received SIGHUP; shutting down");
                        hup_shutdown.notify_one();
                    }
                }
                Err(e) => {
                    tracing::warn!(error = %e, "could not install SIGHUP handler");
                }
            }
        });

        let term_shutdown = shutdown.clone();
        tokio::spawn(async move {
            match signal(SignalKind::terminate()) {
                Ok(mut term) => {
                    if term.recv().await.is_some() {
                        tracing::info!("jobd received SIGTERM; shutting down");
                        term_shutdown.notify_one();
                    }
                }
                Err(e) => {
                    tracing::warn!(error = %e, "could not install SIGTERM handler");
                }
            }
        });
    }
}

async fn stop_job_daemon(socket_path: &std::path::Path, pid_path: &std::path::Path) -> Result<()> {
    if !socket_path.exists() {
        anyhow::bail!(
            "jobd socket not found at {} (daemon may not be running)",
            socket_path.display()
        );
    }
    crate::jobs::client::send_shutdown(socket_path).await?;
    let _ = std::fs::remove_file(pid_path);
    Ok(())
}

fn jobd_socket_path() -> Result<PathBuf> {
    Ok(crate::session::jobs_dir()?.join("jobd.sock"))
}

fn jobd_pid_path() -> Result<PathBuf> {
    Ok(crate::session::jobs_dir()?.join("jobd.pid"))
}
