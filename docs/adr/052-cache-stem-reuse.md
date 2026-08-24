# ADR-052: Client-side prompt cache stem reuse

<!-- adr-predicates
status: accepted
implemented: true
supersedes: []
affects-crates: []
-->

- **Status:** Accepted
- **Date:** 2026-07-26

## Context

The Anthropic adapter marks the last two prefix messages and the last
system block with `cache_control: {type: "ephemeral"}` so the provider
can hit its KV-cache for the stable prefix (`src/adapters/anthropic.rs`,
ADR-0027). The OpenAI-compatible path does the same via
`obj["cache_control"] = {"type": "ephemeral"}` in
`src/adapters/mod.rs`. `PromptBuilder` already memoises the system
`Message` it produces so the bytes going to the provider are
byte-for-byte identical across turns (ADR-0027, `cached_system` field).

What is missing is **client-side stem-reuse detection**: nothing
records the hash of the stable prefix across turns, so the system
cannot emit a metric event when the stem is reused. Operators can see
*that* the adapter placed cache markers, but not *whether* the prefix
was actually stable turn-over-turn. This is the observability gap WO
9.5 closes.

## Decision

Add a `CacheStemTracker` in a new `src/session/prompt/cache_stem.rs`
module. The tracker:

- Holds a single `Option<u64>` — the hash of the prefix messages
  recorded in the prior turn.
- `record_prefix_hash(&mut self, messages: &[Message], prefix_len: usize)
  -> u64` — hashes the first `prefix_len` messages and stores the
  hash. `prefix_len` lets the caller exclude the volatile trailing
  user turn.
- `is_stable(&self, messages: &[Message], prefix_len: usize) -> bool` —
  returns `true` when the current prefix hashes to the same value as
  the previously recorded hash. The first call (no prior hash) returns
  `false`.
- `last_hash(&self) -> Option<u64>` — the most recently recorded hash.

The hash uses `std::collections::hash_map::DefaultHasher` — no new
dependency. Each message is serialised to its canonical JSON form (the
same form the on-disk NDJSON log uses) and the bytes are fed to the
hasher. This means any semantic change to a message's role, content,
thinking, tool calls, tool name, or tool call id invalidates the stem,
while fields that do not affect the provider's cache key (e.g.
`token_count`, a local estimate) are excluded by `skip_serializing_if`.

Add a `PlanDecisionKind::CacheStemReuse` variant to
`src/shared/metrics.rs`. When the executor detects a stable stem (a
follow-up WO will wire `CacheStemTracker` into `Executor::turn`), it
emits a `MetricEvent::PlanReason` with this decision kind so the
operator can see stem reuse in the NDJSON log or OTel backend. The
existing `to_otel_attrs` path handles the new variant automatically
because it stringifies the `decision_kind` with `format!("{:?}", ...)`.

### Why not modify the adapter?

The WO brief floated a change to `src/adapters/anthropic.rs` to "log
stem-reuse status and skip redundant cache_control markers on
already-stable messages." This was walked back in the same brief: the
Anthropic API requires the full content even for cached messages (the
server computes the cache key from the bytes), so there is nothing to
short-circuit on the wire. The `cache_control` markers are idempotent —
re-marking a stable message is harmless. An adapter change would be
cosmetic and would add `tracing` calls to a hot path (AGENTS.md §5
forbids debug spam in committed code). The useful client-side signal is
the metric event, not an adapter log line. **This WO does not touch
`src/adapters/anthropic.rs`.**

### Why not wire the tracker into the executor now?

The tracker is self-contained and unit-tested. Wiring it into
`Executor::turn` requires deciding the `prefix_len` policy (system-only
vs. system + first N turns), the emit point (before or after
`build_messages`), and the interaction with microcompaction (which
rewrites the prefix). That is a follow-up WO. Shipping the tracker + the
metric variant + the test + this ADR is what the gate asks for; the
wiring is a separate, reviewable change.

### What shipped in WO 9.5 vs WO 10.2

WO 9.5 shipped the **measurement**: the `CacheStemTracker` struct, the
`PlanDecisionKind::CacheStemReuse` metric variant, 6 unit tests, and
this ADR. The tracker was not called from anywhere outside its own test
module.

WO 10.2 shipped the **wiring**: a `cache_stem: CacheStemTracker` field
on `Executor`, and a call site in `stream_iteration` (`turn.rs`) that
runs after `build_messages` returns. The `prefix_len` policy is
`prefix_len = 1` (system message only) — the first cut documented in
the Future Work section below — because the conversation history grows
every turn and cannot be part of the stable stem. When `is_stable`
returns `true`, the executor emits a `PlanReason::CacheStemReuse`
metric event; `record_prefix_hash` then advances the recorded hash for
the next turn. The integration test `cache_stem_reuse_emitted_on_stable_turn`
proves the event fires on turns 2-5 of a 5-turn conversation, not on
turn 1, and that a `set_system_override` change breaks stability on
turn 6.

The adapter short-circuit (content-omission for cached messages) was
**not** implemented. The Anthropic API requires the full content of
every message on every request — the server computes the cache key
from the bytes sent ("Cache hits require 100% identical prompt
segments", Anthropic prompt-caching docs) — so there is nothing to
omit on the wire. The `cache_control` markers in `anthropic.rs` are
unchanged; the server-side KV-cache still hits. The useful client-side
signal is the metric event, not a wire-bytes saving.

## Consequences

Positive:

- Operators get a `PlanReason::CacheStemReuse` metric event when the
  stem is stable, so cache reuse is observable from the NDJSON log or
  OTel backend without parsing adapter request bodies.
- No new dependencies (`DefaultHasher` + `serde_json` are already in
  the tree).
- The tracker is pure, sync, and `Send + Sync`; it fits behind the
  executor's existing `&mut self.prompt_builder` slot with no extra
  locking.
- The hash is a single `u64` comparison; the serialisation cost is one
  pass over the prefix messages per turn, dominated by the system
  message (which is small).

Negative:

- `DefaultHasher` is not guaranteed to be stable across Rust versions
  (it is `SipHash-1-3` today but the std docs say "it should not be
  used where DoS resistance is needed" and the algorithm may change).
  This is fine for a per-process, turn-to-turn comparison (both
  turns run in the same process), but the hash must not be persisted
  or compared across versions. The tracker's `last_hash` is
  intentionally in-memory only.

## Tests

- `session::prompt::cache_stem::tests::test_prefix_hash_stable_across_five_turns`
  — builds a 5-turn conversation, confirms the prefix hash is stable
  across turns 2-5 with a fixed system-only prefix, and that changing
  the system message breaks stability.
- `test_fresh_tracker_is_not_stable` — a fresh tracker reports
  `is_stable=false` and `last_hash=None`.
- `test_empty_prefix_is_trivially_stable` — `prefix_len=0` hashes
  nothing and is stable across any message list.
- `test_prefix_len_clamped_to_message_count` — `prefix_len` larger
  than the message list is clamped (no panic).
- `test_hash_is_deterministic_across_instances` — the same prefix
  produces the same hash across separate tracker instances.
- `test_semantic_change_invalidates_hash` — a change in any
  contributing field (role, content, thinking, tool_calls,
  tool_name, tool_call_id) invalidates the hash.

## Future work

- Extend the `prefix_len` policy from "system message only" (prefix_len
  = 1, shipped in WO 10.2) to "system + first N turns" once
  microcompaction's prefix-rewrite interaction is characterised. The
  conversation history grows every turn, so a fixed `prefix_len` past
  the system message would need to account for microcompaction
  rewriting older turns (which changes the prefix bytes and breaks the
  hash even when the semantic content is stable).
- Surface the stem-reuse status in the TUI status bar alongside the
  existing `CacheStats` event (ADR-0027).
- Consider a `content_parts`-aware hash if the adapter ever caches
  multimodal stems (currently the adapter collapses parts to text for
  the cache stem, so the JSON-form hash is correct).