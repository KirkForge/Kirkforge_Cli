//! Slash-command handlers and background-job notifier.
//!
//! All of these are pure functions over `AppState` (and channels) that
//! return a `String` describing the outcome. The input-key handler in
//! `keys.rs` is responsible for pushing the string into `state.messages`
//! as a system message — keeping the formatting policy in one place.

use crate::session::access::PathGuard;
use crate::session::conversation::ConversationLog;
use crate::shared::minify::minify_source;
use crate::shared::{Message, Role};
use crate::tui::app::{AppState, ConversationEntry};
use std::path::Path;
use tokio::sync::mpsc;

/// Convert a slice of persisted `Message`s into display-ready
/// `ConversationEntry`s for the TUI's chat panel.
///
/// This is the inverse of what `ConversationLog::append` does on
/// disk — it rebuilds the in-memory display from the NDJSON log so
/// the user can see the conversation history after a `/resume`.
///
/// `Message::role` is a `Role` enum; `ConversationEntry::role` is a
/// `String` (the chat widget switches on `"user"`, `"assistant"`,
/// `"system"`, `"tool"`). The mapping is mechanical:
///
/// - `Role::User`      → `"user"`
/// - `Role::System`    → `"system"`
/// - `Role::Assistant` → `"assistant"`
/// - `Role::Tool`      → `"tool"` (no sidecar — see note below)
///
/// For `Role::Tool` we do **not** reconstruct the `tool_output`
/// sidecar that the live event loop populates when a `ToolResult`
/// arrives. The on-disk log only has the full content; the
/// "summary vs full" distinction is a UI-time concept. So we use
/// `ConversationEntry::new("tool", content)` and the chat widget
/// will render the full content verbatim — this is the documented
/// behaviour for tool entries without a `tool_output` sidecar and
/// matches the legacy forward-compat path.
///
/// Pure function, no I/O, no async. Unit-tested at the bottom of
/// this file.
pub fn messages_to_entries(messages: &[Message]) -> Vec<ConversationEntry> {
    messages
        .iter()
        .map(|m| {
            let role = match m.role {
                Role::User => "user",
                Role::System => "system",
                Role::Assistant => "assistant",
                Role::Tool => "tool",
            };
            ConversationEntry::new(role, m.content.clone())
        })
        .collect()
}

/// Handle `/fork` command: list forks or create a new one.
pub async fn handle_fork_command(args: &str, state: &mut AppState) -> String {
    let fm = match state.fork_manager.as_mut() {
        Some(fm) => fm,
        None => return "No fork manager available (session not initialized).".into(),
    };

    let trimmed = args.trim();
    if trimmed.eq_ignore_ascii_case("list") || trimmed.is_empty() {
        let forks = fm.list();
        if forks.is_empty() {
            return "No forks created yet. Use `/fork <label> [count]` to create one.".into();
        }
        let mut out = "Session forks:\n".to_string();
        for f in forks {
            out.push_str(&format!(
                "  {} — {} (fork point: {}, created: {})\n",
                f.id, f.label, f.fork_point, f.created_at
            ));
        }
        return out;
    }

    // Parse: label [count]
    let parts: Vec<&str> = trimmed.splitn(2, ' ').collect();
    if parts.is_empty() || parts[0].is_empty() {
        return "Usage: /fork list | /fork <label> [count]".into();
    }

    let label = parts[0];

    // Build a fake ConversationLog from our messages for fork creation
    // We use the conversation log path stored in state
    let log_path = match &state.log_path {
        Some(p) => p.clone(),
        None => return "No log path available. Cannot create fork.".into(),
    };

    // Open the conversation log to read the latest state
    match ConversationLog::open(log_path) {
        Ok(conv_log) => {
            // Fork point: -1 (end) by default, or parse an optional count
            let fork_point: i64 = parts.get(1).and_then(|s| s.parse().ok()).unwrap_or(-1);

            match fm.create_fork(label, &conv_log, fork_point) {
                Ok(fork) => format!(
                    "✅ Created fork {} — \"{}\" at message #{} (path: {})",
                    fork.id,
                    fork.label,
                    fork.fork_point,
                    fork.path.display()
                ),
                Err(e) => format!("Error creating fork: {}", e),
            }
        }
        Err(e) => format!("Error opening conversation log: {}", e),
    }
}

/// Handle `/resume` command: switch the active session to a fork.
///
/// Usage: `/resume <fork-id>`
pub async fn handle_resume_command(
    args: &str,
    state: &mut AppState,
    resume_tx: &mpsc::UnboundedSender<ConversationLog>,
) -> String {
    let fork_id = args.trim();
    if fork_id.is_empty() {
        return "Usage: /resume <fork-id>\nUse `/fork list` to see available forks.".into();
    }

    let fm = match state.fork_manager.as_mut() {
        Some(fm) => fm,
        None => return "No fork manager available.".into(),
    };

    // Verify the fork exists
    let fork = match fm.get(fork_id) {
        Some(f) => f.clone(),
        None => {
            let available: Vec<&str> = fm.list().iter().map(|f| f.id.as_str()).collect();
            return format!(
                "Fork '{}' not found. Available forks: [{}]",
                fork_id,
                available.join(", ")
            );
        }
    };

    // Open the fork's conversation log
    match ConversationLog::open(fork.path.clone()) {
        Ok(fork_log) => {
            // Reload the TUI's display list from the fork's persisted
            // history BEFORE sending the log to the executor. If the
            // executor swap fails for any reason (e.g. it has shut
            // down), the TUI will at least show the fork's history
            // and the user can see what they were resuming into.
            let entries = messages_to_entries(fork_log.all());
            let entry_count = entries.len();

            // Send the fork log to the executor (swaps in-place)
            if resume_tx.send(fork_log).is_err() {
                return "Error: executor is not running.".into();
            }

            // Reload TUI state from the fork's conversation. We clear
            // everything that came from the OLD session so the user
            // doesn't see stale indicators from a previous turn:
            //
            //   - messages: replaced with the fork's history
            //   - thinking_buffer: any in-flight thinking text is
            //     for the OLD session
            //   - pending_approval: any approval prompt is for a
            //     tool call in the OLD session; the fork's history
            //     may not even have a counterpart
            //   - expanded_tools / notified_jobs: indices/ids from
            //     the OLD session are meaningless
            //   - last_turn_prompt_tokens: 0; the executor will
            //     emit a fresh CostStats on the next turn of the
            //     resumed session
            //   - tokens_sent / tokens_received / cumulative_cost:
            //     these are running-session counters; the resumed
            //     session is logically a new session for accounting
            //     purposes (a re-fork from the resumed session will
            //     record `parent_session` as the original anyway)
            state.messages = entries;
            state.thinking_buffer.clear();
            state.pending_approval = None;
            state.expanded_tools.clear();
            state.notified_jobs.clear();
            state.last_turn_prompt_tokens = 0;
            state.tokens_sent = 0;
            state.tokens_received = 0;
            state.cumulative_cost = 0.0;
            state.turn_cost = 0.0;

            // Update session identity
            state.session_id = format!("{} (fork: {})", state.session_id, fork.id);
            state.log_path = Some(fork.path);

            format!(
                "✅ Resumed fork '{}' — \"{}\" ({} messages reloaded). Type a message to continue.",
                fork.id,
                fork.label,
                entry_count,
            )
        }
        Err(e) => format!("Error opening fork log: {}", e),
    }
}

/// Maximum number of stdout/stderr lines shown when inspecting a single
/// job via `/jobs <id>`. Long-running builds can produce thousands of
/// lines; the user usually wants the tail (the actual error or final
/// status), so we keep the LAST `TAIL_LINES` lines and indicate how many
/// were elided.
const JOB_DETAIL_TAIL_LINES: usize = 50;

/// Format a `BashJob`'s status as a single short string.
///
/// Centralised so the list view (`/jobs`), the detail view
/// (`/jobs <id>`), and the completion notifier in
/// `notify_completed_jobs` all render the status the same way. Earlier
/// versions had inconsistent formatting (Running used `(id=5)`, the
/// others used `#5`); the new format always uses `#id` for consistency.
fn format_job_status(job: &crate::session::bash_jobs::BashJob) -> String {
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
fn tail_lines(s: &str, n: usize) -> (String, usize) {
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
///
/// Four sub-commands:
/// - `/jobs`              → list all background jobs (status + command)
/// - `/jobs <id>`         → show detail for a single job: status, command,
///   start/finish timestamps, and the tail of
///   stdout/stderr (with an elision marker if
///   truncated)
/// - `/jobs <id> cancel`  → cancel a running job. The job's status flips
///   to `Cancelled`; a completion notification
///   will be appended to the chat on the next
///   event-loop tick. Already-finished jobs are
///   not affected (the `BashJobRegistry::cancel`
///   method already enforces this — see its
///   unit test in `bash_jobs.rs`).
/// - `/jobs clean`        → drop all completed/failed/cancelled jobs from
///   the registry. Running jobs are preserved.
///   Uses `BashJobRegistry::clean()` which is
///   already unit-tested.
///
/// `/jobs <id>` is the natural follow-up to spawning a long-running job
/// (e.g. `cargo build` in the background) — without it the user has no
/// way to see the job's output except by running a fresh `bash` tool
/// to read it back. With it, the full lifecycle is observable from
/// the chat. `/jobs <id> cancel` closes the remaining gap: previously
/// the only way to stop a hung background job was to ask the model
/// to call `bash_cancel` (one full turn + tool approval); now the
/// user can cancel directly from the TUI without round-tripping
/// through the model.
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
            format!("🧹 Cleaned {} finished jobs.", cleaned)
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
                first, second.unwrap_or(""), extra
            );
        }

        let id: u64 = match first.parse() {
            Ok(n) => n,
            Err(_) => {
                return format!(
                    "Usage: /jobs [clean | <id> | <id> cancel]\nGot: /jobs {}",
                    first
                );
            }
        };

        // `/jobs <id> cancel` — cancel a running job
        if let Some(sub) = second {
            if !sub.eq_ignore_ascii_case("cancel") {
                return format!(
                    "Usage: /jobs [clean | <id> | <id> cancel]\nGot: /jobs {} {}",
                    id, sub
                );
            }
            let registry = crate::session::bash_jobs::global_registry();
            return match registry.cancel(id).await {
                true => format!(
                    "🚫 Cancellation requested for job #{}. The completion notifier will post the final status.",
                    id
                ),
                false => match registry.get(id).await {
                    Some(job) => format!(
                        "Job #{} is not running (status: {}). Nothing to cancel.",
                        id,
                        format_job_status(&job)
                    ),
                    None => format!("Job #{} not found. No jobs to cancel.", id),
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
                    out.push_str(&format!(
                        "  Finished: {}\n",
                        f.format("%Y-%m-%d %H:%M:%S")
                    ));
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
                            "  [... {} lines elided, showing last {} ...]\n",
                            elided, JOB_DETAIL_TAIL_LINES
                        ));
                    }
                    for line in tail.lines() {
                        out.push_str(&format!("  {}\n", line));
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
                            "  [... {} lines elided, showing last {} ...]\n",
                            elided, JOB_DETAIL_TAIL_LINES
                        ));
                    }
                    for line in tail.lines() {
                        out.push_str(&format!("  {}\n", line));
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
                    return format!("Job #{} not found. No background jobs exist.", id);
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
            out.push_str(&format!(
                "  {} — {}\n",
                format_job_status(job),
                job.command
            ));
        }
        out.push_str(
            "\nTip: /jobs <id> for detail, /jobs <id> cancel to stop a running job, /jobs clean to drop finished jobs.\n",
        );
        out
    }
}

/// Handle `/compact` command: trigger a user-driven compaction of the
/// conversation history.
///
/// The actual compaction work happens asynchronously in the executor
/// (it calls `PromptBuilder::compact` and rewrites the NDJSON log
/// atomically). We just kick it off by sending `()` on `compact_tx`
/// and return a status string immediately. When the executor finishes,
/// it emits `TurnEvent::CompactionReport`, which the TUI event loop
/// consumes to rebuild the display list and append a 🧹 status message.
///
/// `args` is accepted for forward-compatibility (e.g. `/compact --force`
/// to skip the "no recent tool results to compact" short-circuit) but is
/// currently ignored. Keeps the signature symmetric with
/// `handle_fork_command` / `handle_resume_command`.
pub async fn handle_compact_command(
    args: &str,
    compact_tx: &mpsc::UnboundedSender<()>,
) -> String {
    // Reserved for future flags; explicit `_args` naming keeps clippy happy
    // and signals the intent without forcing a `let _ = args;` no-op.
    let _ = args;

    match compact_tx.send(()) {
        Ok(()) => "🧹 Compaction requested. The executor will rewrite the conversation log and the chat view will refresh when it finishes.".into(),
        Err(e) => format!("❌ Failed to send compact request to executor: {}", e),
    }
}

/// Handle `/status` command: print a one-shot summary of session metrics
/// to the chat view.
///
/// This is a richer, on-demand view of the same data the status bar
/// shows in real time: model, cumulative cost, tokens sent/received,
/// and the per-turn context pressure as a fraction of the model's
/// max context window. Useful when the user wants to know "should I
/// `/compact` now?" without scanning the status bar.
///
/// Reads the same fields the status widget reads (`last_turn_prompt_tokens`,
/// `tokens_sent`, `tokens_received`, `cumulative_cost`, `turn_cost`,
/// `model_info.max_context_tokens`) and reuses `format_budget_indicator`
/// for the percentage so the math is consistent between the bar and
/// the on-demand view. The pure `format_status_block` helper is
/// unit-tested without touching `AppState`.
///
/// `args` is accepted for forward-compatibility (e.g. `/status --json`)
/// but is currently ignored.
pub async fn handle_status_command(args: &str, state: &mut AppState) -> String {
    let _ = args;
    format_status_block(state)
}

/// Build the `/status` report as a single string. Pulled out of the
/// async handler so it can be unit-tested synchronously.
///
/// Sections:
///   1. Model identity (name, max context, thinking/tool support
///      from `ModelInfo`)
///   2. Session metrics (elapsed, cumulative + turn cost, tokens in/out)
///   3. Context pressure (per-turn prompt tokens vs. max context,
///      in the same format the status bar shows it)
///   4. Recommendation: when the per-turn prompt is in the
///      "consider /compact" zone, surface that explicitly.
pub fn format_status_block(state: &AppState) -> String {
    use crate::tui::rendering::{budget_pct, format_budget_indicator, format_token_count};

    let mut out = String::new();
    out.push_str("📊 Session status\n\n");

    // 1. Model
    if let Some(m) = &state.model_info {
        out.push_str(&format!("Model:        {}\n", m.name));
        out.push_str(&format!(
            "Max context:  {} tokens\n",
            format_token_count(m.max_context_tokens)
        ));
        out.push_str(&format!(
            "Thinking:     {}\n",
            if m.supports_thinking { "yes" } else { "no" }
        ));
        out.push_str(&format!(
            "Tool calls:   {}\n",
            match m.tool_call_format {
                crate::shared::ToolCallStyle::Native => "native",
                crate::shared::ToolCallStyle::OpenAiCompat => "openai-compat",
                crate::shared::ToolCallStyle::None => "none",
            }
        ));
    } else {
        out.push_str("Model:        (not connected)\n");
    }

    // 2. Session metrics
    let elapsed = state.session_started.elapsed();
    let secs = elapsed.as_secs();
    let h = secs / 3600;
    let m = (secs % 3600) / 60;
    let s = secs % 60;
    let elapsed_str = if h > 0 {
        format!("{}h {}m {}s", h, m, s)
    } else if m > 0 {
        format!("{}m {}s", m, s)
    } else {
        format!("{}s", s)
    };
    out.push_str(&format!("\nSession:      {}\n", elapsed_str));
    out.push_str(&format!(
        "Cost:         ${:.4} cumulative, ${:.4} this turn\n",
        state.cumulative_cost, state.turn_cost
    ));
    out.push_str(&format!(
        "Tokens:       {} sent, {} received\n",
        format_token_count(state.tokens_sent),
        format_token_count(state.tokens_received)
    ));

    // 3. Context pressure
    let max_ctx = state
        .model_info
        .as_ref()
        .map(|m| m.max_context_tokens)
        .unwrap_or(0);
    if state.last_turn_prompt_tokens > 0 && max_ctx > 0 {
        let (text, _color) = format_budget_indicator(state.last_turn_prompt_tokens, max_ctx);
        out.push_str(&format!("\nContext:      {} (last turn)\n", text));
    } else {
        out.push_str("\nContext:      (no turn yet)\n");
    }

    // 4. Recommendation
    if let Some(pct) = budget_pct(state.last_turn_prompt_tokens, max_ctx) {
        if pct >= 80 {
            out.push_str("\n⚠️  Context is tight — consider /compact.\n");
        } else if pct >= 50 {
            out.push_str("\n💡 Context is filling up — /compact is a one-shot way to free room.\n");
        } else {
            out.push_str("\n✅ Context is comfortable.\n");
        }
    }

    out
}

/// Check for recently-completed background jobs and push a notification.
pub async fn notify_completed_jobs(state: &mut AppState) {
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
        }
    }
}


// ============================================================================
// v1.2-p14 — `!` bash passthrough
// ============================================================================
//
// The `!` prefix is a UX escape hatch: a line like `!cargo test` runs the
// command directly via the shell and shows the output in the chat, with no
// model round trip and no approval gate. This is the feature Claude Code
// users reach for when they want to "do this now and don't ask the model" —
// for fast feedback loops, eyeballing a build, or running a one-liner
// before composing a longer prompt.
//
// Design choices:
//
// - **No model round trip.** The whole point is "don't wait for inference."
//   The command runs in `~ms`, not seconds. The user sees output immediately
//   and can re-engage the model with a follow-up prompt.
// - **No approval gate.** The user *typed* the command. Approval would
//   defeat the purpose of the escape hatch. The bash tool's read-only
//   classification and permission rules don't apply here; the user is
//   the author. (Future: a config flag `bang_requires_approval` could
//   opt in to the approval flow for high-stakes environments.)
// - **30-second timeout** (matches the bash tool's default). Long-running
//   commands should use `!cmd &` (background) and then `/jobs`.
// - **Output goes through `ConversationEntry::tool(summary, full)`** so the
//   existing collapse/expand UX in `chat.rs` applies automatically. A
//   `!find .` that returns 500 lines is collapsed to a 4-line summary
//   box; the user hits Enter or Tab on empty input to see the full
//   output. This is critical — otherwise `!` becomes a new flood vector.
// - **Stderr is shown separately** with a `⚠ stderr:` marker, so the user
//   can distinguish "this is normal output" from "this is a warning."
// - **Exit code is always shown**, even on success. Saves the user from
//   squinting at "did it actually work?".
// - **Pure formatting helpers are unit-tested**; the shell spawn is tested
//   with fast `echo` / `false` / `true` commands.

/// Default timeout for `!` commands, in seconds. Matches the bash tool's
/// default (`src/tools/bash.rs`). Long-running work should use the
/// background syntax (`!cmd &`) and `/jobs`.
pub const BANG_DEFAULT_TIMEOUT_SECS: u64 = 30;

/// What a `!` command actually did. Pure data — the formatting helpers
/// turn this into a display string. Splitting spawn from formatting
/// keeps the presentation policy testable without I/O.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BangResult {
    /// The command that was run, verbatim. Display only.
    pub cmd: String,
    /// Process exit code, or `-1` if the process was killed by signal.
    pub exit_code: i32,
    /// Captured stdout (UTF-8 lossy).
    pub stdout: String,
    /// Captured stderr (UTF-8 lossy).
    pub stderr: String,
    /// `true` if the command hit the timeout and was killed.
    pub timed_out: bool,
    /// How long the command took, in milliseconds. Display only.
    pub elapsed_ms: u64,
}

impl BangResult {
    /// `true` if the process exited with status 0 and was not killed by
    /// the timeout. Used by `format_bang_output` to pick the icon and
    /// banner colour.
    pub fn is_success(&self) -> bool {
        self.exit_code == 0 && !self.timed_out
    }
}

/// Run a shell command directly without going through the model. The
/// user typed `!` deliberately — no approval gate, no model round trip.
///
/// This is a thin wrapper over `tokio::process::Command` with a timeout
/// and `kill_on_drop`, matching the bash tool's foreground-execution
/// shape (`src/tools/bash.rs::run_shell`). Working dir is the current
/// process dir; we deliberately don't chdir to the project root here —
/// `!` is a "I want to do this now in the shell I'm in" feature, not a
/// re-skin of the bash tool.
///
/// Returns a `BangResult` capturing stdout/stderr/exit_code/timed_out
/// for the formatter to consume. Does not write to `state` — the
/// caller (`keys.rs`) is responsible for pushing the formatted string
/// into `state.messages` so the conversation log records what happened.
pub async fn run_bang_command(cmd: &str) -> BangResult {
    use std::process::Stdio;
    use std::time::{Duration, Instant};

    let start = Instant::now();
    let mut proc = tokio::process::Command::new("/bin/sh");
    proc.arg("-c")
        .arg(cmd)
        .kill_on_drop(true)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let output = tokio::time::timeout(
        Duration::from_secs(BANG_DEFAULT_TIMEOUT_SECS),
        proc.output(),
    )
    .await;

    let elapsed_ms = start.elapsed().as_millis() as u64;

    match output {
        Ok(Ok(out)) => BangResult {
            cmd: cmd.to_string(),
            exit_code: out.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&out.stdout).to_string(),
            stderr: String::from_utf8_lossy(&out.stderr).to_string(),
            timed_out: false,
            elapsed_ms,
        },
        Ok(Err(e)) => BangResult {
            cmd: cmd.to_string(),
            // `-1` signals "could not even spawn", distinct from a process
            // that ran and exited non-zero. The formatter surfaces this
            // as a clear error to the user.
            exit_code: -1,
            stdout: String::new(),
            stderr: format!("Failed to execute command: {}", e),
            timed_out: false,
            elapsed_ms,
        },
        Err(_) => BangResult {
            cmd: cmd.to_string(),
            exit_code: -1,
            stdout: String::new(),
            stderr: format!("Command timed out after {} seconds", BANG_DEFAULT_TIMEOUT_SECS),
            timed_out: true,
            elapsed_ms,
        },
    }
}

/// Format a `BangResult` into a single display string. Pure function —
/// given the same `BangResult`, produces the same string. This is what
/// the user sees in the chat view.
///
/// Layout (success, no stderr):
/// ```text
/// $ cargo build
/// ✅ exit 0 in 1.42s
/// <stdout>
/// ```
///
/// Layout (failure, has stderr):
/// ```text
/// $ rm /etc/passwd
/// ❌ exit 1 in 0.03s
/// <stdout>
/// ⚠ stderr:
/// <stderr>
/// ```
///
/// Layout (timed out):
/// ```text
/// $ sleep 99
/// ⏰ timed out after 30s
/// ```
///
/// Stdout/stderr are passed through verbatim — the user wants to see
/// what the command actually did. If output is huge, the existing
/// `ConversationEntry::tool(summary, full)` collapse applies in `chat.rs`,
/// so a 500-line `!find` doesn't flood the chat.
pub fn format_bang_output(result: &BangResult) -> String {
    if result.timed_out {
        return format!("$ {}\n⏰ timed out after {}s", result.cmd, BANG_DEFAULT_TIMEOUT_SECS);
    }

    let elapsed = format_elapsed(result.elapsed_ms);
    let icon = if result.is_success() { "✅" } else { "❌" };

    let mut out = format!("$ {}\n{} exit {} in {}", result.cmd, icon, result.exit_code, elapsed);

    if !result.stdout.is_empty() {
        out.push('\n');
        out.push_str(&result.stdout);
    }

    if !result.stderr.is_empty() {
        // Trim trailing whitespace from stdout before appending the
        // stderr marker so the layout is clean even when stdout didn't
        // end in a newline.
        if !result.stdout.is_empty() && !out.ends_with('\n') {
            out.push('\n');
        }
        out.push_str("⚠ stderr:\n");
        out.push_str(&result.stderr);
    }

    out
}

/// Format a millisecond duration as a short human string. Pure helper,
/// unit-tested alongside `format_bang_output`.
///
/// Examples:
/// - `0`     → `"0ms"`
/// - `42`    → `"42ms"`
/// - `1420`  → `"1.42s"`
/// - `90000` → `"1m30s"`
fn format_elapsed(ms: u64) -> String {
    if ms < 1000 {
        format!("{}ms", ms)
    } else if ms < 60_000 {
        // Two decimal places for sub-minute durations. e.g. 1.42s, 12.34s.
        format!("{:.2}s", ms as f64 / 1000.0)
    } else {
        let secs = ms / 1000;
        let minutes = secs / 60;
        let rem = secs % 60;
        format!("{}m{:02}s", minutes, rem)
    }
}

/// The full `!` command pipeline: run the command, format the result.
/// The caller is responsible for pushing the returned string into
/// `state.messages`.
///
/// This is the function `keys.rs::Enter` calls when the input buffer
/// starts with `!`. The `!` itself is stripped before this is called
/// (the keys handler is responsible for the prefix detection).
///
/// Returns a `String` ready to display. The caller may want to wrap
/// it in `ConversationEntry::tool(summary, full)` for collapse support,
/// but for the v1.2-p14 first cut we return the full string and let
/// the caller decide how to display it.
pub async fn handle_bang_command(cmd: &str) -> String {
    if cmd.trim().is_empty() {
        // Empty `!` is a no-op. We could also surface a hint about
        // "type `!help` for what this does" but the user already
        // knows they're in a TUI.
        return "Usage: !<command>  — runs <command> via /bin/sh with no model round trip.".to_string();
    }
    let result = run_bang_command(cmd).await;
    format_bang_output(&result)
}

// ── @-mentions (v1.2-p15) ─────────────────────────────────────────────
//
// A line containing `@<path>` tokens is "augmented" before being sent
// to the model: each token is replaced with the file's contents,
// formatted as a fenced code block. This gives the user a Claude
// Code–style "review this file" gesture without forcing them to use
// the model's tool calls.
//
// Examples:
//
//   @src/main.rs                          inline the file (minified)
//   @src/main.rs:raw                      inline the file verbatim
//   @src/main.rs:10-50                    inline lines 10–50 (1-indexed,
//                                         inclusive on both ends)
//   @~/notes.md                           tilde expansion supported
//   @src/lib.rs:10-50:raw                 range + verbatim
//   multiple @<path> tokens in one input  all expanded
//
// The file is read at submit time, NOT when the model asks for it.
// If the file is huge, it is minified (default) and capped at
// `MENTION_MAX_BYTES`. Missing/denied/unreadable paths are NOT errors
// from the user's perspective — the prompt still goes through, and
// the model sees a short `[could not read: <reason>]` placeholder so
// it can react. We strip the `@<path>` tokens from the user-facing
// display copy (they would just look like noise in the chat log).
//
// Pure helpers throughout — the I/O happens in `expand_mentions`,
// everything else is byte/string surgery. This makes the parsing
// edge cases trivially unit-testable.

/// Per-mention byte cap. Matches `B1.4` read_file budget so a single
/// @-mention cannot blow the model's context window by itself.
pub const MENTION_MAX_BYTES: usize = 50_000;

/// Number of bytes a single mention occupies in the rendered prompt
/// when the source file is too large to fit. We always include some
/// head + tail + a marker so the model sees both the call site and
/// the result/error (same pattern as `B1.1` tool output cap).
const MENTION_HEAD_BYTES: usize = 30_000;
const MENTION_TAIL_BYTES: usize = 15_000;

/// A parsed `@<path>[:<range>][:raw]` token, with byte offsets in the
/// original input string so the stripper can excise it without
/// re-scanning.
#[derive(Debug, Clone, PartialEq)]
pub struct MentionParse {
    pub spec: MentionSpec,
    /// Inclusive byte offset of the leading `@`.
    pub start: usize,
    /// Exclusive byte offset one past the last byte of the token
    /// (i.e. the slice `&input[start..end]` is the raw token).
    pub end: usize,
}

/// What the user actually asked for. The path may be relative
/// (resolved against the project root at expand time) or absolute.
/// `raw` suppresses the default minification.
#[derive(Debug, Clone, PartialEq)]
pub struct MentionSpec {
    pub path: String,
    /// 1-indexed, inclusive on both ends. `None` = whole file.
    pub range: Option<(usize, usize)>,
    pub raw: bool,
}

/// A mention that has been resolved against the filesystem.
#[derive(Debug, Clone, PartialEq)]
pub struct MentionExpansion {
    pub spec: MentionSpec,
    pub content: String,
    pub status: MentionStatus,
}

/// Outcome of the file read. `Ok` carries the number of bytes the
/// model actually sees (post-minify, post-truncate) and whether the
/// file was minified or truncated.
#[derive(Debug, Clone, PartialEq)]
pub enum MentionStatus {
    Ok {
        bytes: usize,
        minified: bool,
        truncated: bool,
    },
    NotFound,
    Denied(String),
    IoError(String),
    InvalidRange(String),
}

impl MentionExpansion {
    /// True if the resolved content is suitable to be inlined into
    /// the prompt (i.e. we have actual file content to show the model).
    pub fn is_ok(&self) -> bool {
        matches!(self.status, MentionStatus::Ok { .. })
    }

    /// Display label for the path — always shows the path the user
    /// typed, never the resolved/canonical form (the user typed what
    /// they typed).
    pub fn display_path(&self) -> &str {
        &self.spec.path
    }
}

/// Scan `input` for `@<path>[:<range>][:raw]` tokens.
///
/// A token starts with `@` and continues until the next whitespace
/// (or end of string). Inside the token, the path component is
/// everything up to the first `:`; if no `:` is present, the whole
/// thing after `@` is the path. The path may not contain `:`, so
/// `~`-expanded home dirs are fine but `C:\...` Windows paths will
/// be cut at the colon. We accept that limitation — the TUI runs
/// on Linux/macOS in practice.
///
/// Edge cases handled:
///
/// - `@` alone (no path) — NOT a mention, kept as literal text
/// - `@@foo` (double-`@`) — kept as literal text; only single `@` starts a mention
/// - `@path with spaces` — first whitespace ends the mention
/// - `@path:` (empty range) — kept as literal path (no range)
/// - `@path:abc` (non-numeric range) — kept as literal path
#[derive(Debug, Clone, PartialEq)]
pub struct MentionToken {
    pub spec: MentionSpec,
    pub start: usize,
    pub end: usize,
}

pub fn parse_mentions(input: &str) -> Vec<MentionToken> {
    let mut out = Vec::new();
    let bytes = input.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'@' {
            i += 1;
            continue;
        }
        // Reject `@@` (double-@ — only single @ starts a mention).
        if i + 1 < bytes.len() && bytes[i + 1] == b'@' {
            i += 2;
            continue;
        }
        // Reject `@` at end of input or followed by whitespace.
        let after = i + 1;
        if after >= bytes.len() || bytes[after].is_ascii_whitespace() {
            i += 1;
            continue;
        }
        // Find end of token — next whitespace.
        let mut end = after;
        while end < bytes.len() && !bytes[end].is_ascii_whitespace() {
            end += 1;
        }
        // Parse the slice input[after..end] as `path[:range][:raw]`.
        let raw = &input[after..end];
        if let Some(spec) = parse_mention_spec(raw) {
            out.push(MentionToken { spec, start: i, end });
        }
        i = end;
    }
    out
}

/// Parse a `path[:range][:raw]` string into a `MentionSpec`. Returns
/// `None` if the path is empty or the range syntax is malformed
/// (in which case the caller should NOT treat the original text as
/// a mention — we want the user to see it literally).
fn parse_mention_spec(raw: &str) -> Option<MentionSpec> {
    // Strip a trailing `:raw` first.
    let (raw_stripped, raw) = match raw.strip_suffix(":raw") {
        Some(rest) => (rest, true),
        None => (raw, false),
    };
    // Now look for a range. The range uses `-` as the separator and
    // is the FIRST `:...` segment if present.
    let (path_part, range) = match raw_stripped.find(':') {
        Some(idx) => {
            let (p, r) = raw_stripped.split_at(idx);
            // r starts with `:` — strip it and parse the range body.
            let range_body = &r[1..];
            let parsed = parse_range(range_body);
            match parsed {
                Some(rng) => (p, Some(rng)),
                None => {
                    // Malformed range — bail out and let the user
                    // see the literal text. This is important for
                    // @-mentions inside prose that happen to
                    // contain a colon (e.g. `@see RFC 1234: details`).
                    return None;
                }
            }
        }
        None => (raw_stripped, None),
    };
    if path_part.is_empty() {
        return None;
    }
    Some(MentionSpec {
        path: path_part.to_string(),
        range,
        raw,
    })
}

/// Parse a `START-END` range string. `START` and `END` are positive
/// integers; `END >= START`. Returns `None` for malformed input.
fn parse_range(body: &str) -> Option<(usize, usize)> {
    let dash = body.find('-')?;
    let (a, b) = body.split_at(dash);
    let b = &b[1..]; // strip the `-`
    if a.is_empty() || b.is_empty() {
        return None;
    }
    let start: usize = a.parse().ok()?;
    let end: usize = b.parse().ok()?;
    if end < start {
        return None;
    }
    Some((start, end))
}

/// Remove the raw `@<path>...` tokens from `input` and collapse the
/// whitespace around them. Returns the cleaned text. The relative
/// ordering of the non-mention text is preserved.
///
/// We do not delete the original text — the user typed it and we
/// want to show them what the model actually received. Instead, we
/// replace each mention with an empty string, then collapse runs of
/// whitespace that adjoin the deletion.
pub fn strip_mentions(input: &str, mentions: &[MentionToken]) -> String {
    if mentions.is_empty() {
        return input.to_string();
    }
    // Sort by start (parse_mentions already returns them in order, but
    // we don't want to rely on that).
    let mut sorted: Vec<&MentionToken> = mentions.iter().collect();
    sorted.sort_by_key(|m| m.start);
    let mut out = String::with_capacity(input.len());
    let mut cursor = 0;
    for m in sorted {
        // Append everything between cursor and m.start, then collapse
        // any trailing whitespace into a single space.
        if m.start > cursor {
            out.push_str(&input[cursor..m.start]);
        }
        // Skip the mention and any whitespace immediately after it.
        let mut new_cursor = m.end;
        while new_cursor < input.len() && input.as_bytes()[new_cursor].is_ascii_whitespace() {
            new_cursor += 1;
        }
        cursor = new_cursor;
        // If we still have non-whitespace text after the mention
        // (in practice this doesn't happen because the mention
        // ends at whitespace, but defensive), leave a single space
        // so words don't run together.
        if cursor < input.len() && cursor > m.end {
            out.push(' ');
        }
    }
    if cursor < input.len() {
        out.push_str(&input[cursor..]);
    }
    out
}

/// Read the files for the given mention parses and return one
/// expansion per parse. Uses `PathGuard` for the same path-safety
/// checks the model's `read_file` tool would — a path that's denied
/// for the model is also denied for the user-driven mention.
///
/// Behaviour matches the model's tool semantics:
/// - `read_file` denials (`Denied` verdict) → `MentionStatus::Denied`
/// - missing file → `MentionStatus::NotFound`
/// - I/O error → `MentionStatus::IoError`
/// - malformed range → `MentionStatus::InvalidRange`
/// - success → `MentionStatus::Ok` with bytes/minified/truncated flags
pub fn expand_mentions(
    mentions: &[MentionToken],
    path_guard: &PathGuard,
) -> Vec<MentionExpansion> {
    mentions
        .iter()
        .map(|m| expand_one(m, path_guard))
        .collect()
}

fn expand_one(m: &MentionToken, path_guard: &PathGuard) -> MentionExpansion {
    // Tilde expansion — same convention as read_file.
    let expanded = shellexpand::tilde(&m.spec.path);
    let path = Path::new(expanded.as_ref());

    // PathGuard::check_read denies a missing path with a "Path does
    // not exist" reason. We want the user to see a clean `NotFound`
    // for the common "I typed the wrong path" case rather than the
    // raw guard message, so check existence up front and short-circuit.
    if !path.exists() {
        return MentionExpansion {
            spec: m.spec.clone(),
            content: String::new(),
            status: MentionStatus::NotFound,
        };
    }

    // Path safety check. The user's intent is "I want the model to
    // see this file", which is a read.
    let resolved = match path_guard.check_read(path) {
        crate::session::access::GuardVerdict::Allowed(p) => p,
        crate::session::access::GuardVerdict::Denied(reason) => {
            return MentionExpansion {
                spec: m.spec.clone(),
                content: String::new(),
                status: MentionStatus::Denied(reason),
            };
        }
    };

    // Read the file.
    let raw = match std::fs::read_to_string(&resolved) {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return MentionExpansion {
                spec: m.spec.clone(),
                content: String::new(),
                status: MentionStatus::NotFound,
            };
        }
        Err(e) => {
            return MentionExpansion {
                spec: m.spec.clone(),
                content: String::new(),
                status: MentionStatus::IoError(e.to_string()),
            };
        }
    };

    // Apply range filter (1-indexed, inclusive on both ends).
    let ranged: String = if let Some((start, end)) = m.spec.range {
        // We need to validate the range against the actual line count.
        let lines: Vec<&str> = raw.lines().collect();
        if start == 0 || start > lines.len() {
            return MentionExpansion {
                spec: m.spec.clone(),
                content: String::new(),
                status: MentionStatus::InvalidRange(format!(
                    "start line {} is out of range (file has {} lines)",
                    start,
                    lines.len()
                )),
            };
        }
        let end = end.min(lines.len());
        lines[(start - 1)..end].join("\n")
    } else {
        // Strip a single trailing newline if present so that
        // @-mentioning a typical text file (which ends in `\n`)
        // produces a clean prompt without a phantom blank line at
        // the end. The model's read_file tool returns content
        // verbatim, but @-mentions are inlined into prose, so the
        // trim is the user-friendly default.
        let trimmed = raw.strip_suffix('\n').unwrap_or(&raw);
        trimmed.to_string()
    };

    // Apply minification unless :raw was specified.
    let minified = !m.spec.raw;
    let content = if m.spec.raw {
        ranged
    } else {
        minify_source(&resolved, &ranged)
    };

    // Truncate if still too big — keep head + marker + tail so the
    // model sees both the call site and the result/error (B1.1 pattern).
    let (final_content, truncated) = truncate_to_cap(&content);

    MentionExpansion {
        spec: m.spec.clone(),
        content: final_content,
        status: MentionStatus::Ok {
            bytes: content.len(),
            minified,
            truncated,
        },
    }
}

fn truncate_to_cap(content: &str) -> (String, bool) {
    if content.len() <= MENTION_MAX_BYTES {
        return (content.to_string(), false);
    }
    // We need the tail to come from the END of the original content,
    // not the middle. Take head from the start, tail from the end.
    let head_end = MENTION_HEAD_BYTES.min(content.len());
    let tail_start = content.len().saturating_sub(MENTION_TAIL_BYTES);
    let head = &content[..head_end];
    let tail = &content[tail_start..];
    let marker = format!(
        "\n... [truncated, {} bytes total — showing first {} + last {}] ...\n",
        content.len(),
        MENTION_HEAD_BYTES,
        MENTION_TAIL_BYTES
    );
    let mut out = String::with_capacity(head.len() + marker.len() + tail.len());
    out.push_str(head);
    out.push_str(&marker);
    out.push_str(tail);
    (out, true)
}

/// Build the inlined block that gets appended to the model's prompt.
/// One fenced code block per mention, in input order. Failures
/// (denied/missing/etc.) are rendered as short `> ` quoted placeholders
/// so the model can react to them in the same turn.
pub fn render_mentions_block(expansions: &[MentionExpansion]) -> String {
    if expansions.is_empty() {
        return String::new();
    }
    let mut out = String::from("\n\nThe user shared the following files for context:\n");
    for e in expansions {
        let label = mention_label(e);
        match &e.status {
            MentionStatus::Ok {
                bytes,
                minified,
                truncated,
            } => {
                let mut flags = Vec::new();
                if *minified {
                    flags.push("minified");
                }
                if *truncated {
                    flags.push("truncated");
                }
                let annotation = if flags.is_empty() {
                    String::new()
                } else {
                    format!(" ({})", flags.join(", "))
                };
                out.push_str(&format!(
                    "\n### `{}` — {} bytes{}\n```\n{}\n```\n",
                    label,
                    bytes,
                    annotation,
                    e.content
                ));
            }
            MentionStatus::NotFound => {
                out.push_str(&format!(
                    "\n### `{}` — could not read: file not found\n",
                    label
                ));
            }
            MentionStatus::Denied(reason) => {
                out.push_str(&format!(
                    "\n### `{}` — could not read: denied ({})\n",
                    label, reason
                ));
            }
            MentionStatus::IoError(err) => {
                out.push_str(&format!(
                    "\n### `{}` — could not read: I/O error ({})\n",
                    label, err
                ));
            }
            MentionStatus::InvalidRange(reason) => {
                out.push_str(&format!(
                    "\n### `{}` — could not read: invalid range ({})\n",
                    label, reason
                ));
            }
        }
    }
    out
}

/// Human-readable label for the rendered block header. Shows the path
/// as typed plus a `:START-END` / `:raw` suffix if those were used.
fn mention_label(e: &MentionExpansion) -> String {
    let mut s = e.spec.path.clone();
    if let Some((start, end)) = e.spec.range {
        s.push_str(&format!(":{}-{}", start, end));
    }
    if e.spec.raw {
        s.push_str(":raw");
    }
    s
}

/// One-line system message for the TUI chat log describing what was
/// inlined. Tells the user "your @-mentions were resolved" with a
/// per-file status row. Pure formatter.
pub fn format_mention_status(expansions: &[MentionExpansion]) -> String {
    if expansions.is_empty() {
        return String::new();
    }
    let mut out = String::from("📎 @-mentions resolved:\n");
    for e in expansions {
        let label = mention_label(e);
        match &e.status {
            MentionStatus::Ok {
                bytes,
                minified,
                truncated,
            } => {
                let mut note = format!("{} bytes", bytes);
                if *minified {
                    note.push_str(", minified");
                }
                if *truncated {
                    note.push_str(", truncated to cap");
                }
                out.push_str(&format!("  ✓ `{}` — {}\n", label, note));
            }
            MentionStatus::NotFound => {
                out.push_str(&format!("  ✗ `{}` — not found\n", label));
            }
            MentionStatus::Denied(reason) => {
                out.push_str(&format!("  ✗ `{}` — denied ({})\n", label, reason));
            }
            MentionStatus::IoError(err) => {
                out.push_str(&format!("  ✗ `{}` — I/O error: {}\n", label, err));
            }
            MentionStatus::InvalidRange(reason) => {
                out.push_str(&format!("  ✗ `{}` — {}\n", label, reason));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shared::{Config, Message, ModelInfo, Role};
    use crate::shared::ToolCallStyle;

    fn user_msg(content: &str) -> Message {
        Message {
            role: Role::User,
            content: content.to_string(),
            thinking: None,
            tool_calls: None,
            tool_call_id: None,
            tool_name: None,
            token_count: None,
        }
    }

    fn assistant_msg(content: &str) -> Message {
        Message {
            role: Role::Assistant,
            content: content.to_string(),
            thinking: None,
            tool_calls: None,
            tool_call_id: None,
            tool_name: None,
            token_count: None,
        }
    }

    fn system_msg(content: &str) -> Message {
        Message {
            role: Role::System,
            content: content.to_string(),
            thinking: None,
            tool_calls: None,
            tool_call_id: None,
            tool_name: None,
            token_count: None,
        }
    }

    fn tool_msg(content: &str) -> Message {
        Message {
            role: Role::Tool,
            content: content.to_string(),
            thinking: None,
            tool_calls: None,
            tool_call_id: Some("call_1".into()),
            tool_name: Some("bash".into()),
            token_count: None,
        }
    }

    // ---------------------------------------------------------------
    // messages_to_entries: pure conversion, exhaustive role coverage
    // ---------------------------------------------------------------

    #[test]
    fn messages_to_entries_empty_input_returns_empty_vec() {
        let entries = messages_to_entries(&[]);
        assert!(entries.is_empty());
    }

    #[test]
    fn messages_to_entries_user_role_maps_to_user_string() {
        let msgs = vec![user_msg("hi")];
        let entries = messages_to_entries(&msgs);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].role, "user");
        assert_eq!(entries[0].content, "hi");
        assert!(entries[0].tool_output.is_none());
    }

    #[test]
    fn messages_to_entries_assistant_role_maps_to_assistant_string() {
        let msgs = vec![assistant_msg("hello back")];
        let entries = messages_to_entries(&msgs);
        assert_eq!(entries[0].role, "assistant");
        assert_eq!(entries[0].content, "hello back");
        assert!(entries[0].tool_output.is_none());
    }

    #[test]
    fn messages_to_entries_system_role_maps_to_system_string() {
        let msgs = vec![system_msg("compaction done")];
        let entries = messages_to_entries(&msgs);
        assert_eq!(entries[0].role, "system");
        assert_eq!(entries[0].content, "compaction done");
    }

    #[test]
    fn messages_to_entries_tool_role_uses_new_constructor_no_sidecar() {
        // The on-disk log only has the full content; there is no
        // separate "summary" field. So we use `::new("tool", content)`
        // (not `::tool(summary, full)`) and the chat widget will
        // render the full content verbatim. This is the documented
        // forward-compat path for entries without a tool_output
        // sidecar.
        let msgs = vec![tool_msg("command output here")];
        let entries = messages_to_entries(&msgs);
        assert_eq!(entries[0].role, "tool");
        assert_eq!(entries[0].content, "command output here");
        // Critical: no sidecar. The chat widget's
        // `tool_should_collapse` check is `entry.tool_output.is_some()`.
        assert!(entries[0].tool_output.is_none());
    }

    #[test]
    fn messages_to_entries_preserves_order() {
        let msgs = vec![
            user_msg("u1"),
            assistant_msg("a1"),
            tool_msg("t1"),
            user_msg("u2"),
        ];
        let entries = messages_to_entries(&msgs);
        assert_eq!(entries.len(), 4);
        assert_eq!(entries[0].role, "user");
        assert_eq!(entries[1].role, "assistant");
        assert_eq!(entries[2].role, "tool");
        assert_eq!(entries[3].role, "user");
        assert_eq!(entries[3].content, "u2");
    }

    #[test]
    fn messages_to_entries_does_not_clone_tool_calls_or_thinking() {
        // The conversion drops optional fields (thinking, tool_calls,
        // tool_call_id, tool_name, token_count) — those are model-API
        // details, not display details. The chat widget only needs
        // role + content. This test pins down that contract: if
        // somebody later decides to surface `thinking` in the UI,
        // they have to add it to ConversationEntry first, not rely
        // on this function preserving it.
        let mut m = tool_msg("output");
        m.thinking = Some("I should call the bash tool".to_string());
        m.tool_calls = Some(vec![crate::shared::ToolInvocation {
            id: "call_1".into(),
            name: "bash".into(),
            arguments: serde_json::json!({"command": "ls"}),
        }]);
        m.token_count = Some(42);

        let entries = messages_to_entries(&[m]);
        assert_eq!(entries[0].role, "tool");
        assert_eq!(entries[0].content, "output");
        assert!(entries[0].tool_output.is_none());
        // ConversationEntry doesn't have `thinking` / `tool_calls`
        // fields, so there's nothing to assert on them — they are
        // dropped by construction. The test documents the contract.
    }

    #[test]
    fn messages_to_entries_round_trip_through_conversation_log() {
        // Integration smoke test: serialize a few messages to NDJSON
        // (mimicking what ConversationLog does on disk), read them
        // back, and confirm the conversion is lossless on the
        // (role, content) tuple.
        let tmp = std::env::temp_dir().join("kf_test_messages_round_trip.ndjson");
        let _ = std::fs::remove_file(&tmp);

        let original = vec![
            user_msg("hello"),
            assistant_msg("hi there"),
            tool_msg("ls: file.txt"),
        ];

        // Write
        {
            use std::io::Write;
            let mut f = std::fs::File::create(&tmp).unwrap();
            for m in &original {
                writeln!(f, "{}", serde_json::to_string(m).unwrap()).unwrap();
            }
        }

        // Read
        let content = std::fs::read_to_string(&tmp).unwrap();
        let parsed: Vec<Message> = content
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();

        let entries = messages_to_entries(&parsed);
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].role, "user");
        assert_eq!(entries[0].content, "hello");
        assert_eq!(entries[1].role, "assistant");
        assert_eq!(entries[2].role, "tool");
        assert_eq!(entries[2].content, "ls: file.txt");

        let _ = std::fs::remove_file(&tmp);
    }

    // ── format_status_block tests ─────────────────────────────────
    //
    // The helper is pure (no I/O, no async) so we can drive it with
    // a hand-rolled AppState. The fields we set:
    //   - model_info: Option<ModelInfo>  — drives the "Model:" section
    //   - last_turn_prompt_tokens / tokens_sent / tokens_received
    //   - cumulative_cost / turn_cost
    //
    // We avoid touching `session_started` (Instant::now() is fine for
    // the production path; the elapsed string is hard to pin down in
    // a test without flakiness — we don't assert on it).

    fn make_state_with_model(name: &str, max_ctx: usize) -> AppState {
        let mut s = AppState::new(Config::default());
        s.model_info = Some(ModelInfo {
            name: name.to_string(),
            supports_thinking: true,
            tool_call_format: ToolCallStyle::Native,
            max_context_tokens: max_ctx,
            recommended_temperature: 0.7,
        });
        s
    }

    /// No model connected: the "Model:" line says so, the "Context:"
    /// line says "no turn yet", and no recommendation is printed
    /// (the helper bails on `max_ctx == 0`).
    #[test]
    fn format_status_block_no_model_shows_placeholder() {
        let s = AppState::new(Config::default());
        let out = format_status_block(&s);
        assert!(out.contains("Session status"));
        assert!(out.contains("(not connected)"));
        assert!(out.contains("Context:"));
        assert!(out.contains("no turn yet"));
        // No "consider /compact" line because we have no max_ctx
        assert!(!out.contains("consider /compact"));
        assert!(!out.contains("Context is comfortable"));
    }

    /// Comfortable context (< 50%): the recommendation is the
    /// ✅ comfortable line, the percentage is in the green band.
    #[test]
    fn format_status_block_comfortable_recommends_nothing() {
        let mut s = make_state_with_model("deepseek-v4-flash:cloud", 100_000);
        s.last_turn_prompt_tokens = 30_000; // 30%
        let out = format_status_block(&s);
        assert!(out.contains("deepseek-v4-flash:cloud"));
        assert!(out.contains("30.0K/100.0K (30%)"));
        assert!(out.contains("Context is comfortable"));
        assert!(!out.contains("consider /compact"));
    }

    /// Tight context (50–80%): the recommendation is the
    /// 💡 filling-up hint. The percentage is in the yellow band.
    #[test]
    fn format_status_block_tight_recommends_considering_compact() {
        let mut s = make_state_with_model("glm-5.1:cloud", 100_000);
        s.last_turn_prompt_tokens = 60_000; // 60%
        let out = format_status_block(&s);
        assert!(out.contains("60.0K/100.0K (60%)"));
        assert!(out.contains("filling up"));
        assert!(!out.contains("consider /compact")); // this is the > 80% line
    }

    /// Critical context (>= 80%): the recommendation is the
    /// ⚠️ tight / consider /compact line. The percentage is in
    /// the red band.
    #[test]
    fn format_status_block_critical_recommends_compact_now() {
        let mut s = make_state_with_model("kimi:cloud", 128_000);
        s.last_turn_prompt_tokens = 110_000; // 85.9%
        let out = format_status_block(&s);
        assert!(out.contains("110.0K/128.0K (85%)"));
        assert!(out.contains("Context is tight"));
        assert!(out.contains("consider /compact"));
    }

    /// Cost and token counters are formatted to the spec.
    #[test]
    fn format_status_block_shows_cost_and_tokens() {
        let mut s = make_state_with_model("test", 100_000);
        s.tokens_sent = 123_456;
        s.tokens_received = 78_901;
        s.cumulative_cost = 0.0042;
        s.turn_cost = 0.0011;
        s.last_turn_prompt_tokens = 0; // no context-pressure line
        let out = format_status_block(&s);
        assert!(out.contains("$0.0042 cumulative"));
        assert!(out.contains("$0.0011 this turn"));
        assert!(out.contains("123.5K sent")); // format_token_count formatting
        assert!(out.contains("78.9K received"));
        // No context line when last_turn_prompt_tokens == 0
        assert!(!out.contains("(last turn)"));
    }

    /// Tool-call style enum is rendered as a human-readable string
    /// (not the Rust Debug form like "Native" — already fine, but
    /// `OpenAiCompat` would render as `OpenAiCompat` in lowercase
    /// form. Test the OpenAI-compat path explicitly to pin it down).
    #[test]
    fn format_status_block_renders_openai_compat_tool_style() {
        let mut s = AppState::new(Config::default());
        s.model_info = Some(ModelInfo {
            name: "qwen2.5:0.5b".to_string(),
            supports_thinking: false,
            tool_call_format: ToolCallStyle::OpenAiCompat,
            max_context_tokens: 32_000,
            recommended_temperature: 0.5,
        });
        let out = format_status_block(&s);
        assert!(out.contains("Tool calls:   openai-compat"));
        assert!(out.contains("Thinking:     no"));
    }

    // ── /jobs helpers: format_job_status + tail_lines ───────────
    //
    // Both are pure (no I/O, no async) so we can drive them with
    // hand-built BashJob values. The full `handle_jobs_command` flow
    // is integration-tested by the bash_jobs module's own tests; the
    // tests here pin down the *format* contract that the list view,
    // the detail view, and the completion notifier all depend on.

    use crate::session::bash_jobs::{BashJob, JobStatus};

    fn dummy_job(id: u64, status: JobStatus) -> BashJob {
        BashJob {
            id,
            command: "echo hi".to_string(),
            status,
            stdout: String::new(),
            stderr: String::new(),
            started_at: chrono::Local::now(),
            finished_at: None,
        }
    }

    /// All four status variants render as `#<id>` (no `(id=...)` form).
    /// The old code had `(id=5)` for Running and `#5` for the others —
    /// this test pins down the consolidated form so the list, detail,
    /// and notifier views stay consistent.
    #[test]
    fn format_job_status_running_uses_hash_form() {
        let j = dummy_job(5, JobStatus::Running);
        let s = format_job_status(&j);
        assert!(s.contains("#5"), "got: {}", s);
        assert!(!s.contains("(id="), "old format leaked: {}", s);
        assert!(s.contains("running"), "got: {}", s);
    }

    #[test]
    fn format_job_status_completed_includes_exit_code() {
        let j = dummy_job(7, JobStatus::Completed(0));
        let s = format_job_status(&j);
        assert!(s.contains("#7"));
        assert!(s.contains("completed"));
        assert!(s.contains("exit 0"));
    }

    #[test]
    fn format_job_status_failed_includes_error_text() {
        let j = dummy_job(9, JobStatus::Failed("killed".into()));
        let s = format_job_status(&j);
        assert!(s.contains("#9"));
        assert!(s.contains("failed"));
        assert!(s.contains("killed"));
    }

    #[test]
    fn format_job_status_cancelled_is_distinct_from_failed() {
        let cancelled = format_job_status(&dummy_job(11, JobStatus::Cancelled));
        let failed = format_job_status(&dummy_job(11, JobStatus::Failed("x".into())));
        assert!(cancelled.contains("cancelled"));
        assert!(!cancelled.contains("failed"));
        assert_ne!(cancelled, failed);
    }

    /// `tail_lines("", n)` returns the empty input unchanged with
    /// 0 elided — guards the early-return path in `tail_lines`.
    #[test]
    fn tail_lines_empty_input_returns_empty() {
        let (out, elided) = tail_lines("", 10);
        assert_eq!(out, "");
        assert_eq!(elided, 0);
    }

    /// When the input is shorter than the limit, no elision happens.
    #[test]
    fn tail_lines_short_input_unchanged() {
        let (out, elided) = tail_lines("a\nb\nc", 10);
        assert_eq!(out, "a\nb\nc");
        assert_eq!(elided, 0);
    }

    /// When the input is longer, the LAST `n` lines survive and the
    /// count of dropped leading lines is reported.
    #[test]
    fn tail_lines_long_input_keeps_tail_and_reports_elided() {
        let input = (1..=100)
            .map(|i| format!("line{}", i))
            .collect::<Vec<_>>()
            .join("\n");
        let (out, elided) = tail_lines(&input, 5);
        assert_eq!(elided, 95);
        // Tail should be the last 5 lines joined
        assert_eq!(out, "line96\nline97\nline98\nline99\nline100");
        // The first 95 should be gone
        assert!(!out.contains("line1\n"));
        assert!(!out.contains("line50"));
    }

    /// The boundary at exactly `n` lines: no elision reported.
    #[test]
    fn tail_lines_exact_boundary_no_elision() {
        let input = (1..=5).map(|i| format!("L{}", i)).collect::<Vec<_>>().join("\n");
        let (out, elided) = tail_lines(&input, 5);
        assert_eq!(out, input);
        assert_eq!(elided, 0);
    }

    // ── handle_jobs_command integration tests ───────────────────
    //
    // These tests hit the real `global_registry()`. They run in
    // parallel with the rest of the test suite by default, and
    // cargo's test harness runs them across multiple threads. The
    // global registry is shared, so a `clean()` call from one test
    // would otherwise race with a `spawn()` + `get()` from another.
    //
    // The 5 tests that MUTATE the registry (`spawn` or `clean`)
    // therefore acquire `TEST_REGISTRY_LOCK` for their whole
    // body. The 4 read-only tests (no spawn, no clean) leave it
    // alone — they only assert on "not found" / usage messages
    // and tolerate whatever state the registry is in.
    //
    // We use a `OnceLock<tokio::sync::Mutex<()>>` rather than
    // `std::sync::Mutex` because the tests are async — the
    // `tokio::sync::Mutex` lets us hold the guard across `.await`
    // points. The `OnceLock` initialises the lock lazily on first
    // access; the inner value is the lock itself.
    //
    // Locking order is irrelevant because all 5 mutating tests
    // acquire only this one lock. Holding it for the whole test
    // body means each mutating test runs effectively serialized
    // relative to the others, but the 4 read-only tests can still
    // race in parallel — which is fine because they only observe.

    use std::sync::OnceLock;
    static TEST_REGISTRY_LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
    fn test_registry_lock() -> &'static tokio::sync::Mutex<()> {
        TEST_REGISTRY_LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
    }

    /// `/jobs` with no args includes the job we just spawned. The
    /// marker we look for is the unique command string, so this test
    /// is robust against other tests' jobs being in the registry.
    #[tokio::test]
    async fn handle_jobs_command_list_includes_spawned_job() {
        let _guard = test_registry_lock().lock().await;
        let registry = crate::session::bash_jobs::global_registry();
        let unique = format!("echo kf_test_list_{}", std::process::id());
        let id = registry.spawn(&unique, None, None).await.unwrap();
        for _ in 0..40 {
            let job = registry.get(id).await.unwrap();
            if !matches!(job.status, JobStatus::Running) {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }

        let out = handle_jobs_command("").await;
        assert!(out.starts_with("Background jobs:"), "got: {}", out);
        assert!(out.contains(&unique), "unique cmd missing: {}", out);
        assert!(out.contains(&format!("#{}", id)), "job id missing: {}", out);
    }

    /// `/jobs <id>` for a finished job shows the full output.
    #[tokio::test]
    async fn handle_jobs_command_detail_shows_stdout() {
        let _guard = test_registry_lock().lock().await;
        let registry = crate::session::bash_jobs::global_registry();
        let unique = format!("echo kf_test_detail_{}", std::process::id());
        let id = registry.spawn(&unique, None, None).await.unwrap();
        for _ in 0..40 {
            let job = registry.get(id).await.unwrap();
            if !matches!(job.status, JobStatus::Running) {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }

        let out = handle_jobs_command(&id.to_string()).await;
        assert!(out.contains(&format!("#{}", id)), "got: {}", out);
        assert!(out.contains("Command:"), "got: {}", out);
        assert!(out.contains("Started:"), "got: {}", out);
        assert!(out.contains("Finished:"), "got: {}", out);
        assert!(out.contains("stdout"), "got: {}", out);
        assert!(out.contains(&unique), "stdout missing unique: {}", out);
    }

    /// `/jobs <id>` for a non-existent id returns a "not found" message
    /// that lists available job ids. Uses a high id (999_999) that
    /// is extremely unlikely to collide.
    #[tokio::test]
    async fn handle_jobs_command_detail_unknown_id_says_not_found() {
        let out = handle_jobs_command("999999").await;
        assert!(out.contains("not found"), "got: {}", out);
    }

    /// `/jobs foo` (non-numeric, non-"clean") returns a usage hint.
    #[tokio::test]
    async fn handle_jobs_command_unknown_subcommand_returns_usage() {
        let out = handle_jobs_command("foo").await;
        assert!(out.contains("Usage"), "got: {}", out);
        assert!(out.contains("/jobs foo"), "got: {}", out);
    }

    /// `/jobs clean` returns either "cleaned N" or "nothing to clean"
    /// depending on what other tests left behind. Both are valid
    /// outcomes for the global registry. The point is that the
    /// command does not panic and does not return an error string.
    #[tokio::test]
    async fn handle_jobs_command_clean_is_idempotent() {
        let _guard = test_registry_lock().lock().await;
        let out = handle_jobs_command("clean").await;
        assert!(
            out.contains("Cleaned") || out.contains("No completed jobs"),
            "got: {}",
            out
        );
    }

    /// `/jobs <id> cancel` for a running job returns a cancellation
    /// confirmation. Uses a long-running `sleep` so the job is still
    /// in `Running` state when we call cancel.
    #[tokio::test]
    async fn handle_jobs_command_cancel_running_job_succeeds() {
        let _guard = test_registry_lock().lock().await;
        let registry = crate::session::bash_jobs::global_registry();
        // The `unique` stamp goes in a shell comment so `sleep`
        // gets exactly one argument. Without the `#`, `sleep` would
        // treat `kf_test_cancel_<pid>` as a second time-interval
        // and error out before the 5s elapse.
        let unique = format!("sleep 5  # kf_test_cancel_{}", std::process::id());
        let id = registry.spawn(&unique, None, None).await.unwrap();
        // Give the spawned process a moment to register as Running.
        // No `break` here — we want the job to still be running when
        // we cancel it.
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        let out = handle_jobs_command(&format!("{} cancel", id)).await;
        assert!(out.contains("Cancel"), "got: {}", out);
        assert!(out.contains(&format!("#{}", id)), "got: {}", out);

        // Verify the job is now in Cancelled status (best-effort:
        // a `sleep 5` should still be running when we cancel, but
        // the kill might race with natural completion in CI).
        let job = registry.get(id).await;
        if let Some(j) = job {
            assert!(
                matches!(j.status, JobStatus::Cancelled | JobStatus::Failed(_)),
                "expected cancelled or failed, got: {:?}",
                j.status
            );
        }
    }

    /// `/jobs <id> cancel` for an already-completed job returns a
    /// "not running" error pointing the user at the actual status.
    #[tokio::test]
    async fn handle_jobs_command_cancel_finished_job_returns_error() {
        let _guard = test_registry_lock().lock().await;
        let registry = crate::session::bash_jobs::global_registry();
        let unique = format!("echo kf_test_cancel_done_{}", std::process::id());
        let id = registry.spawn(&unique, None, None).await.unwrap();
        // Wait for it to finish.
        for _ in 0..40 {
            let job = registry.get(id).await.unwrap();
            if !matches!(job.status, JobStatus::Running) {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }

        let out = handle_jobs_command(&format!("{} cancel", id)).await;
        assert!(out.contains("not running"), "got: {}", out);
    }

    /// `/jobs <id> cancel` for a non-existent id returns a "not found"
    /// message. Uses a high id (999_999) to avoid collision.
    #[tokio::test]
    async fn handle_jobs_command_cancel_unknown_id_returns_not_found() {
        let out = handle_jobs_command("999999 cancel").await;
        assert!(out.contains("not found"), "got: {}", out);
    }

    /// `/jobs cancel` (no id) returns a usage hint. Without the id
    /// there's nothing to cancel.
    #[tokio::test]
    async fn handle_jobs_command_cancel_without_id_returns_usage() {
        let out = handle_jobs_command("cancel").await;
        assert!(out.contains("Usage"), "got: {}", out);
    }

    /// `/jobs <id> foo` (unknown sub-command) returns a usage hint.
    /// We don't want silent typos like `/jobs 5 stop` to cancel
    /// anything — they should fail loudly.
    #[tokio::test]
    async fn handle_jobs_command_cancel_unknown_subcommand_returns_usage() {
        let out = handle_jobs_command("5 foo").await;
        assert!(out.contains("Usage"), "got: {}", out);
        assert!(out.contains("foo"), "got: {}", out);
    }

    /// `/jobs 5 cancel now` (extra token past cancel) returns a usage
    /// hint. Pin this down to prevent silent ignoring of trailing args.
    #[tokio::test]
    async fn handle_jobs_command_cancel_extra_token_returns_usage() {
        let out = handle_jobs_command("5 cancel now").await;
        assert!(out.contains("Usage"), "got: {}", out);
    }

    // ========================================================================
    // v1.2-p14 — `!` bash passthrough tests
    // ========================================================================

    // ----- format_bang_output (pure) -----

    /// A successful `BangResult` with no stderr renders as:
    /// - `$ <cmd>` header
    /// - `✅ exit 0 in <elapsed>` banner
    /// - stdout verbatim below
    fn sample_success(cmd: &str, stdout: &str, ms: u64) -> BangResult {
        BangResult {
            cmd: cmd.to_string(),
            exit_code: 0,
            stdout: stdout.to_string(),
            stderr: String::new(),
            timed_out: false,
            elapsed_ms: ms,
        }
    }

    #[test]
    fn format_bang_output_success_includes_command_and_banner() {
        let r = sample_success("echo hi", "hi\n", 42);
        let out = format_bang_output(&r);
        assert!(out.starts_with("$ echo hi"), "got: {:?}", out);
        assert!(out.contains("✅ exit 0"), "got: {:?}", out);
        assert!(out.contains("hi"), "got: {:?}", out);
        // sub-second shows ms granularity
        assert!(out.contains("42ms"), "got: {:?}", out);
    }

    /// A non-zero exit code surfaces a `❌` banner and includes the code.
    /// This is critical: a silent success on `cargo build` that actually
    /// exited 1 would be very confusing.
    #[test]
    fn format_bang_output_failure_includes_exit_code_and_stderr() {
        let r = BangResult {
            cmd: "ls /nonexistent".to_string(),
            exit_code: 2,
            stdout: String::new(),
            stderr: "ls: /nonexistent: No such file or directory\n".to_string(),
            timed_out: false,
            elapsed_ms: 12,
        };
        let out = format_bang_output(&r);
        assert!(out.contains("$ ls /nonexistent"), "got: {:?}", out);
        assert!(out.contains("❌ exit 2"), "got: {:?}", out);
        assert!(out.contains("⚠ stderr:"), "got: {:?}", out);
        assert!(out.contains("No such file or directory"), "got: {:?}", out);
    }

    /// A timed-out command shows the `⏰` icon and the timeout duration.
    /// Stdout/stderr are not rendered separately (the process was killed).
    #[test]
    fn format_bang_output_timed_out_shows_clock_icon() {
        let r = BangResult {
            cmd: "sleep 99".to_string(),
            exit_code: -1,
            stdout: String::new(),
            stderr: String::new(),
            timed_out: true,
            elapsed_ms: 30_000,
        };
        let out = format_bang_output(&r);
        assert!(out.contains("⏰"), "got: {:?}", out);
        assert!(out.contains("30s"), "got: {:?}", out);
        assert!(!out.contains("✅"), "got: {:?}", out);
        assert!(!out.contains("❌"), "got: {:?}", out);
    }

    /// Success with both stdout and stderr: stderr still gets a `⚠ stderr:`
    /// marker even though the command exited 0. Many real tools (cargo,
    /// npm, gcc) write progress to stderr; this lets the user see it
    /// distinctly from stdout.
    #[test]
    fn format_bang_output_success_with_stderr_separates_them() {
        let r = BangResult {
            cmd: "cargo build 2>&1 >/dev/null".to_string(),
            exit_code: 0,
            stdout: String::new(),
            stderr: "Compiling foo v0.1.0\nFinished release in 1.2s\n".to_string(),
            timed_out: false,
            elapsed_ms: 1200,
        };
        let out = format_bang_output(&r);
        assert!(out.contains("✅ exit 0"), "got: {:?}", out);
        assert!(out.contains("⚠ stderr:"), "got: {:?}", out);
        assert!(out.contains("Compiling foo"), "got: {:?}", out);
    }

    /// Empty stdout + empty stderr: just the banner, no extra blank line.
    /// Common case for `!true`, `!cd` (silent), `!export FOO=bar`.
    #[test]
    fn format_bang_output_empty_output_is_just_banner() {
        let r = sample_success("true", "", 5);
        let out = format_bang_output(&r);
        assert!(out.contains("$ true"), "got: {:?}", out);
        assert!(out.contains("✅ exit 0"), "got: {:?}", out);
        // No stderr marker
        assert!(!out.contains("⚠"), "got: {:?}", out);
    }

    // ----- format_elapsed (pure) -----

    #[test]
    fn format_elapsed_subsecond_is_ms() {
        assert_eq!(format_elapsed(0), "0ms");
        assert_eq!(format_elapsed(1), "1ms");
        assert_eq!(format_elapsed(999), "999ms");
    }

    #[test]
    fn format_elapsed_subminute_is_seconds_with_decimals() {
        assert_eq!(format_elapsed(1000), "1.00s");
        assert_eq!(format_elapsed(1420), "1.42s");
        assert_eq!(format_elapsed(12_345), "12.35s"); // banker-ish rounding
        assert_eq!(format_elapsed(59_999), "60.00s");
    }

    #[test]
    fn format_elapsed_minute_plus_uses_minute_second_format() {
        assert_eq!(format_elapsed(60_000), "1m00s");
        assert_eq!(format_elapsed(90_000), "1m30s");
        assert_eq!(format_elapsed(125_000), "2m05s");
    }

    // ----- is_success (pure) -----

    #[test]
    fn bang_result_is_success_only_for_zero_exit_no_timeout() {
        assert!(sample_success("x", "", 0).is_success());
        assert!(!BangResult { exit_code: 1, timed_out: false, ..sample_success("x", "", 0) }
            .is_success());
        assert!(!BangResult { exit_code: 0, timed_out: true, ..sample_success("x", "", 0) }
            .is_success());
    }

    // ----- handle_bang_command (integration, real shell) -----

    /// `!echo hi` actually runs echo and produces "hi" in the output.
    /// This is the minimum end-to-end check: subprocess wiring works.
    #[tokio::test]
    async fn handle_bang_command_echo_runs() {
        let out = handle_bang_command("echo hi").await;
        assert!(out.contains("$ echo hi"), "got: {:?}", out);
        assert!(out.contains("✅ exit 0"), "got: {:?}", out);
        assert!(out.contains("hi"), "got: {:?}", out);
    }

    /// `!true` exits 0 with no output. Banner-only, no stderr marker.
    #[tokio::test]
    async fn handle_bang_command_true_exits_zero_silently() {
        let out = handle_bang_command("true").await;
        assert!(out.contains("✅ exit 0"), "got: {:?}", out);
        assert!(!out.contains("⚠"), "got: {:?}", out);
    }

    /// `!false` exits non-zero. Banner says `❌ exit 1`.
    #[tokio::test]
    async fn handle_bang_command_false_exits_nonzero() {
        let out = handle_bang_command("false").await;
        assert!(out.contains("❌ exit 1"), "got: {:?}", out);
    }

    /// `!` with nothing after it returns a usage hint, not a crash.
    /// Pin this so we never end up running `/bin/sh -c ""` and waiting
    /// for the timeout for no reason.
    #[tokio::test]
    async fn handle_bang_command_empty_returns_usage() {
        let out = handle_bang_command("").await;
        assert!(out.contains("Usage"), "got: {:?}", out);
    }

    /// Whitespace-only `!   ` is also "empty" for our purposes.
    #[tokio::test]
    async fn handle_bang_command_whitespace_only_returns_usage() {
        let out = handle_bang_command("   ").await;
        assert!(out.contains("Usage"), "got: {:?}", out);
    }

    // ── @-mention tests (v1.2-p15) ─────────────────────────────────

    /// `parse_mentions` returns an empty vec for prose with no @-tokens.
    #[test]
    fn parse_mentions_empty_for_prose() {
        let m = parse_mentions("hello world, no mentions here");
        assert!(m.is_empty());
    }

    /// A single @-mention is parsed with the right offsets.
    #[test]
    fn parse_mentions_single_token() {
        let m = parse_mentions("please review @src/main.rs carefully");
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].spec.path, "src/main.rs");
        assert_eq!(m[0].spec.range, None);
        assert!(!m[0].spec.raw);
        // "@" is at byte index 14: "please review " is 14 bytes
        // (p=0, l=1, e=2, a=3, s=4, e=5, ' '=6, r=7, e=8, v=9,
        //  i=10, e=11, w=12, ' '=13, @=14).
        assert_eq!(m[0].start, 14);
        // ends right before " carefully"
        assert_eq!(m[0].end, 14 + "@src/main.rs".len());
    }

    /// Multiple @-mentions in one input are all parsed, in order.
    #[test]
    fn parse_mentions_multiple_tokens() {
        let m = parse_mentions("look at @a.rs and @b/c.py:10-20:raw please");
        assert_eq!(m.len(), 2);
        assert_eq!(m[0].spec.path, "a.rs");
        assert_eq!(m[1].spec.path, "b/c.py");
        assert_eq!(m[1].spec.range, Some((10, 20)));
        assert!(m[1].spec.raw);
    }

    /// `@@` (double-@) is treated as literal text, not a mention.
    #[test]
    fn parse_mentions_double_at_is_literal() {
        let m = parse_mentions("send email to @@user");
        assert!(m.is_empty(), "got: {:?}", m);
    }

    /// A trailing `@` with nothing after it is not a mention.
    #[test]
    fn parse_mentions_trailing_at_ignored() {
        let m = parse_mentions("what about @");
        assert!(m.is_empty());
    }

    /// `@` followed immediately by whitespace is not a mention.
    #[test]
    fn parse_mentions_at_before_space_ignored() {
        let m = parse_mentions("try @ then run");
        assert!(m.is_empty());
    }

    /// `:raw` suffix is recognised and sets the raw flag.
    #[test]
    fn parse_mentions_raw_suffix() {
        let m = parse_mentions("inline @notes.md:raw now");
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].spec.path, "notes.md");
        assert!(m[0].spec.raw);
    }

    /// `START-END` range is parsed and must be 1-indexed.
    #[test]
    fn parse_mentions_range() {
        let m = parse_mentions("see @foo.rs:1-10");
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].spec.range, Some((1, 10)));
    }

    /// A range with start > end is rejected (returns literal text).
    #[test]
    fn parse_mentions_invalid_range_rejected() {
        let m = parse_mentions("see @foo.rs:10-5");
        assert!(m.is_empty(), "inverted range should be rejected, got: {:?}", m);
    }

    /// A range with non-numeric body is rejected.
    #[test]
    fn parse_mentions_non_numeric_range_rejected() {
        let m = parse_mentions("see @foo.rs:abc-def");
        assert!(m.is_empty());
    }

    /// `strip_mentions` removes the @-tokens and the whitespace after
    /// them, leaving the surrounding text intact.
    #[test]
    fn strip_mentions_removes_tokens() {
        let input = "look at @a.rs and @b.py now";
        let mentions = parse_mentions(input);
        let cleaned = strip_mentions(input, &mentions);
        assert_eq!(cleaned, "look at  and  now", "got: {:?}", cleaned);
    }

    /// `strip_mentions` is a no-op when there are no mentions.
    #[test]
    fn strip_mentions_no_mentions_unchanged() {
        let input = "nothing to strip here";
        let cleaned = strip_mentions(input, &parse_mentions(input));
        assert_eq!(cleaned, input);
    }

    /// `expand_mentions` reads a real file (via tempfile) and returns
    /// an `Ok` status with the byte count.
    #[test]
    fn expand_mentions_reads_real_file() {
        let tmp = tempfile::NamedTempFile::new().expect("tempfile");
        std::fs::write(tmp.path(), "hello world\n").expect("write");
        let spec = MentionSpec {
            path: tmp.path().to_string_lossy().to_string(),
            range: None,
            raw: true,
        };
        let token = MentionToken { spec, start: 0, end: 0 };
        let guard = crate::session::access::PathGuard::default();
        let expansions = expand_mentions(&[token], &guard);
        assert_eq!(expansions.len(), 1);
        assert!(expansions[0].is_ok());
        assert_eq!(expansions[0].content, "hello world");
    }

    /// `expand_mentions` returns `NotFound` for a missing path.
    #[test]
    fn expand_mentions_not_found() {
        let spec = MentionSpec {
            path: "/nonexistent/path/that/definitely/does/not/exist.rs".into(),
            range: None,
            raw: true,
        };
        let token = MentionToken { spec, start: 0, end: 0 };
        let guard = crate::session::access::PathGuard::default();
        let expansions = expand_mentions(&[token], &guard);
        assert!(matches!(expansions[0].status, MentionStatus::NotFound));
    }

    /// `expand_mentions` applies a 1-indexed line range.
    #[test]
    fn expand_mentions_range_filters_lines() {
        let tmp = tempfile::NamedTempFile::new().expect("tempfile");
        std::fs::write(tmp.path(), "line1\nline2\nline3\nline4\n").expect("write");
        let spec = MentionSpec {
            path: tmp.path().to_string_lossy().to_string(),
            range: Some((2, 3)),
            raw: true,
        };
        let token = MentionToken { spec, start: 0, end: 0 };
        let guard = crate::session::access::PathGuard::default();
        let expansions = expand_mentions(&[token], &guard);
        assert_eq!(expansions[0].content, "line2\nline3");
    }

    /// `expand_mentions` returns `InvalidRange` if start is 0.
    #[test]
    fn expand_mentions_invalid_range_zero_start() {
        let tmp = tempfile::NamedTempFile::new().expect("tempfile");
        std::fs::write(tmp.path(), "line1\nline2\n").expect("write");
        let spec = MentionSpec {
            path: tmp.path().to_string_lossy().to_string(),
            range: Some((0, 1)),
            raw: true,
        };
        let token = MentionToken { spec, start: 0, end: 0 };
        let guard = crate::session::access::PathGuard::default();
        let expansions = expand_mentions(&[token], &guard);
        assert!(matches!(expansions[0].status, MentionStatus::InvalidRange(_)));
    }

    /// `render_mentions_block` is empty when there are no expansions.
    #[test]
    fn render_mentions_block_empty_for_no_expansions() {
        assert_eq!(render_mentions_block(&[]), "");
    }

    /// `render_mentions_block` produces a fenced block per expansion.
    #[test]
    fn render_mentions_block_produces_fenced_code() {
        let e = MentionExpansion {
            spec: MentionSpec {
                path: "src/main.rs".into(),
                range: None,
                raw: true,
            },
            content: "fn main() {}".into(),
            status: MentionStatus::Ok { bytes: 12, minified: false, truncated: false },
        };
        let s = render_mentions_block(&[e]);
        assert!(s.contains("```"));
        assert!(s.contains("fn main()"));
        assert!(s.contains("src/main.rs"));
    }

    /// `render_mentions_block` renders a `not found` placeholder for
    /// failed expansions so the model can react.
    #[test]
    fn render_mentions_block_handles_not_found() {
        let e = MentionExpansion {
            spec: MentionSpec { path: "x.rs".into(), range: None, raw: true },
            content: String::new(),
            status: MentionStatus::NotFound,
        };
        let s = render_mentions_block(&[e]);
        assert!(s.contains("not found"), "got: {:?}", s);
    }

    /// `format_mention_status` is empty when there are no expansions.
    #[test]
    fn format_mention_status_empty_for_no_expansions() {
        assert_eq!(format_mention_status(&[]), "");
    }

    /// `format_mention_status` lists each mention with a status icon.
    #[test]
    fn format_mention_status_lists_with_icons() {
        let expansions = vec![
            MentionExpansion {
                spec: MentionSpec { path: "a.rs".into(), range: None, raw: true },
                content: "x".into(),
                status: MentionStatus::Ok { bytes: 1, minified: false, truncated: false },
            },
            MentionExpansion {
                spec: MentionSpec { path: "missing.rs".into(), range: None, raw: true },
                content: String::new(),
                status: MentionStatus::NotFound,
            },
        ];
        let s = format_mention_status(&expansions);
        assert!(s.contains("✓"));
        assert!(s.contains("✗"));
        assert!(s.contains("a.rs"));
        assert!(s.contains("missing.rs"));
    }

    /// Tilde expansion works in the path component.
    #[test]
    fn expand_mentions_tilde_expansion() {
        // We can't reliably create a file at $HOME in CI, so just
        // verify that the expansion function does NOT panic on a tilde
        // path and returns a NotFound or IoError (not a parse error).
        let spec = MentionSpec {
            path: "~/nonexistent_xyzzy_kf_test.rs".into(),
            range: None,
            raw: true,
        };
        let token = MentionToken { spec, start: 0, end: 0 };
        let guard = crate::session::access::PathGuard::default();
        let expansions = expand_mentions(&[token], &guard);
        // Either NotFound (if the home dir exists but the file doesn't)
        // or IoError (if the home dir is missing). Both are fine — the
        // important property is that tilde expansion didn't crash.
        assert!(matches!(
            expansions[0].status,
            MentionStatus::NotFound | MentionStatus::IoError(_)
        ));
    }
}
