# ADR 015: Per-Session Undo Stack for File Edits

## Status

Accepted

## Date

2026-06-24

## Context

`edit_file` and `write_file` are the most destructive tools in the agent's toolkit. Before this feature, the only recovery path for a bad edit was the user's git working tree — useless for untracked files or for users who do not immediately notice the mistake. Other agent CLIs (Aider, Claude Code, Cursor) provide an in-app undo, and users expect it.

## Decision

Snapshot the pre-edit bytes of every file touched by `edit_file` or `write_file` before the write happens, keep up to 50 snapshots on disk for the session, and allow the user to pop the most recent snapshot via `/undo`.

### Snapshot storage

Snapshots live in `~/.local/share/kirkforge/undo/<session_id>/<seq>.snap`. Each snapshot file contains the raw bytes of the file as it existed before the operation. The `UndoStack` stores only metadata (`UndoOp`): sequence number, path, whether the file existed, kind (`Edit` vs `Write`), snapshot size, and timestamp.

### Atomicity

- Snapshots are written via `tempfile::NamedTempFile` + atomic rename so a crash mid-write cannot leave a corrupt snapshot.
- Restoration writes to `<path>.tmp`, fsyncs, and atomically renames over the target.
- If the file did not exist before the edit, restoration removes it.

### Lifecycle

- The stack is created when the session starts and is shared between the executor and the TUI via `Arc<Mutex<UndoStack>>`.
- `push` is called from the `edit_file` and `write_file` tools before the destructive write.
- `pop` is called from `/undo` or `/undo count` in the TUI.
- On FIFO overflow past 50 entries, the oldest snapshots are deleted from disk and dropped from the in-memory deque.

### Failure modes

- If `for_session` cannot determine a snapshot directory at session start, edits still work but cannot be undone. This is logged; refusing all edits would be worse.
- If writing the snapshot fails (disk full, permission denied), the tool aborts before modifying the target file.
- If restoration fails, the snapshot remains on disk for manual recovery.

## Consequences

**Positive:**
- Users can recover from bad edits without leaving the CLI.
- Snapshots are on disk, so they survive temporary process issues within the session.
- The cap prevents runaway agents from filling the disk.

**Negative / limitations:**
- Snapshots are session-scoped only; closing the CLI loses the undo stack. Future `--continue <session_id>` could reload it.
- Only the most recent 50 edits are recoverable.
- The stack does not snapshot directory-level changes (e.g. `bash` creating files); only `edit_file` and `write_file` are covered.

## Implementation

- `src/session/undo.rs` — `UndoStack`, `UndoOp`, `UndoKind`, `UndoSummary`.
- `src/tools/edit_file.rs` and `src/tools/write_file.rs` — snapshot before write, push to stack.
- `src/tui/commands/undo.rs` — `/undo`, `/undo list`, `/undo count` handlers.
- `src/session/executor.rs` — builds the shared `UndoStack` at session start and passes it to the file tools.
