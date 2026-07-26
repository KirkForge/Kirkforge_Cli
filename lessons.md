# Lessons — WO 9.5 session

## What I learned
- `Role` and `ToolInvocation` in `src/shared/mod.rs` do not derive
  `Hash`. Rather than modify the shared types (scope creep + possible
  ripple into serde), I hash the canonical JSON serialisation of each
  `Message` via `serde_json::to_vec`. This is dep-free (`serde_json` is
  already in the tree) and captures every field that affects the
  provider's cache key while respecting `skip_serializing_if` for
  local-only fields like `token_count`.
- `DefaultHasher` is `SipHash-1-3` today but the std docs say the
  algorithm may change. Fine for a per-process, turn-to-turn comparison
  (both turns run in the same process); the tracker's `last_hash` is
  intentionally in-memory only and must not be persisted.

## Scope creep: forced by concurrent-agent contamination
- `src/tui/replay.rs` (untracked, foreign) had 2 clippy
  `doc_overindented_list_items` errors in doc-comment continuation
  lines (lines 13-14). The foreign file is wired into the lib via a
  foreign `pub mod replay;` line in `src/tui/mod.rs`, so the lib build
  (and thus the gate) could not compile without it. I fixed the 2
  doc-indent lines (4-space indent per clippy's suggestion) to unblock
  the gate. This is a 2-line cosmetic fix to foreign WIP; the foreign
  agent would need to make the same fix before their own commit. Noted
  here per AGENTS.md §7.

## What I would do differently
- The WO brief walked back the `src/adapters/anthropic.rs` change
  mid-paragraph ("the real optimization is: … emit a
  `PlanReason::CacheStemReuse` metric event"). I did NOT touch the
  adapter — the `cache_control` markers are idempotent and the API
  needs full content even for cached messages, so an adapter change
  would be cosmetic and would add `tracing` to a hot path (AGENTS.md §5
  forbids debug spam). The useful client-side signal is the metric
  event, which is what I shipped.
