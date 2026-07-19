# ADR-025: Windows parity approach

- **Status:** Accepted
- **Date:** 2026-07-19

## Context

KirkForge-Cli is primarily a Unix-native tool: it uses Unix domain sockets for
daemon IPC, `setsid()` for background daemon detachment, `SIGHUP` for config
hot-reload, `/dev/tty` for line-mode approval prompts, and Unix process groups
for subprocess cleanup. Windows has none of these primitives in the same form.
The project already compiles for Windows (`x86_64-pc-windows-msvc`) because
most Unix-specific code is `#[cfg(unix)]` gated, but several features silently
behave differently or are unsupported.

The goal of this ADR is "works on Windows", not "optimised for Windows". Every
Unix-only feature must either have a Windows equivalent or be documented as a
limitation with a clear workaround or flag.

## Decision

### Approval reader (`src/main/mod.rs`)

- Unix: polls `/dev/tty` with `poll(2)` so the reader thread joins promptly on
  shutdown.
- Windows: races a `tokio::task::spawn_blocking` stdin reader against a
  `tokio::time::interval` poll of a shutdown flag. If shutdown fires before the
  user answers, the blocking task is aborted and the function returns `None`
  (interrupted). This keeps the outer approval handler joinable without
  blocking forever on a Windows console read.
- Other platforms: return `Some(false)` with a warning.

### Process groups (`src/session/process_group.rs`)

- Unix: `setpgid` + `killpg` ensures subprocess trees are terminated together.
- Windows: no equivalent through `std::process`. `setup_process_group` is a
  no-op and `kill_process_group` falls back to `Child::start_kill()`. This is
  documented as a best-effort limitation; a child that spawns its own
  grandchildren may outlive the parent on Windows.

### Session daemon (`src/daemon/mod.rs`, `src/daemon/server.rs`)

- Unix: `setsid()` detaches the daemon from the controlling terminal, and a
  Unix-domain socket provides IPC.
- Windows: `daemonize()` returns a clear error instructing the user to use
  `--foreground` if they really want a server process. Session discovery falls
  back to the file-based session index; `--auto-resume`, `--attach`, and the
  TUI startup picker degrade gracefully to "no recent sessions" behavior.
  Documented as a known limitation.

### Scheduled-job daemon (`src/jobs/daemon.rs`, `src/jobs/client.rs`)

- Unix: runs as a background daemon over a Unix-domain socket.
- Windows: `kirkforge jobd` returns a clear unsupported-platform error. The
  underlying job store still works, so scheduled jobs can be created and
  inspected, but they cannot run unattended on Windows.

### SIGHUP config hot-reload (`src/tui/mod.rs`)

- Unix: installs a `tokio::signal::unix::SignalKind::hangup()` handler that
  reloads `config.toml` into the shared config and forwards it to the executor.
- Windows: `SIGHUP` does not exist. Users reload config with the `/reload`
  slash command (TUI) or by restarting the process (line mode). Documented as
  the Windows equivalent.

### `/dev/tty` fallback

- Unix: line-mode approval reads from `/dev/tty` so it does not race with stdin
  prompt reading.
- Windows: reads from stdin, relying on the line-mode main loop being suspended
  while a tool awaits approval.

### Bash runner (`src/session/bash_runner/mod.rs`)

- Windows targets `bash` (Git for Windows / WSL) so the same deny-list and
  safety-token logic applies. `cmd.exe` is intentionally not used because it
  would bypass the safety gate. Documented.

## Consequences

Positive:

- The CLI is usable on Windows for the core chat/edit/run/sessions workflow.
- Every missing Unix feature is explicit rather than silently broken.
- The release workflow produces Windows artifacts so users do not need to
  build from source.

Negative:

- Background daemons and Unix-socket IPC are not available on Windows.
- Subprocess cleanup is less thorough on Windows.
- SIGHUP-based automation for reloading config does not work; users must use
  `/reload`.

## Implementation notes

- `README.md` § Platform notes lists each limitation and the workaround/flag.
- `.github/workflows/ci.yml` adds a `windows-latest` job running
  `cargo test --locked --workspace --no-fail-fast`.
- `src/main/mod.rs` uses `#[cfg(windows)]` for the pollable approval reader.
- `src/daemon/mod.rs` and `src/jobs/daemon.rs` use `#[cfg(not(unix))]` stubs
  that return clear errors.

ponytail: "works on Windows" means the CLI compiles, passes tests, and runs the
non-daemon interactive workflow. It does not mean every Unix power feature is
reimplemented. Each gap is documented rather than silently disabled.

upgrade path: existing Linux/macOS users are unaffected. Windows users can
install the release zip and use the same `kirkforge run` workflow; daemon and
scheduled-job features remain unavailable until a future ADR decides otherwise.
