# ADR-048: Draw Fold-In

## Status

Accepted

## Context

Draw (terminal diagram model) is invoked via shell scripts calling the `kfd` binary. The `draw_render` tool and `post-turn` hook add subprocess overhead.

## Decision

Fold `kirkforge-draw-core` into the main binary behind an optional `draw` feature flag (default: enabled). Only the `draw_render` tool is folded as a direct Rust call. The `post-turn` hook remains a shell script for now (deferred — converting it to an in-process handler requires wiring into the TUI event loop and is out of scope for this fold-in).

The standalone `kfd` binary remains for interactive TUI use.

## Consequences

### Positive
- No subprocess overhead for diagram rendering.
- Small binary size increase (`unicode-segmentation` + `unicode-width` are lightweight).

### Negative
- Feature flag adds conditional compilation complexity.
- The `post-turn` hook is still a shell script, so the `.td.json` detection workflow remains subprocess-based.

## Implementation notes

- Feature flag: `draw` (default, optional dep on `kirkforge-draw-core`).
- Only `kirkforge-draw-core` is linked (pure model), not `kirkforge-draw` (TUI binary).
- `draw_render` tool registered under `#[cfg(feature = "draw")]`.
- The `kfd` binary remains standalone.
- Hook conversion (post-turn → in-process) is a follow-up.

## Upgrade path

The `post-turn` hook can be converted to an in-process handler in a follow-up change that globs for new `.td.json` files after each turn and emits a suggestion. This requires wiring into the TUI event loop.