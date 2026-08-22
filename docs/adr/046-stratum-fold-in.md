# ADR-046: Fold Stratum into Core

## Status

Accepted

## Context

Stratum (context compression) is invoked via shell scripts that exec the standalone `stratum` binary. This adds subprocess overhead and prevents access to full session context.

## Decision

Fold Stratum's 5 tools (`run`, `apply`, `mode`, `rules`, `config_validate`) and 2 hooks (`session-start`, `pre-tool-bash`) into the main binary behind an optional `stratum` feature flag (default: enabled). When the feature is on, tools are direct Rust calls and hooks are in-process Rust handlers. When off, the shell-plugin path remains as fallback.

The standalone `kirkstratum-cli` binary was removed in ADR-068 / WO 21.11.

## Consequences

### Positive
- No subprocess overhead for Stratum operations.
- Stratum tools can access full session context in future.
- The 2 hooks run in-process:
  - `StratumSessionStartHook` (session-start) emits the active compression ruleset via `tracing::info`.
  - `StratumPreToolBashHook` (pre-tool-bash) validates stratum config, fail-open.
- Default build includes Stratum; binary size increase is minimal (`blake3` + `toml` are already transitive deps).

### Negative
- Binary size increases slightly when feature is enabled.
- Feature flag adds conditional compilation complexity.

## Implementation notes

- Feature flag: `stratum` (default, optional dep on `kf-compress-core`).
- Tools registered in `src/main/mod.rs` under `#[cfg(feature = "stratum")]`.
- Hooks (`StratumSessionStartHook`, `StratumPreToolBashHook`) registered in `src/session/executor/mod.rs` under `#[cfg(feature = "stratum")]`.
- Shell-plugin path (`plugins/stratum/`) remains as fallback when feature is off.
- The standalone `kirkstratum-cli` binary was removed in ADR-068 / WO 21.11.
- Config field `stratum_mode` (off/lite/full/ultra) was proposed in the workorder but deferred — the existing `enabled_plugins` toggle is sufficient for on/off, and mode selection passes through tool arguments.

## Amendment (2026-08-22, WO 41.3) — shell fallback deleted

The Decision and Implementation notes above state "the shell-plugin path
remains as fallback when feature is off" and reference `plugins/stratum/`.
Both were deleted in WO 29.9: there is no shell-plugin tree and no shell
fallback. When the `stratum` feature is off, the Stratum tools and hooks are
simply absent — not shell-backed. This matches ADR-050 (consolidation) and
`docs/TECHNICAL.md` § Plugin system.