# ADR-0018: Cron / scheduled jobs

- **Status:** Accepted
- **Date:** 2026-07-16

## Context

Users want KirkForge to perform recurring or deferred work without keeping a live TUI session open: nightly `cargo test`, weekly reports, one-off reminders, or a command that should run as soon as the daemon starts. A scheduled-job subsystem should be lightweight, persistent, and safe enough to leave running unattended.

## Decision

### Data model

Scheduled jobs live in `~/.local/share/kirkforge/jobs/<id>/`:

- `job.json` — mutable [`ScheduledJob`] record.
- `runs/<run_id>/run.json`, `stdout`, `stderr` — append-only run artifacts.

All artifact files are created with `0o600`; directories with `0o700`.

```rust
pub struct ScheduledJob {
    pub id: String,
    pub created_at: DateTime<Utc>,
    pub schedule: ScheduleSpec,
    pub kind: JobKind,
    pub enabled: bool,
    pub last_run: Option<JobRunSummary>,
    pub next_run: Option<DateTime<Utc>>,
}

pub enum ScheduleSpec {
    Cron(String),        // normalised to seconds field
    Once(DateTime<Utc>),
    Restart,
}

pub enum JobKind {
    Bash { command: String },
    Skill { name: String, args: Vec<String> },
}
```

### Schedule expressions

The `cron` crate parses 6-field cron. We accept:

- `@hourly`, `@daily`, `@weekly` — translated to standard cron strings.
- `@once <ISO-8601>` — one-shot, then disabled.
- `@restart` — run once when the daemon starts, then disabled.
- Raw 5-field cron — normalised by prepending `0 `.
- Raw 6-field cron — passed through unchanged.

A dedicated `cron` dependency is justified: correctly handling day-of-month and day-of-week interaction is non-trivial and not worth reimplementing.

### Scheduler daemon

`kirkforge jobd` runs as a detached Unix daemon (unless `--foreground`):

- Unix socket at `~/.local/share/kirkforge/jobd.sock` for `ping`/`reload`/`shutdown`.
- PID file at `~/.local/share/kirkforge/jobd.pid`.
- Sleeps until the nearest next run, wakes on signals (`SIGINT`/`SIGTERM`/`SIGHUP`) or socket messages.
- Recomputes `next_run` from the stored schedule on every wake and on startup, so missed runs after a restart are handled deterministically.
- Concurrency is bounded by `max_concurrent_scheduled_jobs`; additional due jobs queue until a slot frees.

The session daemon's existing `daemonize()` helper was moved to `src/daemon/mod.rs` so both daemons can share it.

### Bash execution and unattended safety

Bash jobs reuse the existing `bash_runner` safety gate (`check_bash_command_str`) and the `BashJobRegistry` spawn path. Because scheduled jobs run without a human in the loop, commands that would normally require interactive approval are rejected unless one of the following is true:

- A permission rule explicitly allows the command.
- `scheduled_bash_auto_approve = true` is set in config (default `false`).

Rejected runs are recorded as `Failure` with a clear message instead of blocking the daemon.

### Skill jobs

Skill jobs are accepted by the data model, stored, and listed, but their executor is intentionally stubbed in this ADR. Attempting to run a scheduled skill records `Failure: skill execution not yet implemented`. This lets the TUI and store surface land now without blocking on headless plugin/skill execution.

### TUI integration

New `/jobs` subcommands are namespaced under `/jobs schedule` and `/jobs scheduled` so the existing background-bash `/jobs` commands remain unchanged:

- `/jobs schedule <spec> bash <command>`
- `/jobs schedule <spec> skill <name> [args...]`
- `/jobs scheduled list`
- `/jobs scheduled cancel <id>`
- `/jobs run-now <id>`
- `/jobs logs <id>`

The TUI event loop polls the job store on every tick and emits a one-time system message for each finished scheduled run, tracking run IDs (not job IDs) so recurring cron jobs announce every run exactly once.

### Config additions

```toml
scheduled_bash_auto_approve = false
max_concurrent_scheduled_jobs = 4
```

Env overrides:

- `KIRKFORGE_SCHEDULED_BASH_AUTO_APPROVE`
- `KIRKFORGE_MAX_CONCURRENT_SCHEDULED_JOBS`

## Consequences

Negative:

- Adds a new top-level dependency (`cron`).
- Adds a persistent background daemon that must be started (or auto-started) for scheduling to be active.
- Scheduled skill jobs are not yet executable; the TUI can create them but they will fail until a future ADR implements headless skill invocation.

Positive:

- Cron parsing is correct and well-tested without growing the codebase.
- Bash scheduled jobs get the same dangerous-pattern and deny-list protection as interactive bash.
- The existing `/jobs` background-bash surface is untouched; scheduled jobs live under a clear namespace.
- Persistent artifacts with restrictive permissions keep run logs private by default.
