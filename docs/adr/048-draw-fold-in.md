# ADR-048: Draw Fold-In

## Status

Accepted

## Context

Draw (terminal diagram model) is invoked via shell scripts calling the `kfd` binary. The `draw_render` tool and `post-turn` hook add subprocess overhead.

## Decision

Fold `kirkforge-draw-core` into the main binary behind an optional `draw` feature flag (default: enabled). The `draw_render` tool is a direct Rust call and the `post-turn` hook is an in-process Rust handler (`DrawPostTurnHook`), which scans `./` and `./out/` for `.td.json` files and logs a suggestion if any are found. The hook is registered in `src/session/executor/mod.rs` under `#[cfg(feature = "draw")]`.

The standalone `kfd` binary remains for interactive TUI use.

## Consequences

### Positive
- No subprocess overhead for diagram rendering.
- The `post-turn` hook runs in-process, so `.td.json` detection no longer requires a shell subprocess.
- Small binary size increase (`unicode-segmentation` + `unicode-width` are lightweight).

### Negative
- Feature flag adds conditional compilation complexity.

## Implementation notes

- Feature flag: `draw` (default, optional dep on `kirkforge-draw-core`).
- Only `kirkforge-draw-core` is linked (pure model), not `kirkforge-draw` (TUI binary).
- `draw_render` tool registered under `#[cfg(feature = "draw")]`.
- `DrawPostTurnHook` (post-turn) registered in `src/session/executor/mod.rs` under `#[cfg(feature = "draw")]`.
- The `kfd` binary remains standalone.