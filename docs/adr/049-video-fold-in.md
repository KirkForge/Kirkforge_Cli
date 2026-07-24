# ADR-049: Video Fold-In (Non-Default Feature)

## Status

Accepted

## Context

Video editing is the heaviest satellite crate, pulling `serde_yaml`, `strum`, `which`, and `tracing-subscriber` — dependencies new to the root dependency set. Users who don't need video editing shouldn't pay the binary size cost.

## Decision

Fold `kirkforge-video` into the main binary behind a **non-default** `video` feature flag. When enabled, the 8 video tools are direct Rust calls. When disabled (the default), video is not compiled and not registered — the binary is smaller.

The standalone video binary remains for non-KirkForge use cases.

## Consequences

### Positive
- Default binary is unaffected — no size increase.
- Users who want video can opt in with `--features video`.
- No subprocess overhead when video is enabled.

### Negative
- New transitive deps when video is enabled: `serde_yaml`, `strum`, `which`, `tracing-subscriber`.
- Feature flag adds conditional compilation complexity.
- Default build does not include video tools.

## Implementation notes

- Feature flag: `video` (non-default, optional dep on `kirkforge-video`).
- 8 video tools registered under `#[cfg(feature = "video")]`.
- No shell fallback when feature is off — the tools simply aren't available.
- `kirkforge-video` binary remains standalone.
- Binary size impact: default build unchanged; `--features video` adds ~200KB (estimated).