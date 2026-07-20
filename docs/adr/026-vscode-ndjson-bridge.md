# ADR-026: VS Code NDJSON Bridge (Option B)

- **Status:** Accepted
- **Date:** 2026-07-20

## Context

ADR-019 shipped a minimal VS Code extension: a 46-line PTY wrapper that
spawns `kirkforge run` in the integrated terminal. The model renders in the
terminal; VS Code provides no chat panel, inline diffs, TODO panel, or LSP
bridge. This is the single biggest depth gap vs. Claude Code's extension,
which renders tool results as editor decorations, shares the LSP session, and
surfaces TODO lists as checklists.

The CLI already supports a non-interactive mode (`--non-interactive`) and
has a draft NDJSON protocol in `docs/ideas/headless-json.md`. The extension
needs a stable, versioned NDJSON stream contract so it can render events
without parsing ratatui screen output.

## Decision

Replace the PTY-wrapper approach with a subprocess that runs
`kirkforge run --non-interactive --output-format ndjson` and renders the
streamed NDJSON events in VS Code UI surfaces.

### Surfaces

1. **Chat panel** — webview showing user messages, assistant streaming text,
   tool calls, and tool results.
2. **Inline edit diffs** — when a `TurnEvent::ToolResult` for `edit_file` or
   `write_file` arrives, open a `vscode.diff` editor between the pre-edit
   snapshot and the new content (or, for `write_file`, between old and new
   file bytes). The diff is read-only on the left and the new file on the
   right.
3. **TODO panel** — webview checklist populated from `todo_read` tool results
   and kept in sync with `todo_write`.
4. **LSP bridge** — the extension starts a `vscode-languageclient` for the
   workspace language servers; the model's `lsp_query` tool calls are routed
   through the extension so the editor and the model share the same LSP
   session and diagnostics/symbols stay consistent.

### NDJSON Protocol (v1)

Every line is a JSON object with a `type` field. The extension only acts on
types it understands; unknown types are logged but ignored (forward
compatibility).

```json
{"type":"turn_start","id":"turn-1","timestamp":"2026-07-20T00:00:00Z"}
{"type":"message","role":"user","content":"Fix the bug"}
{"type":"token","content":"I'll"}
{"type":"tool_call","name":"read_file","arguments":{"path":"src/main.rs"}}
{"type":"tool_result","name":"read_file","success":true,"output":"..."}
{"type":"edit","path":"src/main.rs","old_string":"...","new_string":"..."}
{"type":"done","finish_reason":"stop","usage":{"prompt_tokens":120,"completion_tokens":80}}
```

### Extension Architecture

- `src/extension.ts` — activation, command registration, lifecycle.
- `src/bridge.ts` — spawn the kirkforge child process, parse NDJSON lines,
  emit typed events to a central bus.
- `src/panels/chatPanel.ts` — webview provider for the chat view.
- `src/panels/todoPanel.ts` — webview provider for the TODO checklist.
- `src/diff.ts` — open `vscode.diff` editors for `edit_file`/`write_file`.
- `src/lspBridge.ts` — language client wrapper and `lsp_query` request router.
- `src/protocol.ts` — TypeScript types mirroring the NDJSON v1 schema.

## Consequences

- The extension becomes a first-class UI instead of a terminal launcher.
- Any change to the NDJSON schema is a breaking change for the extension;
  schema evolution requires a version bump and coordinated release.
- The LSP bridge avoids duplicating LSP state in the CLI but couples the
  extension to the workspace language server setup.
- Inline diffs require the CLI to emit enough pre-edit state (or the
  extension to buffer file snapshots); this is the riskiest part of the
  implementation.

## Ceiling / Open Questions

- The current CLI does not emit the full NDJSON stream in non-interactive
  mode; this ADR assumes a follow-up CLI change to produce stable NDJSON
  events. The extension can ship once the CLI contract is implemented.
- `lsp_query` currently calls a workspace-local `kirkforge-lsp` pool; the
  bridge needs to either proxy those calls or replace that pool when the
  extension is active.
- Synchronising TODO state across extension webview, CLI session, and
  `todo_read`/`todo_write` tool calls needs a single source of truth (the CLI
  session holds it today).

## Upgrade Path

- ADR-019 (Option A PTY wrapper) remains valid for users who prefer the
  terminal experience. The new extension is Option B and can coexist; the
  command palette exposes both `kirkforge.startTerminal` and
  `kirkforge.startPanel`.
- If the NDJSON contract later needs fields not in v1, bump to v2 and
  gate extension features on the version advertised in the `turn_start`
  event.
