# ADR 014: Background Bash Jobs

## Status

Accepted

## Date

2026-06-24

## Context

The model frequently needs to run long-lived commands — builds, test suites, searches, long-running servers — without blocking the turn loop. Users also want a way to kick off a command and check on it later, similar to terminal job control.

We already had a foreground `bash` tool capped at 30 seconds. A background variant lets the agent return immediately and the user query status later via `/jobs`.

## Decision

Add a global `BashJobRegistry` that spawns `tokio::process::Command` instances as background tasks, tracks their status/output, and exposes slash commands for listing, inspecting, and cancelling jobs.

### Trigger

The `bash` tool accepts `"background": true` in its JSON arguments. When set, the tool registers a background job instead of waiting for the command to finish.

### Safety gate

Every background spawn must pass the same `check_bash_command_str` gate used by foreground bash:

- deny-list check for paths and URLs
- dangerous-pattern block (e.g. `rm -rf /`)
- metadata-endpoint block
- sandbox workdir validation through `PathGuard`

Without this gate, `"background": true` would be an immediate bypass around the foreground safety checks.

### Registry design

- Singleton `BashJobRegistry` accessed via `global_registry()`.
- Jobs are identified by a monotonic `u64` id.
- Each job stores command, status (`Running`, `Completed`, `Failed`, `Cancelled`), stdout/stderr (capped at 1 MiB per stream), and timestamps.
- Child process handles are stored separately so `cancel()` can kill the process group.
- Cap of 64 concurrent jobs; oldest completed jobs are evicted when the registry fills.

### Output capture

Stdout and stderr are drained by spawned tasks using the same `CappedReader` used by foreground bash, so a background `cat /dev/urandom` cannot OOM the agent. Drain joins are wrapped in a short timeout to prevent a misbehaving child from wedging cleanup.

### Cancellation / timeout

- `cancel()` kills the process group and sets status to `Cancelled`.
- An optional per-job timeout kills the process group when elapsed.
- A short `child.wait()` timeout reaps zombies after cancellation or timeout.

### TUI surface

- `/jobs` lists all jobs.
- `/jobs <id>` shows the tail of stdout/stderr.
- `/jobs <id> cancel` cancels a running job.
- `/jobs clean` drops finished jobs.
- The TUI event loop calls `notify_completed_jobs` on each tick to append completion messages for newly finished jobs.

## Consequences

**Positive:**
- Long commands no longer block the agent loop.
- Users can monitor progress and cancel runaway jobs.
- Background and foreground bash share a single safety gate, so the same rules apply.

**Negative / limitations:**
- Job output is capped and not streamed into the live chat; the user must check `/jobs`.
- The registry is in-memory only; restarting the CLI loses job history.
- Maximum 64 jobs; very busy sessions may evict finished jobs before the user reads them.

## Implementation

- `src/session/bash_jobs.rs` — `BashJobRegistry`, `BashJob`, `JobStatus`, `global_registry`.
- `src/tools/bash.rs` — `background` parameter and dispatch to the registry.
- `src/tui/commands/jobs.rs` — `/jobs` slash-command handler and completion notifier.
- `src/tui/mod.rs` — event-loop integration for `notify_completed_jobs`.
