# ADR 008: Session Daemon — Metadata-Only Background Process for Fast Resume

## Status

Accepted

## Date

2026-06-22

## Context

KirkForge sessions are persisted as `*.conv.ndjson` files under `~/.local/share/kirkforge/sessions/`. Every session has a stable id like `2026-06-22-session-01` derived from its filename. The conversation log survives terminal death, but before this ADR there was no fast way to discover or resume the most recent sessions: the user had to remember the id, the full path, or run `/sessions` inside the TUI.

A full tmux-style detach/reattach would require moving the executor, TUI event loop, and LLM streaming state into a background server and rebuilding the frontend as a thin client. That is a large architectural change with many unknowns and would introduce significant complexity around state synchronisation and concurrent access to the same conversation log.

## Decision

Add a **lightweight metadata-only daemon** that tracks the last few session files and provides a fast resume path. The daemon does **not** run the TUI, executor, or LLM; it only owns session metadata.

### Scope

- Remember the last **5** sessions (`RECENT_SESSIONS_LIMIT = 5`).
- Resolve a session id or prefix to a log path.
- Auto-start on demand when a resume-related request is made.
- Provide both CLI flags (`--auto-resume`, `--attach <id>`) and a TUI picker at startup and via `/resume`.

### Protocol

Line-delimited JSON-RPC over a Unix domain socket at `~/.local/share/kirkforge/daemon.sock`.

| Request | Response | Purpose |
|---------|----------|---------|
| `{"op":"ping"}` | `{"status":"ok"}` | Health check |
| `{"op":"list"}` | `{"status":"ok","data":{"sessions":[...]}}` | Last 5 sessions newest-first |
| `{"op":"resolve","id":"<prefix>"}` | `{"status":"ok","data":{"id":"...","path":"..."}}` | Resolve id/prefix → log path |
| `{"op":"touch","id":"...","path":"..."}` | `{"status":"ok"}` | Mark session as recently used |
| `{"op":"shutdown"}` | `{"status":"ok"}` | Stop daemon |

Session entries mirror `SessionEntry` from `session_index`: `id`, `path`, `started_at`, `message_count`, `size_bytes`.

### Daemon lifecycle

- `kirkforge daemon [--foreground|--stop]` subcommand.
- Default (Unix): re-exec into a background foreground daemon and exit the parent.
- `--foreground`: stay attached to the terminal for debugging.
- `--stop`: send `shutdown` to the running daemon and clean up stale socket/pid files.
- Socket and PID files are removed on clean shutdown (SIGINT, SIGHUP, SIGTERM, or `shutdown` op).
- Client helpers auto-start the daemon on demand for resume-related operations.

### Client integration

Priority order for choosing the session log at startup:

1. `--continue-session <value>` — id prefix or full path (legacy / explicit).
2. `--resume <path>` — legacy path-only flag.
3. `--attach <id-or-prefix>` — via daemon.
4. `--auto-resume` — most recent session via daemon.
5. TUI startup picker (only if daemon has recent sessions and no explicit resume flag).
6. Brand-new session.

After opening a session, the client sends `touch` so the daemon keeps the ordering current.

### Failure handling

- If the daemon is unreachable and cannot be auto-started, behaviour falls back to today’s default: create a new session.
- Non-interactive mode prints a hint listing recent sessions instead of blocking on a picker.
- `--attach` errors out if the daemon cannot resolve the id; `--auto-resume` silently starts a new session if no recent sessions exist.

## Consequences

**Positive:**
- Sessions are much easier to discover and resume after a terminal closes.
- Minimal architectural change — the daemon is a separate module that does not touch executor internals.
- Atomic conversation-log writes remain unchanged; executor and tools still own persistence.
- Two processes can resume the same session sequentially without protocol changes.

**Negative / limitations:**
- Background mode is Unix-only. Non-Unix builds default `daemon` to foreground and error for background mode.
- The daemon forks a second process via `current_exe()`. If the binary is moved while running, the auto-start may launch the wrong binary.
- Concurrent attach to the *same* session from two terminals is safe at the file level (atomic appends) but interleaves messages in the conversation log. Session locking is out of scope.
- Full detach/reattach of a running TUI is not supported; only metadata resume.

## Implementation

- `src/daemon/mod.rs` — protocol types, `DaemonState`, unit tests.
- `src/daemon/server.rs` — async Unix socket server loop, signal handlers, round-trip test.
- `src/daemon/client.rs` — client helpers, auto-start convenience functions.
- `src/daemon/paths.rs` — socket and PID file paths.
- `src/main.rs` — `daemon` subcommand, `--attach`, `--auto-resume`, startup picker integration.
- `src/tui/components/session_picker.rs` — reusable recent-session picker overlay.
- `src/tui/mod.rs` + `src/tui/keys.rs` — startup overlay and in-session `/resume` picker.
