//! `TokenBudget` — three-state guard with auto-intervention.
//! Per ADR-0005.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BudgetState {
    Under,
    Approaching,
    Over,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct TokenBudget {
    pub ceiling: usize,
    pub approaching_ratio: f64,
    pub used: usize,
}

impl Default for TokenBudget {
    fn default() -> Self {
        Self {
            ceiling: 200_000,
            approaching_ratio: 0.8,
            used: 0,
        }
    }
}

// ponytail: `BudgetConfig` is the user-editable subset of `TokenBudget`
// that survives across sessions via `config.toml` (ADR-0005 § Defaults).
// The runtime `used` counter is session-local, so it is intentionally
// absent here. The TOML shape mirrors ADR-0005 § Defaults:
//   [budget]
//   ceiling = 200_000
//   approaching_ratio = 0.8
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct BudgetConfig {
    pub ceiling: usize,
    pub approaching_ratio: f64,
}

impl Default for BudgetConfig {
    fn default() -> Self {
        Self {
            ceiling: 200_000,
            approaching_ratio: 0.8,
        }
    }
}

// ponytail: a thin wrapper so serde emits a `[budget]` section header
// (ADR-0005 § Defaults example). Bare-serialising `BudgetConfig`
// produces flat key=value pairs without the header, which diverges
// from the documented shape. Future sections (anchors per ADR-0011,
// compact policy per ADR-0008, usage per ADR-0010) get added as
// siblings here.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ConfigFile {
    #[serde(default)]
    pub budget: BudgetConfig,
    #[serde(default)]
    pub usage: UsageConfig,
}

// ponytail: lives in budget.rs (not cost.rs) so `ConfigFile` can
// name it without crossing module boundaries. The shape mirrors
// ADR-0010 § Privacy:
//   [usage]
//   enabled = false
// Default is `true` — disabling reporting is opt-out-by-write,
// not silent.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageConfig {
    pub enabled: bool,
}

impl Default for UsageConfig {
    fn default() -> Self {
        Self { enabled: true }
    }
}

impl TokenBudget {
    #[must_use]
    pub fn state(&self) -> BudgetState {
        if self.ceiling == 0 {
            return BudgetState::Over;
        }
        // ponytail: used/ceiling are token counts; exceeding f64's 53-bit
        // mantissa requires >4.5e15 tokens, far outside any realistic MVP
        // session. The cast is safe for all practical ceilings.
        #[allow(clippy::cast_precision_loss)]
        let ratio = self.used as f64 / self.ceiling as f64;
        if ratio >= 1.0 {
            BudgetState::Over
        } else if ratio >= self.approaching_ratio {
            BudgetState::Approaching
        } else {
            BudgetState::Under
        }
    }

    #[must_use]
    pub fn can_send(&self, incoming: usize) -> bool {
        self.used.saturating_add(incoming) <= self.ceiling
    }

    pub fn record(&mut self, n: usize) {
        self.used = self.used.saturating_add(n);
    }
    #[must_use]
    pub fn remaining(&self) -> usize {
        self.ceiling.saturating_sub(self.used)
    }
}

/// Slice overhead: typical marker + head/tail framing (ADR-0005).
pub const SLICE_OVERHEAD: usize = 256;

// ponytail: the enum now derives Serialize/Deserialize with the
// same tagged-enum shape as `plugin3_hosts::UserPromptSubmitResponse`
// (`tag = "kind"`, `rename_all = "snake_case"`). The two enums
// stay separate (one lives in core for budget logic; one in
// hosts for the canonical wire shape) but the serde rules make
// them byte-equivalent on the wire — `plugin3-cli/src/hooks::mod`
// round-trips through serde instead of a hand-written 4-arm match
// that duplicates the variant list. Adding a 5th variant now
// requires updating only the enum body and the corresponding
// `UserPromptSubmitResponse` variant; the serde rules carry the
// rename + tag work in both places.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Intervention {
    Allow,
    Warn {
        remaining: usize,
    },
    /// Slice the largest recent tool output to free `slice_to` bytes (target).
    Slice {
        target_key: String,
        slice_to: usize,
    },
    Compact {
        reason: String,
    },
}

#[must_use]
pub fn decide(budget: &TokenBudget, incoming: usize, recent: &[(String, usize)]) -> Intervention {
    if budget.can_send(incoming) {
        return match budget.state() {
            BudgetState::Under => Intervention::Allow,
            BudgetState::Approaching => Intervention::Warn {
                remaining: budget.remaining(),
            },
            BudgetState::Over => Intervention::Warn { remaining: 0 },
        };
    }
    let needed = incoming.saturating_sub(budget.remaining());
    if let Some((key, size)) = recent.iter().max_by_key(|(_, s)| *s) {
        // ponytail: saturating_add so a pathological budget (ceiling=0
        // or incoming/usize::MAX) doesn't wrap the threshold comparison
        // in release and panic in debug. Pre-fix, needed + SLICE_OVERHEAD
        // could overflow, making the Slice branch fire with slice_to=0.
        if *size > needed.saturating_add(SLICE_OVERHEAD) {
            return Intervention::Slice {
                target_key: key.clone(),
                slice_to: size.saturating_sub(needed),
            };
        }
    }
    Intervention::Compact {
        reason: format!(
            "session at {}/{} tokens; cannot fit {} more",
            budget.used, budget.ceiling, incoming
        ),
    }
}

/// Heuristic token estimator — ADR-0005. Conservative.
#[must_use]
pub fn estimate_tokens(s: &str) -> usize {
    let bytes = s.len();
    let trimmed = s.trim_start();
    if trimmed.starts_with('{') || trimmed.starts_with('[') || trimmed.starts_with("fn ") {
        bytes / 3
    } else {
        bytes / 4
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_transitions() {
        let mut b = TokenBudget {
            ceiling: 100,
            approaching_ratio: 0.8,
            used: 0,
        };
        assert_eq!(b.state(), BudgetState::Under);
        b.record(80);
        assert_eq!(b.state(), BudgetState::Approaching);
        b.record(20);
        assert_eq!(b.state(), BudgetState::Over);
    }

    // ponytail: pin the ceiling==0 short-circuit. The `state()`
    // impl guards the ratio division (`used / ceiling`) with an
    // explicit ceiling==0 → Over return. A contributor who
    // removes the guard surfaces as a panic (or an Under state
    // for used=0/ceiling=0 which is wrong) — a user who set
    // ceiling=0 in config.toml would otherwise get NaN at the
    // divide and a misleading Allow through decide(). Pin both
    // the used=0 case (where the guard is the only thing keeping
    // us out of Under) and the used>0 case (where the guard is
    // redundant with the ratio>=1.0 branch but still the
    // documented contract). Also pin can_send: the only Allow
    // path on a zero ceiling is the degenerate zero-token case.
    #[test]
    fn state_with_zero_ceiling_is_over() {
        // used=0: would naively be Under (0/0). The guard catches this.
        let z = TokenBudget {
            ceiling: 0,
            approaching_ratio: 0.8,
            used: 0,
        };
        assert_eq!(
            z.state(),
            BudgetState::Over,
            "zero ceiling must be Over (spec: never allow)"
        );
        // used>0: the ratio>=1.0 branch would also produce Over,
        // but the guard is the documented contract.
        let z_used = TokenBudget {
            ceiling: 0,
            approaching_ratio: 0.8,
            used: 1,
        };
        assert_eq!(z_used.state(), BudgetState::Over);
        // can_send: only 0 fits; anything else is rejected.
        assert!(
            !z.can_send(1),
            "ceiling=0 must reject any non-zero send (spec: never allow)"
        );
    }

    #[test]
    fn decide_allows_when_under_ceiling() {
        let b = TokenBudget {
            ceiling: 100,
            approaching_ratio: 0.8,
            used: 10,
        };
        assert_eq!(decide(&b, 50, &[]), Intervention::Allow);
    }

    #[test]
    fn decide_warns_when_approaching() {
        let b = TokenBudget {
            ceiling: 100,
            approaching_ratio: 0.8,
            used: 85,
        };
        assert!(matches!(
            decide(&b, 5, &[]),
            Intervention::Warn { remaining: 15 }
        ));
    }

    #[test]
    fn decide_slices_largest_when_over() {
        let b = TokenBudget {
            ceiling: 100,
            approaching_ratio: 0.8,
            used: 90,
        };
        let recent = vec![("a".into(), 5_000), ("b".into(), 50_000)];
        // ponytail: pin the exact arithmetic. needed=20, size=50_000,
        // slice_to=49_980. A contributor who swaps `saturating_sub`
        // for plain `sub` or changes the `needed + SLICE_OVERHEAD`
        // check surfaces here.
        match decide(&b, 30, &recent) {
            Intervention::Slice {
                target_key,
                slice_to,
            } => {
                assert_eq!(target_key, "b");
                assert_eq!(slice_to, 49_980);
            }
            other => panic!("expected Slice, got {other:?}"),
        }
    }

    // ponytail: pin the strict-`>` boundary on the size vs
    // (needed + SLICE_OVERHEAD) check. The guard rejects slices
    // where `size <= needed + SLICE_OVERHEAD` because after slicing
    // to `size - needed` bytes there'd be less than SLICE_OVERHEAD
    // bytes of payload left — the marker + framing alone would
    // consume more than the kept payload, which is silly. The
    // boundary is `>` not `>=`, so `size == needed + SLICE_OVERHEAD`
    // must Compact, not Slice. A contributor who flips to `>=`
    // (off-by-one cheap-win) accepts a slice whose kept payload
    // is smaller than its own marker — surfaces here.
    #[test]
    fn decide_size_at_boundary_emits_compact_not_slice() {
        let b = TokenBudget {
            ceiling: 100,
            approaching_ratio: 0.8,
            used: 100,
        };
        // needed = incoming - remaining = 30 - 0 = 30.
        // Threshold = needed + SLICE_OVERHEAD = 30 + 256 = 286.
        // The strict `>` requires size > 286 to Slice.
        for (size, label) in [
            (286_usize, "size == needed + SLICE_OVERHEAD"),
            (285_usize, "size == needed + SLICE_OVERHEAD - 1"),
            (30_usize, "size == needed (well under threshold)"),
        ] {
            let recent = vec![("a".into(), size)];
            match decide(&b, 30, &recent) {
                Intervention::Compact { .. } => {}
                other => panic!(
                    "{label} (size={size}) must fall through to Compact, \
                     got {other:?} — off-by-one in the `size > needed + \
                     SLICE_OVERHEAD` check would silently change this"
                ),
            }
        }
        // And the Slice branch DOES fire at exactly size = threshold + 1.
        // Pin the off-by-one in the *other* direction: a contributor
        // who flips `>` to `==` accepts the boundary and rejects the
        // first legitimate slice — surfaces here.
        let recent = vec![("a".into(), 287)];
        match decide(&b, 30, &recent) {
            Intervention::Slice {
                target_key,
                slice_to,
            } => {
                assert_eq!(target_key, "a");
                // slice_to = size - needed = 287 - 30 = 257.
                assert_eq!(
                    slice_to, 257,
                    "size=287 (first above threshold) must Slice to 257 bytes"
                );
            }
            other => panic!("size=287 (first above threshold) must Slice, got {other:?}"),
        }
    }

    #[test]
    fn decide_compacts_when_no_slice_fits() {
        // ponytail: non-empty recent where the largest entry is
        // still too small to free the needed bytes. The path
        // differs from `decide_over_with_empty_recent_yields_compact`
        // (empty recent): here the orchestrator DID evaluate a
        // candidate, found `size > needed + SLICE_OVERHEAD` false,
        // and fell through to Compact. The reason string is the
        // same shape as the empty-recent case because it doesn't
        // depend on the recent list — pin both the variant and
        // the reason so a contributor who swaps the fall-through
        // for a silent Slice with a default key surfaces here.
        let b = TokenBudget {
            ceiling: 100,
            approaching_ratio: 0.8,
            used: 100,
        };
        let recent = vec![("a".into(), 5)];
        let Intervention::Compact { reason } = decide(&b, 30, &recent) else {
            panic!("expected Compact, got: {:?}", decide(&b, 30, &recent));
        };
        assert_eq!(reason, "session at 100/100 tokens; cannot fit 30 more");
    }

    // ponytail: guard against overflow in the `needed + SLICE_OVERHEAD`
    // threshold. With ceiling=0, any non-zero incoming makes needed
    // enormous; adding SLICE_OVERHEAD must not wrap back down and make
    // the Slice branch fire with slice_to=0 (or panic in debug).
    #[test]
    fn decide_overflow_guard_yields_compact_not_zero_slice() {
        let b = TokenBudget {
            ceiling: 0,
            approaching_ratio: 0.8,
            used: 0,
        };
        let recent = vec![("big".into(), usize::MAX)];
        match decide(&b, usize::MAX, &recent) {
            Intervention::Compact { .. } => {}
            other => panic!("expected Compact when threshold would overflow, got {other:?}"),
        }
    }

    // ponytail: pin `recent.iter().max_by_key(...)` tie-breaking
    // (last-max-wins). `Iterator::max_by_key` returns the LAST
    // element with the maximum key per the stdlib contract —
    // when several recent tool outputs are equally large, the
    // newest of them (later index in the VecDeque) is the one
    // auto-sliced. A contributor who flips to `min_by_key`
    // (smallest-wins) or `.next()` (first-max-wins) silently
    // changes which tool output the host drops to free the
    // ceiling. Pin it: identical sizes, the LAST entry's key
    // must surface as `target_key`.
    #[test]
    fn decide_ties_break_last_max_wins() {
        let b = TokenBudget {
            ceiling: 100,
            approaching_ratio: 0.8,
            used: 90,
        };
        // Three equal-sized recent outputs — sized to fit the
        // Slice branch (size > needed + SLICE_OVERHEAD = 20 +
        // 256 = 276, so 500 > 276 holds).
        let recent = vec![
            ("old".into(), 500),
            ("middle".into(), 500),
            ("newest".into(), 500),
        ];
        match decide(&b, 30, &recent) {
            Intervention::Slice {
                target_key,
                slice_to,
            } => {
                assert_eq!(
                    target_key, "newest",
                    "max_by_key on equal sizes must return the LAST max (newest wins) \
                     per the stdlib contract; flipping to min_by_key or .next() \
                     silently changes which tool output the host drops"
                );
                // slice_to = size - needed = 500 - 20 = 480.
                assert_eq!(slice_to, 480);
            }
            other => panic!("expected Slice, got {other:?}"),
        }
    }

    #[test]
    fn estimator_handles_json_and_prose() {
        // ponytail: the previous shape was a near-tautology —
        // `> 0` always holds for any non-empty string because
        // both branches do `bytes / 3` or `bytes / 4` (≥ 1 for
        // inputs ≥ 3 bytes). It never pinned the JSON-vs-prose
        // division rates, which are load-bearing for the
        // UserPromptSubmit guard — JSON tends to have higher
        // tokens-per-byte than prose, so the divisor is lower.
        // A contributor who flips `/3` to `/4` (or vice versa)
        // silently changes the token cost of every JSON prompt
        // and shifts the Slice boundary. Pin both ratios with
        // the exact division so the spec's `bytes / 3 for
        // JSON/prose-code, bytes / 4 for prose` is checked.

        // JSON branch: starts with `{` → bytes / 3.
        // Fixture: 9 bytes `{"k":"v"}` → 9 / 3 = 3 tokens.
        assert_eq!(
            estimate_tokens(r#"{"k":"v"}"#),
            3,
            "JSON-shape ({{ at start) must divide bytes by 3; \
             9 bytes / 3 = 3 tokens"
        );

        // JSON-array branch: starts with `[`.
        // Fixture: 12 bytes `[1, 2, 3, 4]` → 12 / 3 = 4 tokens.
        assert_eq!(
            estimate_tokens("[1, 2, 3, 4]"),
            4,
            "array-shape ([ at start) must divide bytes by 3"
        );

        // Prose-code branch: starts with `fn `.
        // Fixture: 13 bytes `fn hello() {}` → 13 / 3 = 4 tokens.
        assert_eq!(
            estimate_tokens("fn hello() {}"),
            4,
            "code-shape (fn  at start) must divide bytes by 3"
        );

        // Prose branch: anything else → bytes / 4.
        // Fixture: 11 bytes `hello world` → 11 / 4 = 2 tokens.
        assert_eq!(
            estimate_tokens("hello world"),
            2,
            "prose-shape (anything else) must divide bytes by 4; \
             11 bytes / 4 = 2 tokens"
        );

        // Leading whitespace is trimmed before the branch check.
        // A prompt with leading newlines is still JSON-shaped.
        // Fixture: `   {"k":"v"}` is 12 bytes → 12 / 3 = 4 tokens.
        assert_eq!(
            estimate_tokens(r#"   {"k":"v"}"#),
            4,
            "leading whitespace is trimmed before the JSON check; \
             a prompt with leading newlines is still JSON-shaped"
        );

        // Sanity: empty input → 0 (the divisor leaves 0 unchanged).
        assert_eq!(
            estimate_tokens(""),
            0,
            "empty input must estimate 0 tokens (0 / 3 = 0)"
        );
    }

    #[test]
    fn budget_config_round_trips_via_toml() {
        // ponytail: pin the on-disk shape for ADR-0005's [budget] section.
        // The `budget set --default` writer writes this exact key order;
        // a contributor who renames `approaching_ratio` will surface the
        // change here rather than via a mysterious default-regression.
        let cfg = BudgetConfig {
            ceiling: 300_000,
            approaching_ratio: 0.75,
        };
        let s = toml::to_string(&cfg).expect("serialise");
        assert!(s.contains("ceiling = 300000"), "got: {s}");
        assert!(s.contains("approaching_ratio = 0.75"), "got: {s}");
        let back: BudgetConfig = toml::from_str(&s).expect("parse");
        assert_eq!(back, cfg);
    }

    #[test]
    fn budget_config_defaults_match_token_budget() {
        // ponytail: defaults must agree so a session starting on a
        // fresh config.toml behaves identically to one starting with
        // TokenBudget::default(). A drift here would be invisible in
        // tests because the runtime file shadows it on next record.
        assert_eq!(
            BudgetConfig::default().ceiling,
            TokenBudget::default().ceiling,
        );
        assert!(
            (BudgetConfig::default().approaching_ratio - TokenBudget::default().approaching_ratio)
                .abs()
                < f64::EPSILON,
            "BudgetConfig and TokenBudget default approaching_ratio must match"
        );
    }

    #[test]
    fn config_file_emits_budget_section_header() {
        // ponytail: pin the on-disk shape to ADR-0005 § Defaults.
        // The wrapper exists *only* to emit the `[budget]` section
        // header. A contributor who removes `ConfigFile` to "simplify"
        // will surface that decision via this test.
        let file = ConfigFile {
            budget: BudgetConfig {
                ceiling: 200_000,
                approaching_ratio: 0.8,
            },
            usage: UsageConfig::default(),
        };
        let s = toml::to_string(&file).expect("serialise");
        assert!(s.contains("[budget]"), "missing [budget] header, got: {s}");
        assert!(s.contains("ceiling = 200000"), "got: {s}");
        assert!(s.contains("approaching_ratio = 0.8"), "got: {s}");
        let back: ConfigFile = toml::from_str(&s).expect("parse");
        assert_eq!(back.budget, file.budget);
    }

    // ponytail: pin state() at exact boundaries. The spec
    // uses `>= approaching_ratio` (inclusive at the threshold)
    // and `>= 1.0` (inclusive at the ceiling). A contributor
    // who flips a `>=` to `>` shrinks the Approaching band by
    // one float unit and the Over band by the same — the
    // existing test only checks at integers 80/100 and would
    // miss this. Hard-coded ratios so the test catches a
    // constant change without self-referencing.
    #[test]
    fn state_at_exact_boundaries_is_pinned() {
        let ceiling: usize = 1000;
        // (used, expected, label)
        let rows: &[(usize, BudgetState, &str)] = &[
            (0, BudgetState::Under, "used=0"),
            (799, BudgetState::Under, "used just below 0.8 band"),
            (
                800,
                BudgetState::Approaching,
                "used exactly at 0.8 → Approaching (>=)",
            ),
            (801, BudgetState::Approaching, "used just above 0.8"),
            (999, BudgetState::Approaching, "used just below 1.0"),
            (
                1000,
                BudgetState::Over,
                "used exactly at ceiling → Over (>=)",
            ),
            (1001, BudgetState::Over, "used above ceiling"),
        ];
        for (used, expected, label) in rows {
            let b = TokenBudget {
                ceiling,
                approaching_ratio: 0.8,
                used: *used,
            };
            #[allow(clippy::cast_precision_loss)]
            let ratio = *used as f64 / ceiling as f64;
            assert_eq!(b.state(), *expected,
                "row `{label}`: used={used} ceiling={ceiling} ratio={ratio} expected {expected:?} got {:?}",
                b.state());
        }
    }

    // ponytail: pin the degenerate-but-spec'd Over+can_send path.
    // used == ceiling, incoming == 0 → can_send is true (used +
    // incoming <= ceiling), but state() is Over. Per the source,
    // this emits Warn { remaining: 0 }. A contributor who removes
    // the Over arm from the inner match (thinking "Over implies
    // can_send is false") surfaces here.
    #[test]
    fn decide_at_over_with_zero_incoming_emits_warn_with_zero_remaining() {
        let b = TokenBudget {
            ceiling: 100,
            approaching_ratio: 0.8,
            used: 100,
        };
        assert_eq!(b.state(), BudgetState::Over);
        // can_send(0) is true (100 + 0 <= 100) — degenerate but spec'd.
        assert!(b.can_send(0));
        match decide(&b, 0, &[]) {
            Intervention::Warn { remaining } => {
                assert_eq!(
                    remaining, 0,
                    "Over state with incoming=0 must report remaining=0, got {remaining}"
                );
            }
            other => panic!("expected Warn{{remaining:0}} at Over+incoming=0, got {other:?}"),
        }
    }

    // ponytail: pin the empty-recent Compact path. When the
    // budget is over AND the recent list is empty, there is
    // nothing to slice — the only option is Compact. A
    // contributor who returns Slice with a default key (e.g.
    // key="") instead of Compact silently makes the host try
    // to slice a non-existent output.
    #[test]
    fn decide_over_with_empty_recent_yields_compact() {
        let b = TokenBudget {
            ceiling: 100,
            approaching_ratio: 0.8,
            used: 100,
        };
        // used=100, incoming=30 → can_send is false (130 > 100).
        // recent is empty → no candidate to slice.
        match decide(&b, 30, &[]) {
            Intervention::Compact { reason } => {
                // ponytail: pin the reason string format. The
                // shell-glueable form `session at USED/CEILING
                // tokens; cannot fit INCOMING more` is consumed
                // by downstream tools (e.g. user grep scripts,
                // log dashboards). A contributor who rewrites it
                // to "over budget" loses the structured shape.
                assert_eq!(reason, "session at 100/100 tokens; cannot fit 30 more");
            }
            other => panic!("expected Compact, got {other:?}"),
        }
    }

    // ponytail: pin the reason-string format across more
    // (used, ceiling, incoming) combinations. The format is
    // the only structured field in Intervention::Compact and
    // downstream tools (log dashboards, user greps) may parse
    // it. A contributor who inserts a comma or changes the
    // separator surfaces here.
    #[test]
    fn compact_reason_string_format_is_pinned() {
        let b = TokenBudget {
            ceiling: 200_000,
            approaching_ratio: 0.8,
            used: 195_000,
        };
        let Intervention::Compact { reason } = decide(&b, 10_000, &[]) else {
            panic!("expected Compact");
        };
        assert_eq!(
            reason, "session at 195000/200000 tokens; cannot fit 10000 more",
            "reason format drift (separator / field order)",
        );
    }

    // ponytail: pin the `Intervention` wire shape — the bridge
    // to `plugin3_hosts::UserPromptSubmitResponse` rides on this.
    // Both enums are `#[serde(tag = "kind", rename_all = "snake_case")]`
    // over the same four-variant shape. A contributor who drops
    // the serde derives on `Intervention` or changes the tag/rename
    // rule desyncs the CLI from the canonical host shim — the host
    // sees `"Allow"` (unit variant spelling) instead of
    // `{"kind":"allow"}` (tagged-enum spelling). Pin each variant's
    // JSON shape so the regression is observable.
    #[test]
    fn intervention_wire_shape_matches_user_prompt_submit_response() {
        // ponytail: tagged-enum spelling — `"kind":"<variant>"`,
        // snake_case for the variant name, payload fields inline.
        let allow = serde_json::to_value(&Intervention::Allow).unwrap();
        assert_eq!(
            allow,
            serde_json::json!({"kind": "allow"}),
            "Allow must serialise as a tagged-enum {{kind: allow}} object"
        );
        let warn = serde_json::to_value(&Intervention::Warn { remaining: 42 }).unwrap();
        assert_eq!(
            warn,
            serde_json::json!({"kind": "warn", "remaining": 42}),
            "Warn {{remaining}} must serialise with kind=warn and inline remaining"
        );
        let slice = serde_json::to_value(&Intervention::Slice {
            target_key: "abc".into(),
            slice_to: 100,
        })
        .unwrap();
        assert_eq!(
            slice,
            serde_json::json!({
                "kind": "slice", "target_key": "abc", "slice_to": 100,
            }),
            "Slice must serialise with kind=slice and inline target_key + slice_to"
        );
        let compact = serde_json::to_value(&Intervention::Compact {
            reason: "session at 100/100 tokens".into(),
        })
        .unwrap();
        assert_eq!(
            compact,
            serde_json::json!({
                "kind": "compact", "reason": "session at 100/100 tokens",
            }),
            "Compact must serialise with kind=compact and inline reason"
        );
    }

    // ponytail: round-trip a tagged enum. A contributor who flips
    // the serde derive off `Intervention` (or to a different tag)
    // breaks parse-back for the cost reporter's records and the
    // host's response handling. This test is the only signal
    // that catches a clean serde-derive drop on the enum (other
    // tests assert behaviour, not the wire contract).
    #[test]
    fn intervention_round_trips_via_json() {
        for original in [
            Intervention::Allow,
            Intervention::Warn { remaining: 100 },
            Intervention::Slice {
                target_key: "k1".into(),
                slice_to: 50,
            },
            Intervention::Compact {
                reason: "over budget".into(),
            },
        ] {
            let s = serde_json::to_string(&original).expect("serialise");
            let back: Intervention =
                serde_json::from_str(&s).expect("Intervention must round-trip via JSON");
            assert_eq!(
                back, original,
                "{original:?} must round-trip — wire shape drift breaks \
                 `plugin3 hook user-prompt-submit`'s response contract"
            );
        }
    }

    // ponytail: pin the saturating arithmetic on `remaining`,
    // `record`, and `can_send`. A contributor who replaces
    // `saturating_*` with plain `+`/`-` panics in debug builds
    // when `used > ceiling` (which the state machine permits —
    // used can grow past ceiling between record() calls) or when
    // `incoming + used` overflows usize (a hostile config).
    //
    // Three independent invariants to anchor:
    //   - remaining() = ceiling.saturating_sub(used) — clamps to 0
    //     when used exceeds ceiling (cannot return negative).
    //   - record() = used.saturating_add(n) — clamps to usize::MAX
    //     on overflow rather than wrapping to a tiny used value.
    //   - can_send(incoming) = used.saturating_add(incoming) <= ceiling
    //     — same overflow guard, false on impossible fits.
    //
    // The `> ceiling` case is reachable in practice: a session
    // over ceiling emits Compact and stops sending, but `used`
    // stays at the post-record value. Querying `remaining()` then
    // would underflow without saturating_sub.
    #[test]
    fn remaining_and_record_and_can_send_use_saturating_arithmetic() {
        // 1) remaining() clamps to 0 when used > ceiling.
        let b = TokenBudget {
            ceiling: 100,
            approaching_ratio: 0.8,
            used: 200,
        };
        assert_eq!(
            b.remaining(),
            0,
            "remaining() must clamp to 0 (saturating_sub) when used > ceiling; \
             got {} for ceiling=100, used=200",
            b.remaining()
        );

        // 2) record() clamps to usize::MAX on overflow. We can't
        //    push used all the way to usize::MAX (would take a
        //    64-bit loop) — assert that adding a gigantic n lands
        //    near the top without panicking, and that subsequent
        //    state() is Over (not Under after a wraparound).
        let mut b2 = TokenBudget {
            ceiling: usize::MAX,
            approaching_ratio: 0.8,
            used: usize::MAX - 10,
        };
        b2.record(100); // used would wrap to 90 without saturating
        assert_eq!(
            b2.used,
            usize::MAX,
            "record() must clamp to usize::MAX on overflow; got used={}",
            b2.used
        );
        assert_eq!(
            b2.state(),
            BudgetState::Over,
            "post-overflow state must be Over; a wraparound-to-tiny would surface as Under"
        );

        // 3) can_send() does NOT panic on overflow. used = near-max,
        //    incoming = large — used + incoming would overflow
        //    without saturating_add, producing a tiny used+incoming
        //    that fits in ceiling. Pin the non-panic + the contract:
        //    post-saturation, the comparison runs against the
        //    ceiling, so the result is determined by the actual
        //    math (not by a wrap-around).
        let fits = TokenBudget {
            ceiling: usize::MAX,
            approaching_ratio: 0.8,
            used: usize::MAX - 1,
        };
        assert!(
            fits.can_send(10),
            "(MAX-1) + 10 = MAX <= MAX → can_send returns true; the saturating_add \
             clamps rather than wrapping, so we don't get a spurious false-accept \
             from a wraparound-to-zero"
        );
        let over = TokenBudget {
            ceiling: usize::MAX / 2,
            approaching_ratio: 0.8,
            used: usize::MAX - 1,
        };
        assert!(
            !over.can_send(10),
            "ceiling=MAX/2 with used=MAX-1 must reject any send > 1; \
             without saturating_add the sum would wrap to a tiny number \
             and falsely fit"
        );

        // 4) can_send(0) is true at any non-negative used (degenerate but spec'd).
        let at_ceiling = TokenBudget {
            ceiling: 100,
            approaching_ratio: 0.8,
            used: 100,
        };
        assert!(
            at_ceiling.can_send(0),
            "can_send(0) at used==ceiling must return true (100+0 <= 100)"
        );
        let past_ceiling = TokenBudget {
            ceiling: 100,
            approaching_ratio: 0.8,
            used: 150,
        };
        assert!(
            !past_ceiling.can_send(0),
            "can_send(0) at used>ceiling must return false (150+0 > 100); \
             without saturating_sub on remaining this branch would panic"
        );
    }
}
