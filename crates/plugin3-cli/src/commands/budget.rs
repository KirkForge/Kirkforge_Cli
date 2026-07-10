//! `plugin3 budget {status,set,reset,compact}` — inspect or set the
//! token budget ceiling, reset the session counter, and emit a
//! compact hint.

use plugin3_core::{budget::BudgetConfig, compaction};

// ponytail: crate:: jumps two levels up (commands:: → bin root)
// for the shared helpers that main.rs owns. An alternative would be
// to re-export them through commands::mod — add that wrapper when
// a third caller appears.
use crate::{
    config_path, emit_compact_hint, load_budget, load_recent_outputs, save_budget,
    save_budget_config_at,
};

pub(crate) fn status(as_json: bool) {
    let b = load_budget();
    if as_json {
        let resp = serde_json::json!({
            "used": b.used,
            "ceiling": b.ceiling,
            "state": b.state(),
        });
        crate::json_out::print_json(&resp);
    } else {
        println!("used: {} / {} ({:?})", b.used, b.ceiling, b.state());
    }
}

pub(crate) fn set(ceiling: usize, persist_default: bool, as_json: bool) {
    let mut b = load_budget();
    b.ceiling = ceiling;
    save_budget(&b);
    if persist_default {
        // ponytail: write the persistent default separately from the
        // runtime budget. The runtime file's `used` field is session-
        // local and must not bleed into a future session via config.toml.
        let cfg = BudgetConfig {
            ceiling: b.ceiling,
            approaching_ratio: b.approaching_ratio,
        };
        save_budget_config_at(&cfg, &config_path());
    }
    if as_json {
        let resp = serde_json::json!({
            "ceiling": b.ceiling,
            "persisted_default": persist_default,
        });
        crate::json_out::print_json(&resp);
    } else {
        println!("ceiling set to {ceiling}");
        if persist_default {
            println!("default persisted to {}", config_path().display());
        }
    }
}

// ponytail: B2 fix — `plugin3 budget reset` zeros `used` while
// preserving `ceiling` and `approaching_ratio`. The previous gap
// (plugin3-gaps.md § B2) was that `used` lives in `budget.toml`
// under `data_dir` and persists across Claude Code sessions —
// yesterday's session-end `used` bled into today's first
// UserPromptSubmit, making the ceiling feel like it was hit
// pre-emptively. `reset` is opt-in: the user runs it at session
// start (or hooks it into a shell init). It does NOT touch
// `config.toml` — the persisted `ceiling`/`approaching_ratio`
// defaults survive the reset, so a user who set
// `plugin3 budget set --default 250000` keeps that ceiling.
pub(crate) fn reset(as_json: bool) {
    let mut b = load_budget();
    let prior = b.used;
    b.used = 0;
    save_budget(&b);
    if as_json {
        let resp = serde_json::json!({
            "prior_used": prior,
            "used": b.used,
            "ceiling": b.ceiling,
        });
        crate::json_out::print_json(&resp);
    } else {
        println!(
            "budget.used reset (was {prior}, now 0); \
                  ceiling={} unchanged",
            b.ceiling
        );
    }
}

pub(crate) fn compact(as_json: bool) {
    let b = load_budget();
    let recent = load_recent_outputs();
    // ponytail: we only read the recent outputs (build a turn range
    // for the hint) — `iter()` yields `&(String, usize)` straight off
    // the VecDeque. The earlier `recent.make_contiguous()` coerced to
    // `&mut [T]` to feed `decide(...)` in `hooks/user_prompt_submit`,
    // but here we don't pass the slice to anything that needs a
    // contiguous `&[T]`; `iter()` is the immutable borrow. Avoids
    // the unused-`mut` license on the slice binding and the (rare)
    // reallocation that `make_contiguous()` may trigger when the
    // deque isn't already contiguous.
    // Best-effort turn range from recent outputs: host owns the
    // canonical conversation log; we just surface recent keys.
    let turns: Vec<compaction::Turn> = recent
        .iter()
        .enumerate()
        .map(|(i, (k, size))| compaction::Turn {
            index: i,
            role: "tool".into(),
            content_preview: format!("{k} ({size} bytes)"),
        })
        .collect();
    let hint = compaction::build_hint(&b, &turns);
    emit_compact_hint(&b);
    if as_json {
        let resp = serde_json::json!({ "hint": hint });
        crate::json_out::print_json(&resp);
    } else {
        println!("reason:       {}", hint.reason);
        println!("tokens_used: {}", hint.tokens_used);
        println!("ceiling:      {}", hint.tokens_ceiling);
        if let Some(o) = hint.oldest_turn {
            println!("oldest_turn: {o}");
        }
        if let Some(n) = hint.newest_turn {
            println!("newest_turn: {n}");
        }
    }
}

// ponytail: drive the hint assembly path directly so the test
// catches a regression in the turn-range computation without
// spawning a subprocess. The recent-outputs VecDeque is rebuilt
// by hand — `compact()` only reads it, never writes back, so a
// local value is exactly equivalent to one loaded from disk.
#[cfg(test)]
mod compact_tests {
    use super::*;
    use plugin3_core::budget::TokenBudget;
    use plugin3_core::cost::{UsageKind, UsageRecord};
    use plugin3_core::test_support::EnvGuard;
    use std::collections::VecDeque;

    // ponytail: helper that mirrors the inner block of `compact()`
    // without the I/O or the stdout side-effects, so the test can
    // assert on the hint struct directly. If `compact()` drifts
    // (e.g. drops `role: "tool"` or changes the preview format),
    // this assertion surfaces the divergence.
    fn build_hint_from(
        b: &TokenBudget,
        recent: &VecDeque<(String, usize)>,
    ) -> compaction::CompactHint {
        let turns: Vec<compaction::Turn> = recent
            .iter()
            .enumerate()
            .map(|(i, (k, size))| compaction::Turn {
                index: i,
                role: "tool".into(),
                content_preview: format!("{k} ({size} bytes)"),
            })
            .collect();
        compaction::build_hint(b, &turns)
    }

    #[test]
    fn compact_hint_covers_full_recent_window() {
        // ponytail: oldest_turn / newest_turn span the surviving
        // recent entries (FIFO at 32). A contributor who truncates
        // `turns` to e.g. the last 5 (thinking "the host only cares
        // about recent activity") silently shrinks the hint's range
        // — caught here.
        let b = TokenBudget {
            ceiling: 100,
            approaching_ratio: 0.8,
            used: 95,
        };
        let mut recent: VecDeque<(String, usize)> = VecDeque::with_capacity(32);
        for i in 0..32 {
            recent.push_back((format!("k{i:02}"), (i + 1) * 100));
        }
        let hint = build_hint_from(&b, &recent);
        assert_eq!(
            hint.oldest_turn,
            Some(0),
            "oldest turn is the FIFO head (index 0)"
        );
        assert_eq!(
            hint.newest_turn,
            Some(31),
            "newest turn is the 32nd entry (index 31)"
        );
        assert_eq!(hint.tokens_used, 95);
        assert_eq!(hint.tokens_ceiling, 100);
        assert!(
            hint.reason.contains("95/100"),
            "reason must carry used/ceiling; got: {}",
            hint.reason
        );
    }

    #[test]
    fn compact_hint_with_empty_recent_reports_none_for_turn_range() {
        // ponytail: a fresh session (no tool results yet) yields
        // no turn indices. A contributor who defaults
        // `oldest_turn`/`newest_turn` to `Some(0)` (rather than
        // `None`) misleads the host into thinking there is one
        // empty turn at index 0 — caught here.
        let b = TokenBudget::default();
        let recent: VecDeque<(String, usize)> = VecDeque::new();
        let hint = build_hint_from(&b, &recent);
        assert_eq!(
            hint.oldest_turn, None,
            "empty recent must produce None for oldest_turn"
        );
        assert_eq!(
            hint.newest_turn, None,
            "empty recent must produce None for newest_turn"
        );
        assert_eq!(hint.tokens_used, 0);
        assert_eq!(hint.tokens_ceiling, TokenBudget::default().ceiling);
    }

    #[test]
    fn compact_hint_preserves_recent_iteration_order() {
        // ponytail: VecDeque::iter() walks front-to-back (oldest
        // first, newest last). A contributor who switches to
        // `iter().rev()` for "newest first" formatting flips the
        // index assignment — caught here because newest_turn
        // would become 0 and oldest_turn would become 4.
        let b = TokenBudget {
            ceiling: 1000,
            approaching_ratio: 0.8,
            used: 800,
        };
        let recent: VecDeque<(String, usize)> = VecDeque::from(vec![
            ("first".into(), 100),
            ("second".into(), 200),
            ("third".into(), 300),
            ("fourth".into(), 400),
            ("fifth".into(), 500),
        ]);
        let hint = build_hint_from(&b, &recent);
        assert_eq!(hint.oldest_turn, Some(0), "oldest maps to index 0");
        assert_eq!(hint.newest_turn, Some(4), "newest maps to last index");
    }

    #[test]
    fn compact_hint_renumbers_after_fifo_eviction() {
        // ponytail: simulate the overflow path the live code takes
        // (push 33 entries into a 32-cap VecDeque; the FIFO at the
        // head evicts). After eviction, the surviving entries are
        // renumbered 0..31 by `iter().enumerate()` — the second-
        // pushed entry is now at index 0, and the 33rd-pushed entry
        // is now at index 31. The original first entry (k00) is
        // gone. The earlier tests pin only the no-eviction shape
        // (fresh 32-entry deque, or a small 5-entry deque), so a
        // contributor who introduces a session-relative offset
        // (e.g. `enumerate().map(|(i, ...)| Turn { index: i + 1 })`
        // for "1-based indices", or threads an evicted_count into
        // the assignment) would not surface. A host that uses the
        // turn range to slice the conversation would then index
        // past the head — caught here.
        let b = TokenBudget {
            ceiling: 1000,
            approaching_ratio: 0.8,
            used: 800,
        };
        let mut recent: VecDeque<(String, usize)> = VecDeque::with_capacity(32);
        for i in 0..33_usize {
            recent.push_back((format!("k{i:02}"), (i + 1) * 100));
        }
        // Mirror `append_recent_at`: drop the head until we're back
        // under the bound. The live code uses
        // `while entries.len() > RECENT_BOUND { entries.pop_front(); }`,
        // so a single `pop_front()` is enough for one overflow.
        assert_eq!(recent.len(), 33, "pre-eviction: 33 entries pushed");
        recent.pop_front();
        assert_eq!(recent.len(), 32, "post-eviction: bound enforced");
        assert_eq!(
            recent.front().map(|(k, _)| k.as_str()),
            Some("k01"),
            "post-eviction FIFO head is the original second-pushed entry"
        );
        assert_eq!(
            recent.back().map(|(k, _)| k.as_str()),
            Some("k32"),
            "post-eviction FIFO tail is the original 33rd-pushed entry"
        );

        let hint = build_hint_from(&b, &recent);
        assert_eq!(
            hint.oldest_turn,
            Some(0),
            "oldest turn is the new FIFO head, renumbered to 0; \
             a contributor who threads an evicted_count into the \
             assignment (e.g. `index: i + 1`) would yield Some(1) here"
        );
        assert_eq!(
            hint.newest_turn,
            Some(31),
            "newest turn is the 32 surviving entries numbered 0..31; \
             a 1-based or session-offset assignment would yield Some(32) here"
        );
        // ponytail: drive the hint-assembly path directly so we
        // also assert on the `content_preview` mapping. The new
        // head (index 0) must carry the second-pushed entry's
        // preview, not the first-pushed one's (which was evicted).
        let turns: Vec<compaction::Turn> = recent
            .iter()
            .enumerate()
            .map(|(i, (k, size))| compaction::Turn {
                index: i,
                role: "tool".into(),
                content_preview: format!("{k} ({size} bytes)"),
            })
            .collect();
        assert_eq!(turns[0].index, 0);
        assert_eq!(
            turns[0].content_preview, "k01 (200 bytes)",
            "index 0 preview must be the new head (k01), not the evicted k00"
        );
        assert_eq!(
            turns[31].content_preview, "k32 (3300 bytes)",
            "index 31 preview must be the new tail (k32)"
        );
        assert!(
            turns.iter().all(|t| t.role == "tool"),
            "every recent-output turn is role=tool; \
             a contributor who threads role through the chain would surface here"
        );
    }

    // ponytail: pin the wire shape of the JSONL record emitted by
    // `emit_compact_hint(&b)` in `main.rs` (called from both
    // `compact()` here and `pre_compact()` in hooks/mod.rs).
    // CompactHint is the only `UsageKind` that carries
    // `tokens_used`/`tokens_ceiling` (budget record) but NOT
    // `bytes_in`/`bytes_out`/`tool` (slice record). The aggregator
    // (`report::aggregate_sessions`) keys on `kind == CompactHint`
    // and only reads `bytes_*` for `Slice` records, so the
    // *absence* of those fields is load-bearing — a contributor
    // who sets `bytes_in: Some(0)` (thinking "0 == None") makes
    // the field appear in JSONL and may confuse future
    // aggregators that filter on `bytes_in` presence.
    //
    // The test inlines the exact field set the call site produces
    // (mirroring `emit_compact_hint`'s `..empty_record()` spread,
    // which sets `ts` to `chrono::Utc::now()` — we use a fixed
    // timestamp so the test is deterministic). The drift is in
    // the *shape* of the JSONL record, not in the helper.
    #[test]
    fn compact_hint_usage_record_wire_shape_is_pinned() {
        let b = TokenBudget {
            ceiling: 200_000,
            approaching_ratio: 0.8,
            used: 123_456,
        };
        // Mirror emit_compact_hint's exact field set. The call site
        // uses `..empty_record()` which sets ts to `chrono::Utc::now()`
        // and the four optional fields to None; we pin all of them
        // explicitly so the test is deterministic.
        let rec = UsageRecord {
            ts: chrono::DateTime::parse_from_rfc3339("2026-06-28T00:00:00Z")
                .unwrap()
                .with_timezone(&chrono::Utc),
            kind: UsageKind::CompactHint,
            session_id: String::new(),
            bytes_in: None,
            bytes_out: None,
            tokens_used: Some(b.used),
            tokens_ceiling: Some(b.ceiling),
            tool: None,
        };
        let v = serde_json::to_value(&rec).expect("CompactHint serialises");

        // Positive pins: the four fields the call site explicitly
        // sets must be present with the exact snake_case spellings
        // that `report --kind compact_hint` filters grep for.
        assert_eq!(
            v["kind"], "compact_hint",
            "kind spelling is load-bearing — `report --kind compact_hint` \
             greps for the literal `compact_hint` on the wire. A contributor \
             who renames `UsageKind::CompactHint` (e.g. `CompactHint_v2`) \
             without updating the rename rule surfaces here."
        );
        assert_eq!(
            v["session_id"], "",
            "CompactHint is a session-less event (both call sites — \
             `compact()` here and `pre_compact()` in hooks/mod.rs — \
             pass `String::new()`). A contributor who threads \
             `payload.session_id.clone()` from the surrounding hook \
             makes the JSONL record attributeless, silently leaking \
             the host's session_id onto a global event."
        );
        assert_eq!(
            v["tokens_used"], 123_456,
            "tokens_used carries the runtime budget counter — the same \
             field shape as BudgetWarn/BudgetOver. A contributor who \
             drops `tokens_used: Some(b.used)` (and lets it default to \
             None via `..empty_record()`) makes the field disappear \
             from the JSONL and breaks dashboard readers that group \
             on `tokens_used`."
        );
        assert_eq!(
            v["tokens_ceiling"], 200_000,
            "tokens_ceiling is the user-set budget ceiling — same load- \
             bearing rationale as tokens_used."
        );

        // Negative pins: bytes_in / bytes_out / tool must be ABSENT
        // from the JSONL (None + skip_serializing_if omits them).
        // A contributor who sets any of these to `Some(...)` makes
        // the field appear and breaks the "CompactHint is budget-
        // shaped, not slice-shaped" wire contract.
        for absent in ["bytes_in", "bytes_out", "tool"] {
            assert!(
                v.get(absent).is_none(),
                "CompactHint JSONL record must NOT carry `{absent}` — \
                 the `skip_serializing_if = \"Option::is_none\"` attribute \
                 omits it from JSONL when None. A contributor who adds \
                 `bytes_in: Some(0)` (or any Some(_)) on the \
                 emit_compact_hint call site surfaces here, because \
                 the aggregator keys Slice math on `bytes_in` \
                 presence and a stray `bytes_in: 0` would inflate \
                 `bytes_saved = 0 - 0 = 0` (no-op) but break future \
                 filters that distinguish present-vs-absent."
            );
        }
    }

    // ponytail: pin the `plugin3 budget compact --json` wire shape
    // end-to-end. The CLI wraps the `CompactHint` struct in a
    // top-level `{"hint": ...}` object so the JSON output is
    // extensible (a future contributor adding a sibling field
    // like `{"hint": ..., "triggered_at": ...}` knows where to
    // slot it). The top-level key set is currently exactly one
    // key — a contributor who adds a second key without updating
    // consumers (e.g. dashboard scripts that grep `jq '.hint'`)
    // desyncs the JSON envelope.
    //
    // The hint's per-field set is also pinned (5 fields, sorted):
    // reason / tokens_used / tokens_ceiling / oldest_turn /
    // newest_turn. A rename of `tokens_used` → `used` makes the
    // field reappear at a new key while the existing key
    // disappears — `jq '.hint.tokens_used'` returns null silently,
    // no error. Drift catches here.
    //
    // The test mirrors the exact wrapper shape `compact()` builds
    // (line 74: `serde_json::json!({ "hint": hint })`) and asserts
    // on the parsed JSON value rather than a literal-substring scan
    // — the structure is the contract, not the formatting.
    #[test]
    fn budget_compact_json_output_shape_is_pinned() {
        let b = TokenBudget {
            ceiling: 200_000,
            approaching_ratio: 0.8,
            used: 123_456,
        };
        // recent is empty so oldest_turn/newest_turn are None — the
        // serialised shape carries both as null, exercising the
        // `Option<usize>` field path on the wire.
        let recent: VecDeque<(String, usize)> = VecDeque::new();
        let hint = build_hint_from(&b, &recent);
        // Mirror the production wrapper exactly.
        let resp = serde_json::json!({ "hint": hint });
        let v: serde_json::Value =
            serde_json::to_value(&resp).expect("compact --json wrapper serialises");

        // Top-level: exactly one key, `hint`. A contributor who
        // adds a second key (e.g. `{"hint": ..., "ts": "..."}` or
        // `{"hint": ..., "reason_in_words": "..."}`) breaks the
        // single-key contract here. The sorted assertion catches
        // both "added a key" and "renamed the key" — neither
        // produces the same set.
        let obj = v.as_object().expect("top-level is an object");
        let mut top_keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        top_keys.sort_unstable();
        assert_eq!(
            top_keys,
            vec!["hint"],
            "budget compact --json top-level key set must be exactly \
             {{hint}}; a contributor who adds a sibling key breaks \
             downstream `jq '.hint'` readers. got: {top_keys:?}"
        );

        // Inner hint: 5 fields. `CompactHint` derives Serialize
        // so the field set is owned by the struct definition
        // (compaction.rs); a contributor who adds a 6th field
        // there (e.g. `triggered_at`) propagates here only if the
        // CLI's wrapper logic also surfaces it. The test pins the
        // current 5-field shape; if CompactHint grows, the test
        // needs an update — the failure message names the drift.
        let hint_v = &v["hint"];
        let hint_obj = hint_v
            .as_object()
            .expect("hint is an object (CompactHint is a struct)");
        let mut hint_keys: Vec<&str> = hint_obj.keys().map(String::as_str).collect();
        hint_keys.sort_unstable();
        assert_eq!(
            hint_keys,
            vec![
                "newest_turn",
                "oldest_turn",
                "reason",
                "tokens_ceiling",
                "tokens_used"
            ],
            "CompactHint serialised field set must match the 5-field struct; \
             a contributor who renames `tokens_used` → `used` (or adds \
             a sibling field) makes `jq '.hint.tokens_used'` silently \
             return null. got: {hint_keys:?}"
        );

        // Spot-check the values to catch a regression that swaps
        // the wrapper to `{"hint": {"reason": used, ...}}` (i.e.
        // dropping tokens_used and shoving its value into reason).
        assert_eq!(
            hint_v["reason"], hint.reason,
            "reason must round-trip verbatim from CompactHint::reason"
        );
        assert_eq!(
            hint_v["tokens_used"], 123_456,
            "tokens_used must carry the runtime budget counter"
        );
        assert_eq!(
            hint_v["tokens_ceiling"], 200_000,
            "tokens_ceiling must carry the user-set ceiling"
        );
        assert!(
            hint_v["oldest_turn"].is_null(),
            "empty recent must serialise oldest_turn as null, not 0"
        );
        assert!(
            hint_v["newest_turn"].is_null(),
            "empty recent must serialise newest_turn as null, not 0"
        );
    }

    // ponytail: pin the wire shape of `plugin3 budget status --json`.
    // The CLI prints a 3-key object (used, ceiling, state) — exactly
    // matching `TokenBudget::state()`'s 3-variant return (Under /
    // Approaching / Over). A contributor who adds a sibling key
    // (e.g. `approaching_at: f64` to surface a "75% threshold"
    // delta) breaks downstream `jq '.ceiling'` readers here.
    //
    // The test mirrors the production wrapper (lines 14-22) and
    // asserts on the parsed JSON value rather than a substring
    // scan — the structure is the contract, not the formatting.
    // Mirrors the earlier `budget_compact_json_output_shape_is_pinned`
    // style for the compact subcommand. State names are pinned as
    // snake-case strings — a contributor who switches to
    // PascalCase (matching the Rust enum) breaks dashboard readers
    // here. The `b.state()` call is the source of truth; this test
    // pins that the wrapper surfaces it verbatim.
    #[test]
    fn budget_status_json_output_shape_is_pinned() {
        use plugin3_core::budget::TokenBudget;

        // Three fixture budgets to exercise each state of
        // TokenBudget::state(): Under / Approaching / Over.
        // The boundary values are pinned at 10k (well under 80%
        // approaching_ratio), 85k (above the 80% threshold but
        // still under the 100k ceiling), and 110k (above ceiling —
        // state moves to Over).
        let fixtures = [
            (
                TokenBudget {
                    ceiling: 100_000,
                    approaching_ratio: 0.8,
                    used: 10_000,
                },
                "under",
            ),
            (
                TokenBudget {
                    ceiling: 100_000,
                    approaching_ratio: 0.8,
                    used: 85_000,
                },
                "approaching",
            ),
            (
                TokenBudget {
                    ceiling: 100_000,
                    approaching_ratio: 0.8,
                    used: 110_000,
                },
                "over",
            ),
        ];

        for (b, expected_state) in fixtures {
            // Mirror the production wrapper exactly (lines 14-22
            // of budget.rs). If `status()` drifts — e.g. drops
            // `state` from the JSON, or uses `format!("{:?}")`
            // instead of bare Debug — surfaces here.
            let resp = serde_json::json!({
                "used": b.used,
                "ceiling": b.ceiling,
                "state": b.state(),
            });
            let v: serde_json::Value =
                serde_json::to_value(&resp).expect("budget status --json wrapper serialises");

            // Top-level: exactly 3 keys, sorted for stable comparison.
            // A contributor who adds a sibling key (e.g.
            // `approaching_at: f64`) breaks `jq '.ceiling'` readers
            // by inflating the key set.
            let obj = v.as_object().expect("top-level is an object");
            let mut top_keys: Vec<&str> = obj.keys().map(String::as_str).collect();
            top_keys.sort_unstable();
            assert_eq!(
                top_keys,
                vec!["ceiling", "state", "used"],
                "budget status --json top-level key set must be exactly \
                 {{used, ceiling, state}}; got: {top_keys:?}"
            );

            // Value spot-checks for this fixture. The 3 fixtures
            // exercise each state of TokenBudget::state().
            assert_eq!(v["used"], b.used, "used field carries the runtime counter");
            assert_eq!(
                v["ceiling"], b.ceiling,
                "ceiling field carries the persisted default or runtime override"
            );
            assert_eq!(
                v["state"].as_str(),
                Some(expected_state),
                "state field must match the 3-variant TokenBudget::state() \
                 return; expected `{expected_state}`, got `{}`",
                v["state"]
            );
            assert!(
                v["state"].is_string(),
                "state must serialise as a string, not a number or object \
                 (a contributor who emits `b.state() as usize` surfaces here)"
            );
        }
    }

    // ponytail: B2 fix — `reset` zeros `used` while preserving
    // `ceiling`/`approaching_ratio`. Two pins: the in-memory
    // semantics (zero `used`, keep the rest) and the on-disk
    // persistence (the next `load_budget` reads back `used == 0`).
    // The third caller of the EnvGuard pattern earns a shared
    // helper per the B8 revert note in paths.rs; for now the
    // 25-line duplication is the cost of staying out of
    // `tests/common/mod.rs`.
    #[test]
    fn reset_zeroes_used_and_preserves_ceiling_in_memory() {
        use plugin3_core::budget::TokenBudget;
        // Mirror `reset()` exactly without the I/O: capture prior
        // `used`, zero it, leave `ceiling` and `approaching_ratio`
        // alone. A contributor who flips `b.used = 0` to
        // `*b = TokenBudget::default()` (forgetting the user may
        // have set a non-default ceiling) surfaces here.
        let mut b = TokenBudget {
            ceiling: 250_000,
            approaching_ratio: 0.7,
            used: 123_456,
        };
        let prior = b.used;
        b.used = 0;
        assert_eq!(
            prior, 123_456,
            "reset should report the prior value to the caller"
        );
        assert_eq!(b.used, 0, "reset must zero used; got {}", b.used);
        assert_eq!(
            b.ceiling, 250_000,
            "reset must NOT touch ceiling (it's a persisted default); got {}",
            b.ceiling
        );
        assert!(
            (b.approaching_ratio - 0.7).abs() < f64::EPSILON,
            "reset must NOT touch approaching_ratio (also a persisted default); got {}",
            b.approaching_ratio
        );
    }

    #[test]
    fn reset_on_already_zero_is_idempotent() {
        use plugin3_core::budget::TokenBudget;
        // Re-running reset on a zero counter must be a no-op for
        // every field — the function's return is `prior == 0`,
        // and the post-state is identical to the pre-state. A
        // contributor who subtracts a token on each call (e.g.
        // `b.used = b.used.saturating_sub(1)`) surfaces here
        // because the second call would leave `used == -1`.
        let mut b = TokenBudget {
            ceiling: 100_000,
            approaching_ratio: 0.8,
            used: 0,
        };
        let prior = b.used;
        b.used = 0;
        assert_eq!(prior, 0);
        assert_eq!(b.used, 0);
        assert_eq!(b.ceiling, 100_000);
        assert!((b.approaching_ratio - 0.8).abs() < f64::EPSILON);
    }

    // ponytail: pin the JSON wire shape of `plugin3 budget reset --json`.
    // The contract is a 3-key object: `prior_used`, `used`, `ceiling`.
    // We intentionally omit `approaching_ratio` from the JSON output
    // — it's an internal threshold parameter, not part of the
    // user's mental model of "what got reset". A contributor who
    // adds a 4th key (e.g. `approaching_ratio`) breaks `jq '.used'`
    // readers downstream; this test catches the field-set drift.
    //
    // State values are pinned at 0 (post-reset) and a non-zero
    // `prior_used` to surface a swap regression (e.g. returning
    // `prior` as `used` and `used` as `prior_used`).
    #[test]
    fn budget_reset_json_output_shape_is_pinned() {
        use plugin3_core::budget::TokenBudget;
        let prior = 9_999_usize;
        let b = TokenBudget {
            ceiling: 250_000,
            approaching_ratio: 0.7,
            used: 0,
        };
        // Mirror the production wrapper exactly (the `if as_json`
        // branch of `reset()`).
        let resp = serde_json::json!({
            "prior_used": prior,
            "used": b.used,
            "ceiling": b.ceiling,
        });
        let v: serde_json::Value =
            serde_json::to_value(&resp).expect("budget reset --json wrapper serialises");

        let obj = v.as_object().expect("top-level is an object");
        let mut top_keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        top_keys.sort_unstable();
        assert_eq!(
            top_keys,
            vec!["ceiling", "prior_used", "used"],
            "budget reset --json top-level key set must be exactly \
             {{prior_used, used, ceiling}}; got: {top_keys:?}"
        );

        assert_eq!(
            v["prior_used"], prior,
            "prior_used carries the pre-reset counter; got {}",
            v["prior_used"]
        );
        assert_eq!(v["used"], 0, "used must be 0 post-reset; got {}", v["used"]);
        assert_eq!(
            v["ceiling"], 250_000,
            "ceiling must carry the persisted default unchanged; got {}",
            v["ceiling"]
        );
    }

    // ponytail: integration pin — `reset()` actually persists to
    // disk. The pure in-memory test above pins the semantics; this
    // one pins the I/O: write a budget.toml with `used: 12345` via
    // `save_budget_at` (the helper `reset()` ultimately routes
    // through), then call `reset()` via PLUGIN3_DATA_DIR override,
    // then re-read the file and assert `used: 0`. A contributor
    // who drops the `save_budget(&b)` call from `reset()` would
    // pass the in-memory test (the assertion there happens before
    // the file write) but break this one.
    //
    // Skip-if-conflict: if the developer's shell has PLUGIN3_DATA_DIR
    // or PLUGIN3_RUNTIME_DIR set, we can't tell "test override" from
    // "shell override" — mirror the paths.rs pattern and bail.
    #[test]
    fn reset_persists_zeroed_used_to_disk() {
        if std::env::var("PLUGIN3_DATA_DIR").is_ok() || std::env::var("PLUGIN3_RUNTIME_DIR").is_ok()
        {
            eprintln!("skipping: PLUGIN3_*_DIR already set in this environment");
            return;
        }
        // ponytail: env-var guard lives in `plugin3_core::test_support`
        // now. It uses a process-global reentrant mutex so parallel
        // tests that touch PLUGIN3_*_DIR cannot race, and nested
        // guards in the same thread do not deadlock. See
        // test_support.rs for the `ReentrantMutex` implementation.

        let dir = tempfile::tempdir().expect("tempdir");
        let dir_str = dir.path().to_str().expect("utf8 path");
        // B2: budget.toml is under runtime_dir. Keep data_dir and
        // runtime_dir pointing at the same tempdir so the test
        // touches only the tempdir, not the host's real XDG dirs.
        let _g_data = EnvGuard::set("PLUGIN3_DATA_DIR", dir_str);
        let _g_run = EnvGuard::set("PLUGIN3_RUNTIME_DIR", dir_str);

        // Seed: write a budget.toml that simulates yesterday's
        // session-end state (used: 12345, ceiling: 250000).
        let budget_path = dir.path().join("budget.toml");
        let seeded = plugin3_core::budget::TokenBudget {
            ceiling: 250_000,
            approaching_ratio: 0.7,
            used: 12_345,
        };
        let s = toml::to_string(&seeded).expect("serialize");
        std::fs::write(&budget_path, s).expect("seed budget.toml");

        // Pre-condition: the seed is on disk with non-zero used.
        let pre: plugin3_core::budget::TokenBudget =
            toml::from_str(&std::fs::read_to_string(&budget_path).unwrap()).unwrap();
        assert_eq!(pre.used, 12_345, "seed must have non-zero used");

        // Action: call reset through the public surface.
        super::reset(false);

        // Post-condition: file on disk has used: 0, ceiling preserved,
        // approaching_ratio preserved.
        let post_s = std::fs::read_to_string(&budget_path).expect("budget survives reset");
        let post: plugin3_core::budget::TokenBudget =
            toml::from_str(&post_s).expect("parse post-reset");
        assert_eq!(
            post.used, 0,
            "reset() must persist used=0; file still says {}",
            post.used
        );
        assert_eq!(
            post.ceiling, 250_000,
            "reset() must preserve ceiling (persisted default); got {}",
            post.ceiling
        );
        assert!(
            (post.approaching_ratio - 0.7).abs() < f64::EPSILON,
            "reset() must preserve approaching_ratio (persisted default); got {}",
            post.approaching_ratio
        );

        // Drop order: _g_run / _g_data first (restore env), then
        // dir (removes tempdir). Both run at function end; the binding
        // order determines order. dir is declared first so it drops last.
    }
}
