# ADR 010: Built-In Subagent Personas — Fork-Isolated `/explore`, `/plan`, `/coder`

## Status

Accepted

## Date

2026-06-22

## Context

Long tasks benefit from focused subagents: an explorer that only reads code, a planner that never touches shell, and a coder that works in an isolated context. Kimi Code CLI provides built-in personas (`coder`, `explore`, `plan`) with isolated contexts and restricted toolsets.

KirkForge already has `ForkManager` for session branching and `Executor` for running turns. Rather than build a separate subagent runtime, we can fork the conversation, swap the toolset, run one or more turns, and merge only the final assistant summary back into the parent session.

## Decision

Implement three slash-command personas using fork isolation + restricted toolsets. Each persona runs in a background task and reports completion asynchronously.

### Personas

| Command | Toolset | Context | Result merged back |
|---------|---------|---------|--------------------|
| `/explore <task>` | read-only tools + read-only `bash` | fork of current conversation | final assistant summary |
| `/plan <task>` | read-only tools only (no shell) | fork of current conversation | final assistant summary + parent enters plan mode |
| `/coder <task>` | full toolset | fork of current conversation | final assistant summary |

### Toolset restriction

Each persona builds a fresh `Executor` with a filtered `Vec<Arc<dyn Tool>>`:

- `/explore` keeps `read_file`, `read_image`, `grep`, `glob`, and `bash`, and sets `Executor.plan_mode = true` so the fork's own executor enforces read-only `bash`.
- `/plan` keeps only `read_file`, `read_image`, `grep`, `glob`.
- `/coder` keeps the full built-in toolset.

MCP tools are not passed into the fork to keep the persona self-contained and avoid side effects in external servers.

### Fork lifecycle

1. `start_persona` creates a session fork via `ForkManager` at the current conversation point. The fork has its own `*.conv.ndjson` file under the sessions directory.
2. It builds a fresh `Executor` for the fork with the filtered toolset and a local approval channel that auto-approves all tool calls inside the fork. Sandboxing comes from the toolset filter and `plan_mode`, not from interactive approval.
3. It spawns a tokio task running `run_persona_task`.
4. The parent TUI stores `persona_in_progress: Option<PersonaHandle>` and shows a spinner while the persona runs.
5. On completion, `PersonaResult` is sent over an unbounded channel and the parent event loop calls `handle_persona_complete`.

### Merging results back

Only the last assistant message from the fork is appended to the parent conversation log. The parent then reloads its in-memory `messages` from disk and resumes normal execution. This prevents intermediate tool errors or half-finished reasoning from polluting the parent session.

For `/plan`, after merging the summary the parent also sends `plan_tx.send(true)` and injects a prompt telling the user to type `/implement` to exit plan mode. This preserves the `/plan` + `/implement` contract from ADR 009.

### Cancellation

Ctrl+C in the TUI first checks for a running persona and cancels it via an `Arc<AtomicBool>` before falling back to the normal turn-cancel path. A cancelled persona does not merge anything back.

### Safety caps

Config field `max_persona_turns` (default 10) guards against runaway subagent loops. The current implementation runs a single self-contained `run_turn` per persona, so the field acts as an on/off guard (`0` disables personas) and reserves headroom for future multi-turn personas.

## Consequences

**Positive:**
- Reuses existing `ForkManager`, `Executor::run_turn`, and plan-mode enforcement.
- No separate subagent runtime or message broker needed.
- Personas are sandboxed by toolset filtering, not by trust in the model.
- Parent conversation stays clean; only the distilled summary is merged.
- `/coder` gives the model an isolated scratch space for risky refactors.

**Negative / limitations:**
- Each persona currently runs only one turn. Future work can loop until the model emits a done marker.
- Persona results are text summaries only; they cannot return structured data or propose exact edits directly. The parent model must parse and act on the summary.
- Fork files are left on disk under the sessions directory; garbage collection is manual for now.
- Running personas concurrently with the parent is not supported; only one persona at a time.

## Implementation

- `src/tui/commands/persona.rs` — `PersonaKind`, `PersonaHandle`, `PersonaResult`, `tools_for_persona`, `run_persona_task`, `start_persona`.
- `src/tui/commands/mod.rs` — module export.
- `src/tui/app.rs` — `persona_in_progress`, `persona_cancel` fields in `AppState`.
- `src/tui/keys.rs` — `/explore`, `/plan`, `/coder` dispatch and Ctrl+C cancellation.
- `src/tui/mod.rs` — `persona_tx`/`persona_rx` channels, `handle_persona_complete`.
- `src/shared/mod.rs` — `max_persona_turns: usize` config field with serde default 10.
- `src/session/executor.rs` — `max_persona_turns` included in test config helper.
