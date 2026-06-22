# ADR 009: Enforced Plan Mode — Read-Only Discovery Before Implementation

## Status

Accepted

## Date

2026-06-22

## Context

The previous `/plan` slash command was prompt-only: the system message asked the model to plan before editing, but nothing prevented the model from calling `write_file`, `edit_file`, or destructive `bash` while it was still "thinking". In practice the assistant would frequently start implementing before the user had reviewed the plan, especially on long or ambiguous tasks.

Kimi Code CLI solves this with an enforced planning phase where mutating tools are mechanically unavailable. We want the same guarantee without adding a separate "planner" model or a heavyweight workflow engine.

## Decision

Add an executor-level `plan_mode` flag that blocks mutating tools at the dispatch layer. The flag is entered via `/plan` and exited via `/implement` or explicit user approval.

### Allowed tools in plan mode

| Tool | Allowed? | Notes |
|------|----------|-------|
| `read_file` | yes | discovery |
| `read_image` | yes | discovery |
| `grep` | yes | discovery |
| `glob` | yes | discovery |
| `bash` | sometimes | only if the command passes the `is_read_only_bash` allowlist |
| `write_file` | no | blocked |
| `edit_file` | no | blocked |
| any other tool | no | blocked |

### Read-only bash allowlist

A fixed list of read-only commands (`ls`, `cat`, `head`, `tail`, `pwd`, `echo`, `grep`, `rg`, `diff`, `jq`, etc.) is checked by `is_read_only_bash`. The check also rejects:

- output redirection (`>`)
- pipes (`|`)
- command separators (`;`, `&&`, `||`)
- command substitution (`$()`, backticks)
- sub-shells (`sh`, `bash` as a pipe segment)

This is intentionally conservative. If a command is questionable, plan mode blocks it and the user can exit planning to run it.

### Entering plan mode

- `/plan <task>` flips `Executor.plan_mode = true`, sends the plan prompt, and records a system marker in the conversation log.
- While plan mode is active the executor continues to run turns normally; only tool dispatch is restricted.

### Exiting plan mode

- The model is instructed to end planning with the marker `## Plan Complete — ready to implement`.
- When the executor sees this marker in assistant content it emits `TurnEvent::PlanComplete`.
- The TUI appends a system message telling the user to type `/implement` to exit plan mode.
- `/implement` flips `plan_mode = false` and injects a system message summarizing the transition so the model knows it may now edit files.

### User experience

- A blocked tool call returns a clear message: "📐 Plan mode blocked `<tool>`: only read-only discovery tools are allowed until you type /implement."
- The TUI does not show an approval dialog for blocked tools; the denial is automatic.
- Existing permission rules still apply to the read-only subset (e.g., `read_file` path rules).

## Consequences

**Positive:**
- The model cannot mutate code while planning, regardless of prompt adherence.
- Cheap models that ignore instructions are still safe.
- `/plan` + `/implement` is a lightweight, explicit contract with the user.
- Reuses existing tool dispatch and approval plumbing; no new runtime needed.

**Negative / limitations:**
- The read-only bash allowlist is static and may block legitimate discovery commands. Users can exit plan mode to run them.
- Plan mode does not prevent the model from *asking* the user to run a command in the parent session; it only blocks tools inside the planning turn.
- The marker string is a heuristic. Models that do not emit it will not auto-trigger the `/implement` prompt, but the user can still type `/implement` manually.

## Implementation

- `src/session/executor.rs` — `plan_mode` field, `set_plan_mode`, `exit_plan_mode`, enforcement inside `execute_tool_call`, `is_read_only_bash` helper.
- `src/tui/commands/mod.rs` + keys dispatch — `/plan` and `/implement` handlers.
- `src/tui/mod.rs` — `TurnEvent::PlanComplete` handling and user prompt.
- Tests: `test_plan_mode_blocks_write_file`, `test_plan_mode_blocks_non_read_only_bash`, `test_plan_mode_allows_read_file`, `test_plan_mode_allows_read_only_bash`.
