//! Compaction — `CompactHint` payload + `LocalSummaryCompactor` transform.
//! Per ADR-0008.

use serde::{Deserialize, Serialize};

use crate::budget::TokenBudget;
use crate::error::TransformError;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CompactHint {
    pub reason: String,
    pub tokens_used: usize,
    pub tokens_ceiling: usize,
    pub oldest_turn: Option<usize>,
    pub newest_turn: Option<usize>,
}

/// Turn index into the conversation history (host-side).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Turn {
    pub index: usize,
    pub role: String,
    pub content_preview: String,
}

#[must_use]
pub fn build_hint(budget: &TokenBudget, history: &[Turn]) -> CompactHint {
    CompactHint {
        reason: format!(
            "session at {}/{} tokens; compaction suggested",
            budget.used, budget.ceiling
        ),
        tokens_used: budget.used,
        tokens_ceiling: budget.ceiling,
        oldest_turn: history.first().map(|t| t.index),
        newest_turn: history.last().map(|t| t.index),
    }
}

// ---- LocalSummaryCompactor ---------------------------------------------

pub struct CompactedOutput {
    pub summary: String,
    pub bytes_saved: usize,
    pub lossy: bool,
}

pub trait CompactionTransform: Send + Sync {
    fn name(&self) -> &'static str;
    /// Transform `input` into a compacted form.
    ///
    /// # Errors
    ///
    /// Returns `TransformError::InvalidInput` for malformed input and
    /// `TransformError::Internal` for unexpected failures in the
    /// transform implementation.
    fn apply(&self, input: &str) -> Result<CompactedOutput, TransformError>;
}

/// Heuristic line filter — keeps the first non-empty short line of each
/// "paragraph", drops noisy long lines. ADR-0008: intentional crudeness.
#[must_use]
pub fn local_summarise(input: &str, max_bytes: usize) -> String {
    let mut out = String::new();
    for line in input.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if line.len() > 500 {
            continue;
        }
        // ponytail: pre-check the bound so a single line longer than
        // `max_bytes` doesn't blow past the cap. The earlier shape
        // (push, then check `out.len() >= max_bytes`) effectively
        // ignored the cap for any caller with `max_bytes < line.len()`
        // — a small-cap caller got a line-sized output, not a capped
        // one. The +1 is the trailing `\n` we are about to push.
        if out.len() + line.len() + 1 > max_bytes {
            break;
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}

pub struct LocalSummaryCompactor {
    pub max_output_bytes: usize,
}

impl Default for LocalSummaryCompactor {
    fn default() -> Self {
        Self {
            max_output_bytes: 8192,
        }
    }
}

impl CompactionTransform for LocalSummaryCompactor {
    fn name(&self) -> &'static str {
        "local_summary"
    }

    fn apply(&self, input: &str) -> Result<CompactedOutput, TransformError> {
        let summary = local_summarise(input, self.max_output_bytes);
        let lossy = summary.len() < input.len();
        Ok(CompactedOutput {
            bytes_saved: input.len().saturating_sub(summary.len()),
            summary,
            lossy,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_summarise_empty_input() {
        assert_eq!(local_summarise("", 1024), "");
    }

    #[test]
    fn local_summarise_short_input_kept() {
        let s = "first line\nsecond line\n";
        let out = local_summarise(s, 1024);
        assert!(out.contains("first line"));
        assert!(out.contains("second line"));
    }

    #[test]
    fn local_summarise_skips_empty_and_long_lines() {
        let long = "x".repeat(600);
        let input = format!("\n\nfirst\n{long}\nlast\n");
        let out = local_summarise(&input, 1024);
        assert!(out.contains("first"));
        assert!(!out.contains(&long));
        assert!(out.contains("last"));
    }

    #[test]
    fn local_summarise_respects_max_bytes() {
        let input = "a\nb\nc\nd\ne\nf\n";
        let out = local_summarise(input, 4); // tiny cap
                                             // ponytail: hard cap, no slack. "a\n" + "b\n" = 4 bytes; the
                                             // next line would push us to 6, so the loop breaks. The earlier
                                             // shape (push-then-check) tested `<= 8` because a single 8-byte
                                             // line could slip through. The pre-check pins the bound.
        assert_eq!(
            out, "a\nb\n",
            "max_bytes=4 must cap output at exactly the first two lines"
        );
    }

    // ponytail: pin the bound against a single line longer than the
    // cap. Before the pre-check fix the function pushed the whole line
    // before checking `out.len() >= max_bytes`, so `max_bytes=4` on an
    // input like "abcdefghij\n" produced 11 bytes of output. The
    // pre-check refuses to add a line that would exceed the cap.
    #[test]
    fn local_summarise_single_line_over_cap_does_not_blow_bound() {
        let input = "abcdefghij\n";
        let out = local_summarise(input, 4);
        assert_eq!(
            out, "",
            "a single line > max_bytes must NOT be pushed; the pre-check breaks \
             before the append, leaving the output empty"
        );
    }

    // ponytail: pin the bound for a realistic cap + a line that fits
    // alongside one that doesn't. The cap accepts "short\n" (6 bytes),
    // refuses the 200-byte long line, and the loop ends.
    #[test]
    fn local_summarise_drops_lines_exceeding_remaining_cap() {
        let long = "x".repeat(200);
        let input = format!("short\n{long}\n");
        let out = local_summarise(&input, 16);
        assert!(
            out.contains("short"),
            "a line that fits in the remaining cap must be kept"
        );
        assert!(
            !out.contains(&long),
            "a line that would exceed the remaining cap must NOT be pushed; \
             the pre-check breaks before the append"
        );
        assert!(
            out.len() <= 16,
            "output must respect the hard cap; got {} bytes for cap=16",
            out.len()
        );
    }

    #[test]
    fn build_hint_no_history() {
        let b = TokenBudget::default();
        let h = build_hint(&b, &[]);
        assert!(h.oldest_turn.is_none());
        assert!(h.newest_turn.is_none());
        // ponytail: pin the full phrase. A contributor who rewrites
        // it to "summarise the session" surfaces here, not via a
        // log-grep regression in some downstream tool.
        assert_eq!(h.reason, "session at 0/200000 tokens; compaction suggested");
    }

    #[test]
    fn build_hint_includes_turn_range() {
        let b = TokenBudget {
            ceiling: 100,
            approaching_ratio: 0.8,
            used: 95,
        };
        let turns = vec![
            Turn {
                index: 3,
                role: "user".into(),
                content_preview: "...".into(),
            },
            Turn {
                index: 7,
                role: "assistant".into(),
                content_preview: "...".into(),
            },
        ];
        let h = build_hint(&b, &turns);
        assert_eq!(h.oldest_turn, Some(3));
        assert_eq!(h.newest_turn, Some(7));
        assert_eq!(h.tokens_used, 95);
        assert_eq!(h.tokens_ceiling, 100);
    }

    // ponytail: pin the single-turn edge case. `first()` and `last()`
    // on a 1-element slice both return Some(index), so oldest ==
    // newest. A contributor who switches `history.last().map(...)`
    // to `history.first().map(...)` (typo under refactor) would
    // produce Some(3) for newest — wrong for any 2+ element history
    // but right for 1 element. Pinning both sides of the boundary
    // catches the regression on the 2+ side (above test) AND the
    // 1 side (here), so neither typo can pass.
    #[test]
    fn build_hint_single_turn_history_sets_oldest_equal_newest() {
        let b = TokenBudget::default();
        let turns = vec![Turn {
            index: 42,
            role: "user".into(),
            content_preview: "x".into(),
        }];
        let h = build_hint(&b, &turns);
        assert_eq!(h.oldest_turn, Some(42));
        assert_eq!(
            h.newest_turn,
            Some(42),
            "single-turn history must have oldest == newest (same index)"
        );
    }

    // ponytail: pin the JSON wire shape that `run_pre_compact` and
    // `budget_compact` both emit. A contributor who renames a field
    // surfaces the change here before a host breaks.
    #[test]
    fn compact_hint_serialises_expected_fields() {
        let h = CompactHint {
            reason: "session at 95/100".into(),
            tokens_used: 95,
            tokens_ceiling: 100,
            oldest_turn: Some(3),
            newest_turn: Some(7),
        };
        let v: serde_json::Value = serde_json::to_value(&h).expect("serialise");
        let obj = v.as_object().expect("object");
        let keys: std::collections::BTreeSet<&str> =
            obj.keys().map(std::string::String::as_str).collect();
        assert_eq!(
            keys,
            [
                "newest_turn",
                "oldest_turn",
                "reason",
                "tokens_ceiling",
                "tokens_used"
            ]
            .into_iter()
            .collect(),
        );
        assert_eq!(obj["tokens_used"], 95);
        assert_eq!(obj["oldest_turn"], 3);
    }

    // ponytail: pin the ADR-0003 § LocalSummaryCompactor default.
    // The spec calls for `max_output_bytes: 8192` — a contributor
    // who tunes it (8192 → 4096) silently halves the output budget
    // for every compaction event. The `compactor_reports_lossy_correctly`
    // test uses an explicit `max_output_bytes: 16`, so this default
    // change would slip past it.
    #[test]
    fn local_summary_compactor_default_matches_adr() {
        let c = LocalSummaryCompactor::default();
        assert_eq!(
            c.max_output_bytes, 8192,
            "ADR-0003 § LocalSummaryCompactor spec: max_output_bytes default = 8192"
        );
    }

    // ponytail: pin the transform's name. ADR-0003 § LocalSummaryCompactor
    // names it `"local_summary"` — a contributor who shortens to
    // `"ls"` or `""` silently breaks dashboards that filter by name.
    #[test]
    fn local_summary_compactor_name_is_pinned() {
        let c = LocalSummaryCompactor::default();
        assert_eq!(CompactionTransform::name(&c), "local_summary");
    }

    #[test]
    fn compactor_reports_lossy_correctly() {
        let c = LocalSummaryCompactor {
            max_output_bytes: 16,
        };
        let input = "a\n".repeat(1000);
        let out = c.apply(&input).unwrap();
        assert!(out.lossy);
        assert!(out.bytes_saved > 0);
        assert_eq!(CompactionTransform::name(&c), "local_summary");
    }

    // ---- Property tests (ADR-0016) — no-panic on arbitrary inputs.

    fn lcg_inputs() -> Vec<String> {
        let mut out: Vec<String> = vec![
            String::new(),
            "\n".repeat(10_000),
            "你".repeat(2_000),
            "🦀".repeat(2_000),
            format!("{}{}", "x".repeat(7), "你".repeat(2_000)),
        ];
        let mut state: u64 = 0x00c0_ffee_1234_5678;
        for _ in 0..50 {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            let n = ((state >> 32) as usize) % 4096 + 8;
            let pick = (state >> 8) as usize % 3;
            let chunk: String = match pick {
                0 => "a".repeat(n),
                1 => "你".repeat(n / 3 + 1),
                _ => "🦀".repeat(n / 4 + 1),
            };
            out.push(chunk);
        }
        out
    }

    #[test]
    fn no_panic_on_any_input() {
        let c = LocalSummaryCompactor::default();
        for input in lcg_inputs() {
            let out = c.apply(&input).expect("no panic");
            // Property: bytes_saved <= input.len().
            assert!(out.bytes_saved <= input.len());
        }
    }

    #[test]
    fn output_respects_per_compactor_cap() {
        // ponytail: the bound check is the spec's "max_output_bytes".
        // The earlier `output_grows_monotonically_with_cap` test
        // asserted `a.summary.len() <= b.summary.len() + 1` — that
        // is a near-tautology because `b.summary` is always ≥
        // `a.summary` (bigger cap can only keep more lines). It
        // never pinned that the small-cap compactor stays under
        // its own cap. Strengthen to assert each compactor's
        // output respects ITS cap. The pre-check in `local_summarise`
        // is `if out.len() + line.len() + 1 > max_bytes { break }`,
        // so the loop refuses to add a line whose push would exceed
        // the cap — output is guaranteed `summary.len() <= max_bytes`,
        // not `<= max_bytes + 1`. The earlier +1 was defensive slack
        // for a constraint that doesn't exist.
        let small = LocalSummaryCompactor {
            max_output_bytes: 64,
        };
        let big = LocalSummaryCompactor {
            max_output_bytes: 64 * 1024,
        };
        for input in lcg_inputs().into_iter().filter(|s| s.len() > 1024) {
            let a = small.apply(&input).unwrap();
            let b = big.apply(&input).unwrap();
            // Each compactor must stay under its own cap exactly
            // (the +1 in the pre-check accounts for the trailing
            // newline we are about to push; if we pass the check, we
            // push `line.len() + 1` bytes, so the running total never
            // exceeds `max_bytes`).
            assert!(
                a.summary.len() <= 64,
                "small-cap summary must respect its 64-byte cap exactly; got {} bytes",
                a.summary.len()
            );
            assert!(
                b.summary.len() <= 64 * 1024,
                "big-cap summary must respect its 64 KiB cap exactly; got {} bytes",
                b.summary.len()
            );
            // Monotonicity as a corollary: bigger cap ⇒ ≥ output.
            assert!(
                b.summary.len() >= a.summary.len(),
                "larger cap must produce ≥ as much output as the smaller cap"
            );
        }
    }
}
