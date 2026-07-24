# ADR-047: Fold Plugin3 into Core

- **Status:** Accepted
- **Date:** 2026-07-24

## Context

Plugin3 (`crates/plugin3-core`) is the token-budget guard. It is invoked via shell scripts in `plugins/kirkforge-plugin3/tools/` with only environment variables, not full event context. This lossy shim means the budget system cannot see real `post-tool-bash` results or `pre-compact` history — it only receives the tool name and output size via env vars, then emits canned JSON responses.

The same fold-in pattern established by ADR-046 (Stratum) applies: link the crate as an optional dependency and register its tools as direct Rust calls behind a feature flag.

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

4. **Hooks remain as shell scripts for now** (upgrade path documented below). The 4 Plugin3 hooks (`post-tool-bash`, `post-tool-write_file`, `user-prompt-submit`, `pre-compact`) continue to be invoked via the existing shell-plugin mechanism. Converting them to in-process handlers with full `&BusEvent` context is a follow-up that requires deeper wiring into the turn execution loop.

5. **The `plugin3-cli` binary remains standalone**. It is not removed or deprecated; users who prefer the CLI can continue using it.

## Consequences

### Positive

- Budget tools are direct Rust calls — no subprocess overhead, no JSON marshalling, no env-var lossiness.
- The budget system gains access to `TokenBudget` state that persists across tool calls within a session (shared `Arc<Mutex<TokenBudget>>`).
- Binary size impact is small: `plugin3-core` adds `blake3` and `chrono`, both already transitive dependencies.
- The feature flag allows building without Plugin3 support (`--no-default-features`), keeping the dependency tree lean for minimal builds.

### Negative

- `plugin3-core` becomes a compile-time dependency of the default build. Before this change it was only linked transitively through `plugin3-cli`.
- The hooks still go through the shell-plugin path with only env vars. The full event-context wiring is deferred.

## Upgrade path

The 4 hooks can be upgraded to in-process handlers in a follow-up change:

- `post-tool-bash` and `post-tool-write_file` can receive the actual tool result content from the executor's turn loop, enabling real slicing before results enter the conversation.
- `user-prompt-submit` can receive the full prompt and recent tool outputs, enabling informed `Intervention::Slice` and `Intervention::Compact` decisions.
- `pre-compact` can receive the conversation history, enabling the budget system to suggest which turns to compact.
- `session-start` can initialize budget state from the session's persisted config.

Each of these requires wiring into `src/session/executor/turn.rs` and/or `src/session/prompt/microcompaction.rs`, which is out of scope for this fold-in MVP.