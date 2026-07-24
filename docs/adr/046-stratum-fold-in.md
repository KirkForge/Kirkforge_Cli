# ADR-046: Fold Stratum into Core

## Status

Accepted

## Context

Stratum (context compression) is invoked via shell scripts that exec the standalone `stratum` binary. This adds subprocess overhead and prevents access to full session context.

## Decision

Fold Stratum's 5 tools (`run`, `apply`, `mode`, `rules`, `config_validate`) and 2 hooks (`session-start`, `pre-tool-bash`) into the main binary behind an optional `stratum` feature flag (default: enabled). When the feature is on, tools are direct Rust calls and hooks are in-process Rust handlers. When off, the shell-plugin path remains as fallback.

The standalone `kirkstratum-cli` binary is unaffected.

## Consequences

### Positive
- No subprocess overhead for Stratum operations.
- Stratum tools can access full session context in future.
- The 2 hooks run in-process:
  - `StratumSessionStartHook` (session-start) calls `kirkstratum_core` to emit the active compression ruleset via `tracing::info`.
  - `StratumPreToolBashHook` (pre-tool-bash) validates stratum config via `kirkstratum_core`, fail-open.
- Default build includes Stratum; binary size increase is minimal (`blake3` + `toml` are already transitive deps).

### Negative
- Binary size increases slightly when feature is enabled.
- Feature flag adds conditional compilation complexity.

## Implementation notes

- Feature flag: `stratum` (default, optional dep on `kirkstratum-core`).
- Tools registered in `src/main/mod.rs` under `#[cfg(feature = "stratum")]`.
- Hooks (`StratumSessionStartHook`, `StratumPreToolBashHook`) registered in `src/session/executor/mod.rs` under `#[cfg(feature = "stratum")]`.
- Shell-plugin path (`plugins/stratum/`) remains as fallback when feature is off.
- The `kirkstratum-cli` binary is unaffected.
- Config field `stratum_mode` (off/lite/full/ultra) was proposed in the workorder but deferred — the existing `enabled_plugins` toggle is sufficient for on/off, and mode selection passes through tool arguments.