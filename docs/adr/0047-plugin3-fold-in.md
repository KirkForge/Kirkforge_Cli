# ADR-047: Fold Plugin3 into Core

- **Status:** Accepted
- **Date:** 2026-07-24

## Context

Plugin3 (`crates/plugin3-core`) is the token-budget guard. It is invoked via shell scripts in `plugins/kirkforge-plugin3/tools/` with only environment variables, not full event context. Without the fold-in, the budget system cannot see real `post-tool-bash` results or `pre-compact` history — it only receives the tool name and output size via env vars, then emits canned JSON responses.

The same fold-in pattern established by ADR-046 (Stratum) applies: link the crate as an optional dependency and register its tools as direct Rust calls behind a feature flag. In addition, the 4 hooks are converted to in-process handlers that receive the full event context, eliminating the lossy shim.

## Decision

1. **Feature flag**: Add `budget = ["dep:plugin3-core"]` to the root `Cargo.toml`, included in `default`. The `plugin3-core` crate becomes an optional dependency of the main `kirkforge` binary.

2. **Tool wrappers**: Create `src/session/budget.rs` gated behind `#[cfg(feature = "budget")]`. The module wraps `plugin3_core`'s public API into 7 `Tool` trait implementations matching the existing plugin shell scripts:
   - `budget_status` — show current token budget status
   - `budget_set` — set the token budget ceiling
   - `budget_compact` — compact the budget store (reset used counter)
   - `store_get` — retrieve a stored offload marker by key
   - `config_validate` — validate Plugin3 configuration
   - `report` — print a spending report from usage logs
   - `self_check` — run Plugin3 self-check diagnostics

3. **Registration**: When `#[cfg(feature = "budget")]`, add the 7 tools to the toolset in `src/main/mod.rs` as a `"budget"` source. When the feature is off, the shell-plugin path remains active.

4. **In-process hooks with full event context** (the headline win — the lossy canned-JSON shim is eliminated). The 4 Plugin3 hooks are registered in `src/session/executor/mod.rs` under `#[cfg(feature = "budget")]` as in-process Rust handlers that receive the full `HookContext` instead of env vars:
   - `SessionStartHook` (session-start) — logs budget state at session start.
   - `PostToolBashHook` (post-tool-bash) — receives the real tool result content via `HookContext.tool_result`, estimates tokens (len/4), records to the shared `TokenBudget`, and warns if approaching/over.
   - `PostToolWriteFileHook` (post-tool-write_file) — same as post-tool-bash but for `write_file`.
   - `PreCompactHook` (pre-compact) — receives compact stats via `HookContext.compact_stats`, resets `budget.used` to 0 if the budget was over/approaching.
   - All 4 share a process-global `TokenBudget` via `OnceLock` with the budget tools, so usage recorded by the hooks is visible to the budget tools.
   - The executor's `run_hook_with_result` and `run_compact_hook` pass the full `HookContext` to in-process hooks.

5. **The `plugin3-cli` binary remains standalone**. It is not removed or deprecated; users who prefer the CLI can continue using it.

## Consequences

### Positive

- Budget tools are direct Rust calls — no subprocess overhead, no JSON marshalling, no env-var lossiness.
- The budget system gains access to `TokenBudget` state that persists across tool calls within a session (shared `Arc<Mutex<TokenBudget>>`).
- The 4 hooks are in-process and receive **real event context** (tool result content via `HookContext.tool_result`, compact stats via `HookContext.compact_stats`) instead of the lossy env-var shim. The canned-JSON shim is eliminated.
- Binary size impact is small: `plugin3-core` adds `blake3` and `chrono`, both already transitive dependencies.
- The feature flag allows building without Plugin3 support (`--no-default-features`), keeping the dependency tree lean for minimal builds.

### Negative

- `plugin3-core` becomes a compile-time dependency of the default build. Before this change it was only linked transitively through `plugin3-cli`.
- The hooks observe and report budget usage but do not yet slice/compact tool results before they enter the conversation. The budget check records usage and warns; it does not mutate the turn output.

## Upgrade path

The hooks now receive full event context. The remaining follow-up is to act on it:

- `post-tool-bash` and `post-tool-write_file` could slice oversized outputs before results enter the conversation (currently they record and warn only).
- `pre-compact` could suggest which turns to compact rather than just resetting the used counter.
- `session-start` could initialize budget state from the session's persisted config rather than the hard-coded defaults.

Each of these requires further wiring into `src/session/executor/turn.rs` and/or `src/session/prompt/microcompaction.rs`, which is out of scope for this fold-in.

### Still deferred

- The `budget_ceiling` and `budget_approaching_ratio` config fields remain deferred (the defaults of 200K and 0.8 are used; the budget tools accept a ceiling as a parameter).