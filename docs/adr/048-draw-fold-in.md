# ADR-048: Draw Fold-In

## Status

Superseded (draw removed from workspace in 21.11-R0)

## Context

Draw (terminal diagram model) is invoked via shell scripts calling the `kfd` binary. The `draw_render` tool and `post-turn` hook add subprocess overhead.

## Decision

Draw was removed from the workspace (YEETED in 21.11-R0). This ADR described the original fold-in plan; it is superseded by the removal decision.

The standalone `kfd` binary remains for interactive TUI use.

## Consequences

### Positive
- No subprocess overhead for diagram rendering.
- The `post-turn` hook runs in-process, so `.td.json` detection no longer requires a shell subprocess.
- Small binary size increase (`unicode-segmentation` + `unicode-width` are lightweight).

### Negative
- Feature flag adds conditional compilation complexity.

## Implementation notes

- Draw crate (`crates/kf-draw-core`) and `kfd` binary removed in 21.11-R0.