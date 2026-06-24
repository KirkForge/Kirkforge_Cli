# ADR 016: `/save` Conversation Transcript

## Status

Accepted

## Date

2026-06-24

## Context

Users want to share or archive agent conversations. A chat transcript in a human-readable format (GitHub-flavored Markdown) is a standard expectation for coding-agent CLIs.

The TUI already keeps a structured conversation log (`ConversationEntry`) in memory. Exporting it to Markdown is a small, self-contained feature, but it writes user data to disk and must respect the same path-safety rules as the model's file tools.

## Decision

Add a `/save [path]` slash command that writes the current TUI conversation as a Markdown transcript, defaulting to a path next to the session log.

### Output format

The transcript renders as GitHub-flavored Markdown with:

- A title header (`# KirkForge transcript`)
- Session id and timestamp
- Each message as a role-labelled block (User, Assistant, System, Tool)
- Tool entries collapse to a summary line unless expanded in the TUI

### Path resolution

- If the user supplies a path, use it verbatim.
- If no path is supplied, derive the filename from the session log path: replace the `.conv.ndjson` extension with `.md`.
- If no session log is open, fall back to `kirkforge-transcript-YYYY-MM-DD-HHMMSS.md` in the current directory.

### Safety

`/save` calls `PathGuard::check_write` on the resolved path before writing. This means:

- Writes outside `sandbox_dir` / `allowed_write_dirs` are denied.
- Deny-listed paths and extensions are rejected.
- Dotfile blocking is honored.

This prevents a malicious or misguided prompt from using `/save` to exfiltrate conversation contents to an arbitrary location.

### Parent directory creation

If the resolved path has a non-empty parent directory, it is created with `create_dir_all` before writing. Relative filenames like `chat.md` are written into the current directory without creating parents.

## Consequences

**Positive:**
- Users can save and share transcripts with a single command.
- Default path sits next to the session log, making it easy to find.
- Path-guard enforcement keeps `/save` inside the same sandbox as `write_file`.

**Negative / limitations:**
- Transcripts are written synchronously in the TUI thread; very large conversations could briefly block the UI. In practice the Markdown size is bounded by the chat buffer.
- Only the in-memory TUI conversation is saved; any messages not yet flushed to the session log are still included because `state.messages` is the source.
- The transcript does not include system thinking content or internal tool metadata beyond the rendered tool cards.

## Implementation

- `src/tui/transcript.rs` — `format_transcript` and helpers.
- `src/tui/commands/save.rs` — `handle_save_command`, path resolution, and `PathGuard::check_write`.
- `src/tui/keys.rs` — `/save` dispatch.
