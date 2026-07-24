# ADR-048: Draw Fold-In

## Status

Accepted

## Context

Draw (terminal diagram model) is invoked via shell scripts calling the `kfd` binary. The `draw_render` tool and `post-turn` hook add subprocess overhead.

## Decision

Fold `kirkforge-draw-core` into the main binary behind an optional `draw` feature flag (default: enabled). Only the `draw_render` tool is folded; the `post-turn` hook becomes an in-process Rust handler that globs for `.td.json` files.

The standalone `kfd` binary remains for interactive TUI use.

## Consequences

### Positive
- No subprocess overhead for diagram rendering.
- Small binary size increase (`unicode-segmentation` + `unicode-width` are lightweight).

### Negative
- Feature flag adds conditional compilation complexity.

## Implementation notes

- Feature flag: `draw` (default, optional dep on `kirkforge-draw-core`).
- Only `kirkforge-draw-core` is linked (pure model), not `kirkforge-draw` (TUI binary).
- `draw_render` tool registered under `#[cfg(feature = "draw")]`.
- The `kfd` binary remains standalone.