//! `/jobs` slash-command handler and the background-job completion
//! notifier.
//!
//! The TUI's event loop calls `notify_completed_jobs` on every tick;
//! we keep it cheap (one `HashSet::insert` per finished job) so the
//! cost is bounded by the number of jobs, not the number of turns.
//!
//! Four `/jobs` sub-commands are supported:
//! - `/jobs`              → list all background jobs (status + command)
//! - `/jobs <id>`         → show detail for a single job: status,
//!   command, start/finish timestamps, and the tail of stdout/stderr
//!   (with an elision marker if truncated)
//! - `/jobs <id> cancel`  → cancel a running job. The job's status
//!   flips to `Cancelled`; a completion notification will be appended
//!   to the chat on the next event-loop tick.
//! - `/jobs clean`        → drop all completed/failed/cancelled jobs
//!   from the registry. Running jobs are preserved.
//!
//! See `bash_jobs.rs` for the registry implementation and its unit
//! tests.

use crate::tui::app::AppState;

/// Maximum number of stdout/stderr lines shown when inspecting a single
/// job via `/jobs <id>`. Long-running builds can produce thousands of
/// lines; the user usually wants the tail (the actual error or final
/// status), so we keep the LAST `JOB_DETAIL_TAIL_LINES` lines and
/// indicate how many were elided.
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

/// Handle `/jobs` command.
pub async fn handle_jobs_command(args: &str) -> String {
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
            state.messages.push(crate::tui::app::ConversationEntry::new(
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
