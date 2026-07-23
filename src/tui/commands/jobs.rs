//! `/jobs` slash-command handler and the background-job / scheduled-job
//! completion notifiers.
//!
//! The TUI's event loop calls `notify_completed_jobs` and
//! `notify_completed_scheduled_jobs` on every tick. Both are bounded by the
//! number of jobs/runs, not the number of turns.
//!
//! Background-bash sub-commands (unchanged):
//! - `/jobs`              → list all background jobs (status + command)
//! - `/jobs <id>`         → show detail for a single job
//! - `/jobs <id> cancel`  → cancel a running job
//! - `/jobs clean`        → drop all finished jobs
//!
//! Scheduled-job sub-commands (new):
//! - `/jobs schedule <spec> bash <command>`  → create a bash scheduled job
//! - `/jobs schedule <spec> skill <name> [args...]` → create a skill job
//! - `/jobs scheduled list`                    → list scheduled jobs
//! - `/jobs scheduled cancel <id>`             → disable a scheduled job
//! - `/jobs run-now <id>`                      → run a scheduled job now
//! - `/jobs logs <id>`                         → tail the last run's logs

use crate::jobs::runner::run_job;
use crate::jobs::schedule::{
    compute_next_run, display_cron, generate_job_id, parse_schedule, JobKind, RunStatus,
    ScheduleSpec, ScheduledJob,
};
use crate::jobs::store::JobStore;
use crate::tui::app::AppState;
use chrono::Utc;

/// Maximum number of stdout/stderr lines shown when inspecting a single
/// job via `/jobs <id>` or `/jobs logs <id>`. Long-running builds can
/// produce thousands of lines; the user usually wants the tail (the actual
/// error or final status), so we keep the LAST `JOB_DETAIL_TAIL_LINES` lines
/// and indicate how many were elided.
pub const JOB_DETAIL_TAIL_LINES: usize = 50;

/// Format a `BashJob`'s status as a single short string.
///
/// Centralised so the list view (`/jobs`), the detail view
/// (`/jobs <id>`), and the completion notifier in
/// `notify_completed_jobs` all render the status the same way. Earlier
/// versions had inconsistent formatting (Running used `(id=5)`, the
/// others used `#5`); the new format always uses `#id` for consistency.
pub fn format_job_status(job: &crate::session::bash_jobs::BashJob) -> String {
    match &job.status {
        crate::session::bash_jobs::JobStatus::Running => {
            format!("⏳ running #{}", job.id)
        }
        crate::session::bash_jobs::JobStatus::Completed(code) => {
            format!("✅ completed #{} (exit {})", job.id, code)
        }
        crate::session::bash_jobs::JobStatus::Failed(e) => {
            format!("❌ failed #{}: {}", job.id, e)
        }
        crate::session::bash_jobs::JobStatus::Cancelled => {
            format!("🚫 cancelled #{}", job.id)
        }
    }
}

/// Take the LAST `n` lines of `s` and report how many lines were elided
/// from the head. Returns `(tail, elided_count)`. Empty input returns
/// `("", 0)` — guards against panics in `lines()` and the `>` check.
///
/// `pub` so the unit tests in `commands/mod.rs` can exercise the
/// boundary cases directly without going through the `/jobs` handler.
pub fn tail_lines(s: &str, n: usize) -> (String, usize) {
    if s.is_empty() {
        return (String::new(), 0);
    }
    let all: Vec<&str> = s.lines().collect();
    if all.len() <= n {
        return (s.to_string(), 0);
    }
    let skip = all.len() - n;
    let tail = all[skip..].join("\n");
    (tail, skip)
}

/// Split an input string into tokens, respecting double quotes so that
/// cron expressions and multi-word commands can be written naturally:
///
///   `/jobs schedule "0 9 * * 1-5" bash "cargo test"`
///
/// becomes `["0 9 * * 1-5", "bash", "cargo test"]`.
fn split_quoted(input: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut in_quotes = false;

    for ch in input.chars() {
        if ch == '"' {
            in_quotes = !in_quotes;
        } else if ch.is_whitespace() && !in_quotes {
            if !current.is_empty() {
                tokens.push(std::mem::take(&mut current));
            }
        } else {
            current.push(ch);
        }
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    tokens
}

/// Format a [`ScheduleSpec`] for human-readable display.
fn format_schedule(schedule: &ScheduleSpec) -> String {
    match schedule {
        ScheduleSpec::Cron(expr) => display_cron(expr),
        ScheduleSpec::Once(t) => t.to_rfc3339(),
        ScheduleSpec::Restart => "@restart".into(),
    }
}

/// Format a [`JobKind`] as a short description.
fn format_kind(kind: &JobKind) -> String {
    match kind {
        JobKind::Bash { command } => format!("bash: {command}"),
        JobKind::Skill { name, args } => {
            if args.is_empty() {
                format!("skill: {name}")
            } else {
                format!("skill: {name} {}", args.join(" "))
            }
        }
    }
}

/// Build or refresh a [`JobStore`] rooted under the canonical jobs directory.
fn job_store() -> anyhow::Result<JobStore> {
    let root = crate::session::jobs_dir()?;
    let store = JobStore::new(root);
    store.ensure_root()?;
    Ok(store)
}

/// Handle `/jobs` command.
pub async fn handle_jobs_command(args: &str, state: &mut AppState) -> String {
    let trimmed = args.trim();
    let first = trimmed
        .split_whitespace()
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();

    match first.as_str() {
        "schedule" => handle_schedule_command(trimmed, state).await,
        "scheduled" => handle_scheduled_command(trimmed, state).await,
        "run-now" => handle_run_now_command(trimmed, state).await,
        "logs" => handle_logs_command(trimmed).await,
        _ => handle_background_jobs_command(args).await,
    }
}

// ── Background-bash commands (existing surface, unchanged) ───────────────

async fn handle_background_jobs_command(args: &str) -> String {
    let trimmed = args.trim();

    // `/jobs clean` — drop finished jobs
    if trimmed.eq_ignore_ascii_case("clean") {
        let registry = crate::session::bash_jobs::global_registry();
        let cleaned = registry.clean().await;
        return if cleaned == 0 {
            "No completed jobs to clean.".into()
        } else if cleaned == 1 {
            "🧹 Cleaned 1 finished job.".into()
        } else {
            format!("🧹 Cleaned {cleaned} finished jobs.")
        };
    }

    // `/jobs <id> [cancel]` — split into first and second tokens.
    // The first token must parse as u64; the second (if present)
    // must be "cancel" (case-insensitive). Anything else returns
    // a usage hint.
    if !trimmed.is_empty() {
        let mut tokens = trimmed.split_whitespace();
        let first = tokens.next().unwrap_or(""); // safe: trimmed non-empty
        let second = tokens.next();

        // Anything past the second token is rejected (avoid silent
        // typos like `/jobs 5 cancel now`).
        if let Some(extra) = tokens.next() {
            return format!(
                "Usage: /jobs [clean | <id> | <id> cancel]\nGot: /jobs {} {} {}",
                first,
                second.unwrap_or(""),
                extra
            );
        }

        let id: u64 = match first.parse() {
            Ok(n) => n,
            Err(_) => {
                return format!("Usage: /jobs [clean | <id> | <id> cancel]\nGot: /jobs {first}");
            }
        };

        // `/jobs <id> cancel` — cancel a running job
        if let Some(sub) = second {
            if !sub.eq_ignore_ascii_case("cancel") {
                return format!("Usage: /jobs [clean | <id> | <id> cancel]\nGot: /jobs {id} {sub}");
            }
            let registry = crate::session::bash_jobs::global_registry();
            return match registry.cancel(id).await {
                true => format!(
                    "🚫 Cancellation requested for job #{id}. The completion notifier will post the final status."
                ),
                false => match registry.get(id).await {
                    Some(job) => format!(
                        "Job #{} is not running (status: {}). Nothing to cancel.",
                        id,
                        format_job_status(&job)
                    ),
                    None => format!("Job #{id} not found. No jobs to cancel."),
                },
            };
        }

        // `/jobs <id>` — show detail for one job
        let registry = crate::session::bash_jobs::global_registry();
        match registry.get(id).await {
            Some(job) => {
                let mut out = String::new();
                out.push_str(&format!("{}\n", format_job_status(&job)));
                out.push_str(&format!("  Command:  {}\n", job.command));
                out.push_str(&format!(
                    "  Started:  {}\n",
                    job.started_at.format("%Y-%m-%d %H:%M:%S")
                ));
                if let Some(f) = job.finished_at {
                    out.push_str(&format!("  Finished: {}\n", f.format("%Y-%m-%d %H:%M:%S")));
                }

                // Stdout
                if !job.stdout.is_empty() {
                    let (tail, elided) = tail_lines(&job.stdout, JOB_DETAIL_TAIL_LINES);
                    out.push_str(&format!(
                        "\n  --- stdout ({} bytes) ---\n",
                        job.stdout.len()
                    ));
                    if elided > 0 {
                        out.push_str(&format!(
                            "  [... {elided} lines elided, showing last {JOB_DETAIL_TAIL_LINES} ...]\n"
                        ));
                    }
                    for line in tail.lines() {
                        out.push_str(&format!("  {line}\n"));
                    }
                } else {
                    out.push_str("\n  --- stdout (empty) ---\n");
                }

                // Stderr
                if !job.stderr.is_empty() {
                    let (tail, elided) = tail_lines(&job.stderr, JOB_DETAIL_TAIL_LINES);
                    out.push_str(&format!(
                        "\n  --- stderr ({} bytes) ---\n",
                        job.stderr.len()
                    ));
                    if elided > 0 {
                        out.push_str(&format!(
                            "  [... {elided} lines elided, showing last {JOB_DETAIL_TAIL_LINES} ...]\n"
                        ));
                    }
                    for line in tail.lines() {
                        out.push_str(&format!("  {line}\n"));
                    }
                } else {
                    out.push_str("\n  --- stderr (empty) ---\n");
                }

                // Strip the trailing newline
                out.pop();
                out
            }
            None => {
                let registry = crate::session::bash_jobs::global_registry();
                let ids: Vec<String> = registry
                    .list()
                    .await
                    .iter()
                    .map(|j| j.id.to_string())
                    .collect();
                if ids.is_empty() {
                    return format!("Job #{id} not found. No background jobs exist.");
                }
                format!(
                    "Job #{} not found. Available jobs: [{}]",
                    id,
                    ids.join(", ")
                )
            }
        }
    } else {
        // `/jobs` — list
        let registry = crate::session::bash_jobs::global_registry();
        let jobs = registry.list().await;
        if jobs.is_empty() {
            return "No background jobs.".into();
        }
        let mut out = "Background jobs:\n".to_string();
        for job in &jobs {
            out.push_str(&format!("  {} — {}\n", format_job_status(job), job.command));
        }
        out.push_str(
            "\nTip: /jobs <id> for detail, /jobs <id> cancel to stop a running job, /jobs clean to drop finished jobs.\n",
        );
        out
    }
}

// ── Scheduled-job commands ───────────────────────────────────────────────

async fn handle_schedule_command(args: &str, _state: &mut AppState) -> String {
    let without_prefix = args.strip_prefix("schedule").unwrap_or(args).trim();
    let tokens = split_quoted(without_prefix);
    if tokens.is_empty() {
        return "Usage: /jobs schedule <spec> bash <command>  or  /jobs schedule <spec> skill <name> [args...]".into();
    }

    // Find the "bash" / "skill" keyword that separates the schedule from the payload.
    let split_idx = tokens
        .iter()
        .position(|t| t.eq_ignore_ascii_case("bash") || t.eq_ignore_ascii_case("skill"));
    let Some(idx) = split_idx else {
        return "Usage: /jobs schedule <spec> bash <command>  or  /jobs schedule <spec> skill <name> [args...]\nCould not find 'bash' or 'skill' keyword.".into();
    };

    let schedule_expr = tokens[..idx].join(" ");
    let kind_keyword = tokens[idx].to_ascii_lowercase();
    let rest = &tokens[idx + 1..];

    let schedule = match parse_schedule(&schedule_expr) {
        Ok(s) => s,
        Err(e) => return format!("Invalid schedule '{schedule_expr}': {e:#}"),
    };

    let kind = match kind_keyword.as_str() {
        "bash" => {
            if rest.is_empty() {
                return "Usage: /jobs schedule <spec> bash <command>".into();
            }
            JobKind::Bash {
                command: rest.join(" "),
            }
        }
        "skill" => {
            if rest.is_empty() {
                return "Usage: /jobs schedule <spec> skill <name> [args...]".into();
            }
            JobKind::Skill {
                name: rest[0].clone(),
                args: rest[1..].to_vec(),
            }
        }
        _ => unreachable!(),
    };

    let store = match job_store() {
        Ok(s) => s,
        Err(e) => return format!("Cannot open job store: {e:#}"),
    };

    let id = match generate_job_id(store.root()) {
        Ok(id) => id,
        Err(e) => return format!("Cannot generate job id: {e:#}"),
    };

    let now = Utc::now();
    let next_run = compute_next_run(&schedule, now);
    let job = ScheduledJob {
        id: id.clone(),
        created_at: now,
        schedule,
        kind,
        enabled: true,
        last_run: None,
        next_run,
    };

    if let Err(e) = store.save(&job) {
        return format!("Failed to save scheduled job: {e:#}");
    }

    format!(
        "📅 Scheduled job created: {id}\n  Schedule: {}\n  Kind: {}\n  Next run: {}",
        format_schedule(&job.schedule),
        format_kind(&job.kind),
        job.next_run
            .map(|t| t.to_rfc3339())
            .unwrap_or_else(|| "never (past one-shot or disabled)".into())
    )
}

async fn handle_scheduled_command(args: &str, _state: &mut AppState) -> String {
    let without_prefix = args.strip_prefix("scheduled").unwrap_or(args).trim();
    let tokens: Vec<&str> = without_prefix.split_whitespace().collect();

    if tokens.is_empty() || tokens[0].eq_ignore_ascii_case("list") {
        return handle_scheduled_list().await;
    }

    if tokens[0].eq_ignore_ascii_case("cancel") {
        let id = tokens.get(1).unwrap_or(&"");
        if id.is_empty() {
            return "Usage: /jobs scheduled cancel <id>".into();
        }
        if tokens.len() > 2 {
            return format!(
                "Usage: /jobs scheduled cancel <id>\nGot: /jobs scheduled cancel {}",
                tokens[1..].join(" ")
            );
        }
        return handle_scheduled_cancel(id).await;
    }

    format!(
        "Usage: /jobs scheduled list | cancel <id>\nGot: /jobs scheduled {}",
        tokens.join(" ")
    )
}

async fn handle_scheduled_list() -> String {
    let store = match job_store() {
        Ok(s) => s,
        Err(e) => return format!("Cannot open job store: {e:#}"),
    };

    let jobs = store.list();
    if jobs.is_empty() {
        return "No scheduled jobs.".into();
    }

    let mut out = "Scheduled jobs:\n".to_string();
    for job in &jobs {
        let status = if job.enabled { "enabled" } else { "disabled" };
        let next = job
            .next_run
            .as_ref()
            .map(|t| t.to_rfc3339())
            .unwrap_or_else(|| "—".into());
        let last = job
            .last_run
            .as_ref()
            .map(|r| format!("{} ({})", r.status.label(), r.summary))
            .unwrap_or_else(|| "—".into());
        out.push_str(&format!(
            "  {} [{}] {} | {} | next: {} | last: {}\n",
            job.id,
            status,
            format_schedule(&job.schedule),
            format_kind(&job.kind),
            next,
            last
        ));
    }
    out.push_str("\nTip: /jobs run-now <id> to trigger, /jobs logs <id> for output, /jobs scheduled cancel <id> to disable.\n");
    out
}

async fn handle_scheduled_cancel(id: &str) -> String {
    let store = match job_store() {
        Ok(s) => s,
        Err(e) => return format!("Cannot open job store: {e:#}"),
    };

    match store.load(id) {
        Ok(Some(mut job)) => {
            job.enabled = false;
            job.next_run = None;
            match store.save(&job) {
                Ok(_) => format!("🚫 Scheduled job {id} disabled."),
                Err(e) => format!("Failed to disable scheduled job {id}: {e:#}"),
            }
        }
        Ok(None) => format!("Scheduled job {id} not found."),
        Err(e) => format!("Failed to load scheduled job {id}: {e:#}"),
    }
}

async fn handle_run_now_command(args: &str, state: &mut AppState) -> String {
    let without_prefix = args.strip_prefix("run-now").unwrap_or(args).trim();
    let tokens: Vec<&str> = without_prefix.split_whitespace().collect();
    if tokens.is_empty() {
        return "Usage: /jobs run-now <id>".into();
    }
    if tokens.len() > 1 {
        return format!(
            "Usage: /jobs run-now <id>\nGot: /jobs run-now {}",
            tokens.join(" ")
        );
    }
    let id = tokens[0];

    let store = match job_store() {
        Ok(s) => s,
        Err(e) => return format!("Cannot open job store: {e:#}"),
    };

    let mut job = match store.load(id) {
        Ok(Some(j)) => j,
        Ok(None) => return format!("Scheduled job {id} not found."),
        Err(e) => return format!("Failed to load scheduled job {id}: {e:#}"),
    };

    let config = state.config.read().unwrap().clone();
    match run_job(&mut job, &store, &config).await {
        Ok(run) => format!(
            "▶️ Scheduled job {id} ran now: {} — {} (exit {:?})\n  stdout: {}\n  stderr: {}",
            run.status.label(),
            run.summary,
            run.exit_code,
            run.stdout_path.display(),
            run.stderr_path.display()
        ),
        Err(e) => format!("Scheduled job {id} failed to run: {e:#}"),
    }
}

async fn handle_logs_command(args: &str) -> String {
    let without_prefix = args.strip_prefix("logs").unwrap_or(args).trim();
    let tokens: Vec<&str> = without_prefix.split_whitespace().collect();
    if tokens.is_empty() {
        return "Usage: /jobs logs <id>".into();
    }
    if tokens.len() > 1 {
        return format!(
            "Usage: /jobs logs <id>\nGot: /jobs logs {}",
            tokens.join(" ")
        );
    }
    let id = tokens[0];

    let store = match job_store() {
        Ok(s) => s,
        Err(e) => return format!("Cannot open job store: {e:#}"),
    };

    if let Err(e) = store.load(id) {
        return format!("Failed to load scheduled job {id}: {e:#}");
    }

    let runs = store.list_runs(id);
    if runs.is_empty() {
        return format!("Scheduled job {id} has no recorded runs yet.");
    }

    let run = &runs[0];
    let mut out = format!(
        "Logs for scheduled job {id} — run {} at {}:\n  Status: {} — {} (exit {:?})\n",
        run.run_id,
        run.started_at.format("%Y-%m-%d %H:%M:%S UTC"),
        run.status.label(),
        run.summary,
        run.exit_code
    );

    for (label, path) in [("stdout", &run.stdout_path), ("stderr", &run.stderr_path)] {
        match std::fs::read_to_string(path) {
            Ok(content) => {
                let (tail, elided) = tail_lines(&content, JOB_DETAIL_TAIL_LINES);
                out.push_str(&format!("\n  --- {label} ({} bytes) ---\n", content.len()));
                if elided > 0 {
                    out.push_str(&format!(
                        "  [... {elided} lines elided, showing last {JOB_DETAIL_TAIL_LINES} ...]\n"
                    ));
                }
                if tail.is_empty() {
                    out.push_str("  (empty)\n");
                } else {
                    for line in tail.lines() {
                        out.push_str(&format!("  {line}\n"));
                    }
                }
            }
            Err(e) => {
                out.push_str(&format!("\n  --- {label} ---\n  (could not read: {e})\n"));
            }
        }
    }

    out.push_str(&format!(
        "\n  Artifact paths:\n    stdout: {}\n    stderr: {}\n    summary: {}",
        run.stdout_path.display(),
        run.stderr_path.display(),
        store
            .list_runs(id)
            .first()
            .map(|r| r.run_id.clone())
            .unwrap_or_default()
    ));
    out
}

/// Walk the global `BashJob` registry and push a one-time chat
/// notification for any job that has just finished. Idempotent: each
/// job is notified exactly once thanks to `state.notified_jobs`.
///
/// Called by the TUI event loop on every tick. Cheap O(n) in the
/// number of jobs; not O(n) in the number of turns.
///
/// Returns `true` if any job was newly notified (so `state` was
/// mutated). Callers use this to set the frame-pacing dirty flag
/// instead of re-checking the registry themselves.
pub async fn notify_completed_jobs(state: &mut AppState) -> bool {
    let mut any = false;
    let registry = crate::session::bash_jobs::global_registry();
    let jobs = registry.list().await;
    for job in &jobs {
        let finished = match job.status {
            crate::session::bash_jobs::JobStatus::Completed(_)
            | crate::session::bash_jobs::JobStatus::Failed(_)
            | crate::session::bash_jobs::JobStatus::Cancelled => true,
            crate::session::bash_jobs::JobStatus::Running => false,
        };
        if finished && state.notified_jobs.insert(job.id) {
            // First time seeing this job as finished — push a notification
            let status_icon = match &job.status {
                crate::session::bash_jobs::JobStatus::Completed(code) => {
                    format!("✅ Job #{} completed (exit {})", job.id, code)
                }
                crate::session::bash_jobs::JobStatus::Failed(e) => {
                    format!("❌ Job #{} failed: {}", job.id, e)
                }
                crate::session::bash_jobs::JobStatus::Cancelled => {
                    format!("🚫 Job #{} cancelled", job.id)
                }
                _ => continue,
            };
            state
                .messages
                .push_back(crate::tui::app::ConversationEntry::new(
                    "system",
                    format!("{} — `{}`", status_icon, job.command),
                ));
            any = true;
        }
    }
    // Prune notified_jobs to IDs still in the registry. Once the registry
    // evicts a finished job its ID never reappears, so the HashSet would grow
    // for the lifetime of the session without this.
    let live_ids: std::collections::HashSet<u64> = jobs.iter().map(|j| j.id).collect();
    state.notified_jobs.retain(|id| live_ids.contains(id));
    any
}

/// Poll the persistent scheduled-job store and push a one-time chat
/// notification for every run that has finished since the last poll.
///
/// Tracks run IDs (not job IDs) so recurring cron jobs announce each
/// run exactly once.
///
/// Returns `true` if any new notification was pushed.
pub async fn notify_completed_scheduled_jobs(state: &mut AppState) -> bool {
    let store = match job_store() {
        Ok(s) => s,
        Err(_) => return false,
    };

    let jobs = store.list();
    let mut any = false;
    let mut live_run_ids = std::collections::HashSet::new();

    for job in &jobs {
        if let Some(run) = &job.last_run {
            live_run_ids.insert(run.run_id.clone());
            if state.notified_scheduled_runs.insert(run.run_id.clone()) {
                let icon = match run.status {
                    RunStatus::Success => "✅",
                    RunStatus::Failure => "❌",
                    RunStatus::Cancelled => "🚫",
                };
                state
                    .messages
                    .push_back(crate::tui::app::ConversationEntry::new(
                        "system",
                        format!(
                            "{} Scheduled job {} finished: {} — {} (exit {:?})",
                            icon,
                            job.id,
                            run.status.label(),
                            run.summary,
                            run.exit_code
                        ),
                    ));
                any = true;
            }
        }
    }

    // Prune run IDs that no longer correspond to a stored last_run.
    state
        .notified_scheduled_runs
        .retain(|id| live_run_ids.contains(id));
    any
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shared::Config;
    use crate::tui::app::AppState;
    use std::sync::{Arc, OnceLock, RwLock};

    fn scheduled_test_lock() -> &'static tokio::sync::Mutex<()> {
        static LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
    }

    fn state_with_config(config: Config) -> AppState {
        AppState::new(Arc::new(RwLock::new(config)))
    }

    fn state() -> AppState {
        state_with_config(Config::default())
    }

    fn state_auto_approve() -> AppState {
        let mut cfg = Config::default();
        cfg.tools.scheduled_bash_auto_approve = true;
        state_with_config(cfg)
    }

    fn tmp_jobs_dir() -> (tempfile::TempDir, JobStore) {
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("KIRKFORGE_DATA_DIR", dir.path().as_os_str());
        let store = job_store().unwrap();
        (dir, store)
    }

    #[tokio::test]
    async fn schedule_bash_job_parses_and_saves() {
        let _guard = scheduled_test_lock().lock().await;
        let (_tmp, store) = tmp_jobs_dir();
        let mut state = state();

        let out = handle_jobs_command(
            "schedule @once 2099-07-20T09:00:00 bash echo hello",
            &mut state,
        )
        .await;
        assert!(out.starts_with("📅 Scheduled job created:"), "got: {out}");

        let jobs = store.list();
        assert_eq!(jobs.len(), 1);
        assert!(matches!(jobs[0].kind, JobKind::Bash { .. }));
        assert!(jobs[0].enabled);
    }

    #[tokio::test]
    async fn schedule_skill_job_parses_and_saves() {
        let _guard = scheduled_test_lock().lock().await;
        let (_tmp, store) = tmp_jobs_dir();
        let mut state = state();

        let out = handle_jobs_command(
            "schedule @daily skill summarize-prs owner=KirkForge",
            &mut state,
        )
        .await;
        assert!(out.starts_with("📅 Scheduled job created:"), "got: {out}");

        let jobs = store.list();
        assert_eq!(jobs.len(), 1);
        assert!(
            matches!(
                &jobs[0].kind,
                JobKind::Skill {
                    name,
                    args,
                } if name == "summarize-prs" && *args == ["owner=KirkForge"]
            ),
            "got kind {:?}",
            jobs[0].kind
        );
    }

    #[tokio::test]
    async fn scheduled_list_and_cancel() {
        let _guard = scheduled_test_lock().lock().await;
        let (_tmp, store) = tmp_jobs_dir();
        let mut state = state();

        let out = handle_jobs_command("schedule @hourly bash echo hourly", &mut state).await;
        let id = out
            .lines()
            .next()
            .unwrap()
            .strip_prefix("📅 Scheduled job created: ")
            .unwrap()
            .trim()
            .to_string();

        let list = handle_jobs_command("scheduled list", &mut state).await;
        assert!(list.contains(&id), "list missing job: {list}");
        assert!(list.contains("hourly"), "list missing command: {list}");

        let cancel = handle_jobs_command(&format!("scheduled cancel {id}"), &mut state).await;
        assert!(cancel.contains("disabled"), "got: {cancel}");

        let job = store.load(&id).unwrap().unwrap();
        assert!(!job.enabled);
        assert!(job.next_run.is_none());
    }

    #[tokio::test]
    async fn run_now_and_logs_and_notifier() {
        let _guard = scheduled_test_lock().lock().await;
        let (_tmp, _store) = tmp_jobs_dir();
        let mut state = state_auto_approve();

        let out = handle_jobs_command(
            "schedule @once 2099-07-20 bash echo scheduled-hello",
            &mut state,
        )
        .await;
        let id = out
            .lines()
            .next()
            .unwrap()
            .strip_prefix("📅 Scheduled job created: ")
            .unwrap()
            .trim()
            .to_string();

        let run_now = handle_jobs_command(&format!("run-now {id}"), &mut state).await;
        assert!(
            run_now.contains("success") || run_now.contains("Completed"),
            "got: {run_now}"
        );

        let logs = handle_jobs_command(&format!("logs {id}"), &mut state).await;
        assert!(
            logs.contains("scheduled-hello"),
            "logs missing output: {logs}"
        );

        assert!(notify_completed_scheduled_jobs(&mut state).await);
        let msg = state.messages.back().unwrap().content.clone();
        assert!(msg.contains(&id), "notification missing id: {msg}");
        assert!(
            msg.contains("success"),
            "notification missing status: {msg}"
        );
        // A second pass must not post again.
        assert!(!notify_completed_scheduled_jobs(&mut state).await);
    }

    #[tokio::test]
    async fn schedule_without_kind_returns_usage() {
        let _guard = scheduled_test_lock().lock().await;
        let (_tmp, _store) = tmp_jobs_dir();
        let mut state = state();

        let out = handle_jobs_command("schedule @daily echo hi", &mut state).await;
        assert!(out.contains("Usage"), "got: {out}");
    }
}
