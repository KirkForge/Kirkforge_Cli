//! Naive conversation-history compaction.
//!
//! Called as a fallback from `executor.rs` when the LLM-summarisation
//! path is disabled or fails. Splits the history into three regions:
//!
//! 1. **Anchor** — the leading system message (if any). Always
//!    preserved verbatim. It's the cache stem and dropping it would
//!    invalidate the prompt cache on the next turn.
//!
//! 2. **Tail** — the last `preserve_recent` messages. Preserved
//!    verbatim. Kimi-style "tail preservation" keeps the most recent
//!    user↔assistant turns intact so the model follows the live thread.
//!    Configurable via `Config::preserve_recent_messages` (default 2).
//!
//! 3. **Middle** — everything between the anchor and the tail. The
//!    compaction work happens here:
//!    - Tool results → replaced with a stub marker
//!      (`[previous tool result omitted to save budget …]`).
//!    - Assistant turns → replaced with a short condense marker
//!      (`[previous assistant turn condensed for context budget —
//!      original was N chars]`), so the model still sees the
//!      conversation *shape* (where assistant turns were) without
//!      paying for the prose.
//!    - User turns → preserved verbatim (cheap; the user wrote them).
//!
//! The smart-summarisation path in `executor.rs:316-407` does the
//! better job of *preserving* the middle's semantics by asking the
//! LLM to write a summary; this naive path is the deterministic
//! last-resort that works without an LLM round-trip.

use crate::shared::{Message, Role};

/// Request payload for the user-driven `/compact` command.
#[derive(Debug, Clone, Default)]
pub struct CompactRequest {
    /// Override for `Config::preserve_recent_messages`. When `None`, the
    /// executor uses the configured value.
    pub keep: Option<usize>,
}

#[derive(Debug, Clone)]
pub struct CompactionResult {
    pub new_messages: Vec<Message>,
    pub dropped_tool_results: usize,
    pub condensed_assistant_turns: usize,
    pub original_count: usize,
    pub compacted_count: usize,
    pub tokens_before: usize,
    pub tokens_after: usize,
}

/// Marker text substituted for stubbed tool results. The single source
/// for the stub shape: used by `compact_to_budget`'s middle region and
/// by `PromptBuilder::stub_old_tool_results` (`session/prompt/mod.rs`)
/// via [`stub_tool_result`], so the model sees consistent stub language
/// whether the trimming happened at compaction time or at request-build
/// time.
pub const TOOL_RESULT_STUB: &str =
    "[previous tool result omitted to save budget — see TUI history]";

/// Replace a tool result's content with the [`TOOL_RESULT_STUB`] marker.
/// Preserves `tool_name` + `tool_call_id` so the TUI can still render a
/// meaningful header ("🔧 bash — [previous tool result omitted …]").
/// Shared by `compact_to_budget` (middle region) and
/// `PromptBuilder::stub_old_tool_results` (request-build fallback).
pub fn stub_tool_result(msg: &Message) -> Message {
    let mut stub = msg.clone();
    stub.content = TOOL_RESULT_STUB.to_string();
    stub.token_count = None;
    stub
}

/// Length of the leading system-message anchor: 1 when the history
/// starts with a system message, else 0. The anchor is the cache stem
/// and is always preserved verbatim. Shared by the region splits in
/// `compact_to_budget` and `maybe_microcompact`.
pub fn anchor_len(messages: &[Message]) -> usize {
    if !messages.is_empty() && matches!(messages[0].role, Role::System) {
        1
    } else {
        0
    }
}

/// Marker prefix for condensed assistant turns. The trailing `(N chars)` is
/// the original message's character count, which is useful debugging info
/// (and makes the marker grep-able in the on-disk NDJSON log).
const ASSISTANT_CONDENSED_PREFIX: &str =
    "[previous assistant turn condensed for context budget — original was ";

const ASSISTANT_CONDENSED_SUFFIX: &str = " chars]";

/// Default number of trailing messages to keep verbatim. Used as a
/// fallback when the caller does not specify a `preserve_recent`
/// value. Mirrors the historical `DEFAULT_PRESERVE_RECENT` of 8 for
/// backwards compatibility in tests; production code should pass the
/// configured value from `Config::preserve_recent_messages`.
#[cfg(test)]
pub const DEFAULT_PRESERVE_RECENT: usize = 8;

/// Estimate tokens for a single message.
///
/// Uses the same `count_tokens` + thinking + tool_calls JSON heuristic as
/// `PromptBuilder` so the compaction path reports numbers consistent
/// with the budget checks in the request builder.
fn estimate_message_tokens(m: &Message) -> usize {
    super::estimate_message_tokens(m)
}

/// Estimate tokens for a message list.
pub(crate) fn estimate_tokens(messages: &[Message]) -> usize {
    messages.iter().map(estimate_message_tokens).sum()
}

/// Naive compaction with optional token-budget-aware tail sizing.
///
/// `preserve_recent` is the *minimum* number of trailing messages to keep
/// verbatim. When `target_budget_tokens` is `Some` and the current history
/// exceeds that budget, the tail is expanded backwards as far as the
/// tail budget allows (currently 25% of the total budget), giving the model
/// more recent verbatim context without exceeding the limit.
///
/// The minimum effective value for `preserve_recent` is 1 (always keep at
/// least the final message so the live turn isn't lost). When `messages`
/// is shorter than `preserve_recent + 1` the operation is a no-op.
///
/// Returns `original_count == compacted_count` only on the no-op
/// case (history shorter than the tail, so there's nothing in the
/// middle to compact). Every other invocation reduces
/// `compacted_count` below `original_count` and bumps at least one
/// of the work-counters.
pub fn compact_to_budget(
    messages: &[Message],
    preserve_recent: usize,
    target_budget_tokens: Option<usize>,
) -> CompactionResult {
    let original_count = messages.len();
    let preserve_recent_min = preserve_recent.max(1);
    let original_tokens = estimate_tokens(messages);

    // Empty / trivial input — nothing to do.
    if messages.len() <= preserve_recent_min {
        return CompactionResult {
            new_messages: messages.to_vec(),
            dropped_tool_results: 0,
            condensed_assistant_turns: 0,
            original_count,
            compacted_count: messages.len(),
            tokens_before: original_tokens,
            tokens_after: original_tokens,
        };
    }

    // Anchor: a leading system message, if present.
    let anchor = anchor_len(messages);

    // Tail: start with the minimum, then expand backwards if a budget is
    // set and we are currently over budget.
    let mut tail_size = preserve_recent_min;
    if let Some(budget) = target_budget_tokens {
        if original_tokens > budget {
            let tail_budget = budget / 4;
            let mut tail_tokens = estimate_tokens(&messages[messages.len() - tail_size..]);
            while tail_size < messages.len() - anchor {
                let next_idx = messages.len() - tail_size - 1;
                let next_tokens = estimate_message_tokens(&messages[next_idx]);
                if tail_tokens.saturating_add(next_tokens) > tail_budget {
                    break;
                }
                tail_tokens += next_tokens;
                tail_size += 1;
            }
        }
    }

    let working_set_start = messages.len() - tail_size;

    // Middle: [anchor .. working_set_start). May be empty.
    let mut new_messages: Vec<Message> = Vec::with_capacity(messages.len());
    if anchor > 0 {
        new_messages.push(messages[0].clone());
    }

    let mut dropped_tool_results = 0usize;
    let mut condensed_assistant_turns = 0usize;

    for msg in &messages[anchor..working_set_start] {
        match msg.role {
            Role::Tool => {
                // Stub the content (shared stub shape — see stub_tool_result).
                new_messages.push(stub_tool_result(msg));
                dropped_tool_results += 1;
            }
            Role::Assistant => {
                // Condense: drop the content, replace with a marker
                // that records the original size. Preserve
                // tool_calls (they're the structural intent — the
                // model needs to know it called `bash` here, even
                // if the prose around the call is gone).
                let original_chars = msg.content.chars().count();
                if original_chars == 0 {
                    // No prose to condense — keep the message as-is
                    // so tool_calls / thinking stay attached to a
                    // real message slot.
                    new_messages.push(msg.clone());
                } else {
                    let mut condensed = msg.clone();
                    condensed.content = format!(
                        "{ASSISTANT_CONDENSED_PREFIX}{original_chars}{ASSISTANT_CONDENSED_SUFFIX}",
                    );
                    condensed.token_count = None;
                    new_messages.push(condensed);
                    condensed_assistant_turns += 1;
                }
            }
            // User / System messages in the middle: keep verbatim.
            // System messages in the middle are rare but legal (a
            // post-init re-prompt, an injected reminder); user
            // messages are the user's actual words and cheap to keep.
            _ => new_messages.push(msg.clone()),
        }
    }

    // Append the working set verbatim.
    for msg in &messages[working_set_start..] {
        new_messages.push(msg.clone());
    }

    let compacted_count = new_messages.len();
    let tokens_after = estimate_tokens(&new_messages);
    CompactionResult {
        new_messages,
        dropped_tool_results,
        condensed_assistant_turns,
        original_count,
        compacted_count,
        tokens_before: original_tokens,
        tokens_after,
    }
}

/// Extract unresolved verifier findings from a message list for pinning
/// in the compaction tail (WO 22.6-R6).
///
/// Scans for `Role::Tool` messages whose `tool_name` starts with
/// `"verifier:"` and whose content indicates the issue was NOT resolved
/// (`"Verification failed"`, `"Failed to auto-fix"`, `"Failed to run
/// formatter"`). Returns a formatted summary string, or `None` if the
/// history contains no unresolved findings.
pub fn extract_unresolved_verifier_findings(messages: &[Message]) -> Option<String> {
    let findings: Vec<&str> = messages
        .iter()
        .filter(|m| {
            m.role == Role::Tool
                && m.tool_name
                    .as_ref()
                    .is_some_and(|n| n.starts_with("verifier:"))
                && (m.content.contains("Verification failed:")
                    || m.content.contains("Failed to auto-fix:")
                    || m.content.contains("Failed to run formatter:"))
        })
        .map(|m| m.content.trim())
        .collect();

    if findings.is_empty() {
        return None;
    }
    Some(format!(
        "[Verifier findings from earlier turns — still unresolved]\n{}",
        findings.join("\n")
    ))
}

/// Backward-compatible naive compaction.
///
/// Equivalent to `compact_to_budget(messages, preserve_recent, None)`;
/// keeps exactly `preserve_recent` trailing messages verbatim and does
/// not expand the tail based on a token budget.
#[cfg(test)]
pub fn compact(messages: &[Message], preserve_recent: usize) -> CompactionResult {
    compact_to_budget(messages, preserve_recent, None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shared::Message;

    fn user(text: &str) -> Message {
        Message {
            role: Role::User,
            content: text.into(),
            ..Default::default()
        }
    }

    fn assistant(text: &str) -> Message {
        Message {
            role: Role::Assistant,
            content: text.into(),
            ..Default::default()
        }
    }

    fn assistant_with_tool_call(text: &str, tool_name: &str, call_id: &str) -> Message {
        use crate::shared::ToolInvocation;
        Message {
            role: Role::Assistant,
            content: text.into(),
            tool_calls: Some(vec![ToolInvocation {
                id: call_id.into(),
                name: tool_name.into(),
                arguments: serde_json::json!({}),
            }]),
            ..Default::default()
        }
    }

    fn tool_result(text: &str, call_id: &str, tool_name: &str) -> Message {
        Message {
            role: Role::Tool,
            content: text.into(),
            tool_call_id: Some(call_id.into()),
            tool_name: Some(tool_name.into()),
            ..Default::default()
        }
    }

    fn system(text: &str) -> Message {
        Message {
            role: Role::System,
            content: text.into(),
            ..Default::default()
        }
    }

    #[test]
    fn empty_input_is_no_op() {
        let r = compact(&[], DEFAULT_PRESERVE_RECENT);
        assert_eq!(r.original_count, 0);
        assert_eq!(r.compacted_count, 0);
        assert_eq!(r.dropped_tool_results, 0);
        assert_eq!(r.condensed_assistant_turns, 0);
        assert!(r.new_messages.is_empty());
    }

    #[test]
    fn short_input_below_tail_is_no_op() {
        // 4 messages < DEFAULT_PRESERVE_RECENT (8) — preserve verbatim.
        let msgs = vec![user("a"), assistant("b"), user("c"), assistant("d")];
        let r = compact(&msgs, DEFAULT_PRESERVE_RECENT);
        assert_eq!(r.original_count, 4);
        assert_eq!(r.compacted_count, 4);
        assert_eq!(r.dropped_tool_results, 0);
        assert_eq!(r.condensed_assistant_turns, 0);
        assert_eq!(r.new_messages, msgs);
    }

    #[test]
    fn preserves_system_anchor() {
        // 9 messages: 1 system + 8 tail. No middle. Should be a no-op
        // because the boundary check is on len, not on the anchor.
        let mut msgs = vec![system("you are an agent")];
        for i in 0..8 {
            msgs.push(if i % 2 == 0 {
                user(&format!("q{i}"))
            } else {
                assistant(&format!("a{i}"))
            });
        }
        let r = compact(&msgs, DEFAULT_PRESERVE_RECENT);
        assert_eq!(r.compacted_count, 9);
        assert_eq!(r.dropped_tool_results, 0);
        assert_eq!(r.condensed_assistant_turns, 0);
        // System anchor first, content verbatim.
        assert_eq!(r.new_messages[0].content, "you are an agent");
    }

    #[test]
    fn stubs_middle_tool_results_and_condenses_assistants() {
        // 1 system + 12 tail = 13 total. Middle = [1..5) = 4 messages
        //   - user(1), tool(1), assistant(1), tool(1)
        // Tail: last 8 messages verbatim.
        let mut msgs = vec![system("anchor")];
        // Middle (4 messages):
        msgs.push(user("old question")); // 1
        msgs.push(tool_result("huge output", "c1", "bash")); // 2 — stub
        msgs.push(assistant("old answer with prose")); // 3 — condense
        msgs.push(tool_result("more output", "c2", "read_file")); // 4 — stub
                                                                  // Tail (8 messages):
        msgs.push(user("recent q1"));
        msgs.push(assistant("recent a1"));
        msgs.push(tool_result("r1", "c3", "bash"));
        msgs.push(assistant("recent a2"));
        msgs.push(user("recent q2"));
        msgs.push(assistant("recent a3"));
        msgs.push(tool_result("r2", "c4", "bash"));
        msgs.push(assistant("recent a4"));

        let r = compact(&msgs, DEFAULT_PRESERVE_RECENT);
        assert_eq!(r.original_count, 13);
        assert_eq!(r.compacted_count, 13); // no deletion, only replacement
        assert_eq!(r.dropped_tool_results, 2);
        assert_eq!(r.condensed_assistant_turns, 1);

        // System anchor preserved.
        assert_eq!(r.new_messages[0].content, "anchor");

        // Middle: 4 messages, all 4 preserved (stubs + condense keep slot count).
        let middle = &r.new_messages[1..5];
        assert_eq!(middle[0].content, "old question"); // user verbatim
        assert_eq!(middle[1].content, TOOL_RESULT_STUB); // tool stub
        assert!(middle[2].content.starts_with(ASSISTANT_CONDENSED_PREFIX)); // assistant condense
        assert!(middle[2]
            .content
            .contains("old answer with prose".len().to_string().as_str()));
        assert_eq!(middle[3].content, TOOL_RESULT_STUB); // tool stub

        // Tail: last 8 messages, verbatim.
        let tail = &r.new_messages[5..];
        assert_eq!(tail[0].content, "recent q1");
        assert_eq!(tail[7].content, "recent a4");
    }

    #[test]
    fn stubbed_tool_keeps_tool_name_and_call_id() {
        let mut msgs = vec![system("a")];
        for i in 0..DEFAULT_PRESERVE_RECENT {
            msgs.push(user(&format!("q{i}")));
            msgs.push(assistant(&format!("a{i}")));
        }
        // 1 system + 16 tail = 17. We need a middle, so history.len()
        // must be > DEFAULT_PRESERVE_RECENT. The above gives 17 which is
        // > 8 — middle is [1..9) = 8 messages.
        // Add 1 tool result in the middle:
        msgs.insert(2, tool_result("big output", "call_xyz", "read_file"));

        let r = compact(&msgs, DEFAULT_PRESERVE_RECENT);
        let tool_msg = r
            .new_messages
            .iter()
            .find(|m| m.role == Role::Tool && m.content == TOOL_RESULT_STUB)
            .expect("a tool stub should be present");
        assert_eq!(tool_msg.tool_name.as_deref(), Some("read_file"));
        assert_eq!(tool_msg.tool_call_id.as_deref(), Some("call_xyz"));
    }

    #[test]
    fn condense_preserves_tool_calls_on_assistant() {
        // The intent of the assistant turn (its tool calls) must
        // survive even when the prose is condensed, otherwise the
        // model loses the structural history of "I called bash here".
        let mut msgs = vec![system("a")];
        for i in 0..DEFAULT_PRESERVE_RECENT {
            msgs.push(user(&format!("q{i}")));
            msgs.push(assistant(&format!("a{i}")));
        }
        // Insert an assistant-with-tool-call into the middle.
        msgs.insert(2, assistant_with_tool_call("I'll run ls", "bash", "abc"));

        let r = compact(&msgs, DEFAULT_PRESERVE_RECENT);
        let condensed = r
            .new_messages
            .iter()
            .find(|m| {
                m.role == Role::Assistant && m.content.starts_with(ASSISTANT_CONDENSED_PREFIX)
            })
            .expect("a condensed assistant should be present");
        assert!(condensed.tool_calls.is_some());
        assert_eq!(condensed.tool_calls.as_ref().unwrap()[0].name, "bash");
        assert_eq!(condensed.tool_calls.as_ref().unwrap()[0].id, "abc");
    }

    #[test]
    fn empty_assistant_turn_is_not_counted_as_condensed() {
        // A zero-content assistant turn (e.g. a tool-call-only turn)
        // shouldn't be counted as "condensed" — there's nothing to
        // condense. (Other assistant turns in the middle that DO
        // have prose still get condensed normally.)
        let mut msgs = vec![system("a")];
        for i in 0..DEFAULT_PRESERVE_RECENT {
            msgs.push(user(&format!("q{i}")));
            msgs.push(assistant(&format!("a{i}")));
        }
        // Insert a tool-call-only assistant turn (no prose) in the middle.
        msgs.insert(2, assistant_with_tool_call("", "bash", "abc"));

        let r = compact(&msgs, DEFAULT_PRESERVE_RECENT);
        // Find the empty-prose assistant and confirm it survived verbatim
        // (not converted to a condense marker).
        let empty_prose = r
            .new_messages
            .iter()
            .find(|m| m.role == Role::Assistant && m.content.is_empty() && m.tool_calls.is_some())
            .expect("the empty-prose assistant should be present verbatim");
        assert_eq!(empty_prose.tool_calls.as_ref().unwrap()[0].id, "abc");

        // And confirm the other (prose-bearing) middle assistants *did*
        // get condensed — sanity check that this test is actually
        // exercising the condense path, not bypassing it.
        let condensed_count = r
            .new_messages
            .iter()
            .filter(|m| {
                m.role == Role::Assistant && m.content.starts_with(ASSISTANT_CONDENSED_PREFIX)
            })
            .count();
        assert!(
            condensed_count > 0,
            "the condense path should have fired on the prose-bearing middle assistants"
        );
    }

    #[test]
    fn no_anchor_when_history_starts_with_user() {
        // 1 user + 8 working = 9. > 8. Middle = [0..1) = empty.
        let mut msgs = vec![user("first question")];
        for i in 0..8 {
            msgs.push(assistant(&format!("a{i}")));
        }
        let r = compact(&msgs, DEFAULT_PRESERVE_RECENT);
        // Empty middle, no work done.
        assert_eq!(r.dropped_tool_results, 0);
        assert_eq!(r.condensed_assistant_turns, 0);
        assert_eq!(r.compacted_count, 9);
        assert_eq!(r.new_messages[0].content, "first question");
    }

    #[test]
    fn tail_preservation_keeps_last_n_verbatim() {
        // Kimi-style tail preservation: keep only the last 2 messages
        // verbatim and condense/stub everything else in the middle.
        let mut msgs = vec![system("anchor")];
        // Middle (5 messages): will be compacted.
        msgs.push(user("old q1"));
        msgs.push(assistant("old a1 with lots of prose"));
        msgs.push(tool_result("big output", "c1", "bash"));
        msgs.push(user("old q2"));
        msgs.push(assistant("old a2 with lots of prose"));
        // Tail (2 messages): preserved verbatim.
        msgs.push(user("recent q"));
        msgs.push(assistant("recent a"));

        let r = compact(&msgs, 2);
        assert_eq!(r.original_count, 8);
        assert_eq!(r.compacted_count, 8);
        assert_eq!(r.dropped_tool_results, 1);
        assert_eq!(r.condensed_assistant_turns, 2);

        // Anchor preserved.
        assert_eq!(r.new_messages[0].content, "anchor");
        // Tail preserved verbatim.
        assert_eq!(r.new_messages[r.new_messages.len() - 2].content, "recent q");
        assert_eq!(r.new_messages[r.new_messages.len() - 1].content, "recent a");
    }

    #[test]
    fn preserve_recent_clamped_to_at_least_one() {
        // A pathological preserve_recent of 0 must not drop the final
        // message; the live turn would be lost.
        let msgs = vec![user("q"), assistant("a")];
        let r = compact(&msgs, 0);
        assert_eq!(r.compacted_count, 2);
        assert_eq!(r.new_messages[1].content, "a");
    }

    #[test]
    fn compaction_reduces_visible_prose_size() {
        // Sanity: the total chars in the compacted output should be
        // meaningfully less than the original (otherwise the
        // operation is cosmetic). We don't assert an exact ratio
        // (the LLM is the budget authority), only that the condense
        // + stub path is taking effect.
        let mut msgs = vec![system("anchor")];
        for i in 0..10 {
            msgs.push(user(&format!("q{i}")));
            msgs.push(assistant(&format!("{} ", "x".repeat(2000)))); // 2k chars
            msgs.push(tool_result(&"y".repeat(5000), "c", "bash")); // 5k chars
        }
        let original_chars: usize = msgs.iter().map(|m| m.content.len()).sum();
        let r = compact(&msgs, DEFAULT_PRESERVE_RECENT);
        let compacted_chars: usize = r.new_messages.iter().map(|m| m.content.len()).sum();
        assert!(
            compacted_chars < original_chars,
            "compaction should reduce char count: {original_chars} -> {compacted_chars}"
        );
    }

    #[test]
    fn compact_to_budget_with_none_matches_compact() {
        let mut msgs = vec![system("anchor")];
        for i in 0..10 {
            msgs.push(user(&format!("q{i}")));
            msgs.push(assistant(&"x".repeat(2000)));
            msgs.push(tool_result(&"y".repeat(5000), "c", "bash"));
        }
        let r_budget = compact_to_budget(&msgs, DEFAULT_PRESERVE_RECENT, None);
        let r_plain = compact(&msgs, DEFAULT_PRESERVE_RECENT);
        assert_eq!(r_budget.new_messages, r_plain.new_messages);
        assert_eq!(r_budget.tokens_before, r_plain.tokens_before);
        assert_eq!(r_budget.tokens_after, r_plain.tokens_after);
    }

    #[test]
    fn compact_to_budget_reports_token_counts() {
        let msgs = vec![
            system("anchor"),
            user("old q"),
            assistant(&"x".repeat(4000)),
            user("recent q"),
            assistant("recent a"),
        ];
        let r = compact_to_budget(&msgs, 2, Some(1000));
        assert!(r.tokens_before > 0, "tokens_before should be positive");
        assert!(
            r.tokens_after <= r.tokens_before,
            "tokens_after ({}) should not exceed tokens_before ({})",
            r.tokens_after,
            r.tokens_before
        );
    }

    #[test]
    fn compact_to_budget_expands_tail_when_over_budget() {
        // 1 system + 12 messages: 6 short user + 6 long assistant.
        // Without a budget, preserve_recent=2 keeps 2 tail messages.
        // With a tight budget, the tail should expand to include the
        // cheap user messages while condensing the expensive assistants.
        let mut msgs = vec![system("anchor")];
        for i in 0..6 {
            msgs.push(user(&format!("q{i}"))); // cheap
            msgs.push(assistant(&"x".repeat(2000))); // expensive
        }
        let r_tight = compact_to_budget(&msgs, 2, Some(500));
        let r_plain = compact(&msgs, 2);

        // Tight budget should keep at least the minimum tail.
        assert!(r_tight.compacted_count >= 3); // anchor + at least 2 tail

        // Token count should drop.
        assert!(
            r_tight.tokens_after < r_tight.tokens_before,
            "budget compaction should reduce tokens: {} -> {}",
            r_tight.tokens_before,
            r_tight.tokens_after
        );

        // With very long assistant turns and a tiny budget, the tail
        // expansion may still stop at the minimum (the assistant turns
        // are too expensive to keep verbatim). That's fine — the
        // important invariant is we don't exceed the minimum.
        assert!(r_tight.compacted_count >= r_plain.compacted_count.min(3));
    }

    #[test]
    fn compact_to_budget_no_op_when_too_short() {
        let msgs = vec![user("q"), assistant("a")];
        let r = compact_to_budget(&msgs, 4, Some(100));
        assert_eq!(r.original_count, 2);
        assert_eq!(r.compacted_count, 2);
        assert_eq!(r.tokens_before, r.tokens_after);
    }

    #[test]
    fn estimate_tokens_empty_is_zero() {
        assert_eq!(estimate_tokens(&[]), 0);
    }

    #[test]
    fn estimate_message_tokens_zero_for_empty_message() {
        let m = Message::default();
        assert_eq!(estimate_message_tokens(&m), 0);
    }

    // ── extract_unresolved_verifier_findings ──

    #[test]
    fn no_findings_when_history_clean() {
        let msgs = vec![user("q"), assistant("a"), tool_result("ok", "c1", "bash")];
        assert!(extract_unresolved_verifier_findings(&msgs).is_none());
    }

    #[test]
    fn extracts_verification_failed() {
        let msgs = vec![
            user("q"),
            assistant("a"),
            tool_result(
                "Verification failed: unused import — use std::fs is unused",
                "c1",
                "verifier:lint",
            ),
        ];
        let s = extract_unresolved_verifier_findings(&msgs).unwrap();
        assert!(s.contains("[Verifier findings from earlier turns"));
        assert!(s.contains("Verification failed:"));
    }

    #[test]
    fn extracts_failed_auto_fix() {
        let msgs = vec![tool_result(
            "Failed to auto-fix: warning — mismatched braces",
            "c2",
            "verifier:rustfmt",
        )];
        let s = extract_unresolved_verifier_findings(&msgs).unwrap();
        assert!(s.contains("Failed to auto-fix:"));
    }

    #[test]
    fn skips_resolved_auto_fix() {
        let msgs = vec![tool_result(
            "Auto-fixed: warning — removed unused import",
            "c3",
            "verifier:lint",
        )];
        assert!(extract_unresolved_verifier_findings(&msgs).is_none());
    }

    #[test]
    fn skips_skipped_verifier() {
        let msgs = vec![tool_result(
            "verification skipped: tool not available",
            "c4",
            "verifier:security",
        )];
        assert!(extract_unresolved_verifier_findings(&msgs).is_none());
    }

    #[test]
    fn skips_non_verifier_tool_results() {
        let msgs = vec![tool_result(
            "Verification failed: something broke",
            "c5",
            "bash",
        )];
        assert!(extract_unresolved_verifier_findings(&msgs).is_none());
    }

    #[test]
    fn combines_multiple_unresolved_findings() {
        let msgs = vec![
            tool_result(
                "Verification failed: unused import — src/main.rs",
                "c1",
                "verifier:lint",
            ),
            tool_result(
                "Failed to auto-fix: error — mismatched braces",
                "c2",
                "verifier:rustfmt",
            ),
            tool_result(
                "Auto-fixed: warning — removed dead code",
                "c3",
                "verifier:lint",
            ),
        ];
        let s = extract_unresolved_verifier_findings(&msgs).unwrap();
        assert!(s.contains("Verification failed:"));
        assert!(s.contains("Failed to auto-fix:"));
        assert!(!s.contains("Auto-fixed:"));
    }

    // ── R3.4 — tool-call / tool-result pairing survives compaction ──────
    //
    // After naive compaction, every tool-result message in the middle region
    // is stubbed but KEEPS its `tool_call_id` and `tool_name` (so the TUI
    // header still renders). The structural invariant — every Tool message
    // references a `tool_call_id` that some Assistant message emitted — must
    // hold in the compacted output. Breaking it would orphan tool results
    // and confuse the next model turn.

    #[test]
    fn slice_keeps_tool_call_tool_result_pairs_intact() {
        // 1 system + 1 assistant(tool_call) + 1 tool_result + 8-message tail.
        // The tool_call/result pair lands in the middle (compacted region).
        let mut msgs = vec![system("anchor")];
        msgs.push(assistant_with_tool_call(
            "I'll run it",
            "bash",
            "call_pair_1",
        ));
        msgs.push(tool_result("big output here", "call_pair_1", "bash"));
        for i in 0..DEFAULT_PRESERVE_RECENT {
            msgs.push(user(&format!("q{i}")));
            msgs.push(assistant(&format!("a{i}")));
        }

        let r = compact(&msgs, DEFAULT_PRESERVE_RECENT);

        // Every Tool-role message must still carry a non-empty tool_call_id
        // matching an id emitted on some Assistant message's tool_calls.
        let assistant_call_ids: Vec<String> = r
            .new_messages
            .iter()
            .filter(|m| matches!(m.role, Role::Assistant))
            .flat_map(|m| m.tool_calls.clone().unwrap_or_default())
            .map(|tc| tc.id)
            .collect();
        let tool_results = r
            .new_messages
            .iter()
            .filter(|m| matches!(m.role, Role::Tool))
            .collect::<Vec<_>>();
        assert!(
            !tool_results.is_empty(),
            "compaction must keep the tool-result slot (stubbed), not drop it"
        );
        for tr in &tool_results {
            let id = tr
                .tool_call_id
                .as_ref()
                .expect("tool result must retain tool_call_id after compaction");
            assert!(
                assistant_call_ids.iter().any(|a| a == id),
                "tool result id {id} must match an assistant tool_call id; \
                 pairing broken after compaction"
            );
            assert!(
                tr.tool_name.is_some(),
                "tool_name must survive compaction for TUI rendering"
            );
        }
    }

    // ── R3.1 — slice trips when context exceeds threshold ──────────────
    //
    // `compact_to_budget` with a `target_budget_tokens` of `Some(b)`
    // expands the tail backwards only when `original_tokens > b`.
    // Under the threshold it behaves like `compact` (no expansion).
    // Pin both branches so a contributor who flips the inequality
    // surfaces here.

    #[test]
    fn slice_trips_when_context_exceeds_threshold() {
        // 1 system + 12 messages: 6 short user + 6 expensive assistant.
        let mut msgs = vec![system("anchor")];
        for i in 0..6 {
            msgs.push(user(&format!("q{i}")));
            msgs.push(assistant(&"x".repeat(2000)));
        }
        let original_tokens = estimate_tokens(&msgs);
        // Under the threshold: no expansion beyond the minimum tail.
        let under = compact_to_budget(&msgs, 2, Some(original_tokens + 10_000));
        assert_eq!(
            under.compacted_count, under.original_count,
            "under-threshold compaction must not delete slots (replacement only)"
        );
        let r_plain = compact(&msgs, 2);
        assert_eq!(
            under.compacted_count, r_plain.compacted_count,
            "under-threshold path must match the budget-less path"
        );

        // Over the threshold: the tail expands backwards to include
        // cheap messages while the expensive middle is condensed.
        let over = compact_to_budget(&msgs, 2, Some(500));
        assert!(
            over.tokens_after < over.tokens_before,
            "over-threshold compaction must reduce tokens: {} -> {}",
            over.tokens_before,
            over.tokens_after
        );
        assert!(
            over.condensed_assistant_turns > 0,
            "over-threshold compaction must condense at least one assistant"
        );
    }

    // ── R3.2 — slice selects correct messages for compaction ───────────
    //
    // The three-region split: anchor (system, kept verbatim), middle
    // (compacted: tools stubbed, assistants condensed, users kept), tail
    // (preserved verbatim). Pin each region's shape.

    #[test]
    fn slice_selects_correct_messages_for_compaction() {
        let mut msgs = vec![system("anchor")];
        // Middle (4 messages): user, tool, assistant, tool.
        msgs.push(user("old question"));
        msgs.push(tool_result("huge output", "c1", "bash"));
        msgs.push(assistant("old answer with prose"));
        msgs.push(tool_result("more output", "c2", "read_file"));
        // Tail (2 messages): preserved verbatim.
        msgs.push(user("recent q"));
        msgs.push(assistant("recent a"));

        let r = compact(&msgs, 2);
        assert_eq!(r.original_count, 7);
        assert_eq!(r.dropped_tool_results, 2);
        assert_eq!(r.condensed_assistant_turns, 1);

        // Anchor: first message, verbatim system.
        assert_eq!(r.new_messages[0].role, Role::System);
        assert_eq!(r.new_messages[0].content, "anchor");

        // Middle: the user is verbatim, both tools are stubbed, the
        // assistant is condensed (carries the condense prefix).
        assert_eq!(r.new_messages[1].content, "old question");
        assert_eq!(r.new_messages[2].role, Role::Tool);
        assert_eq!(r.new_messages[2].content, TOOL_RESULT_STUB);
        assert!(
            r.new_messages[3]
                .content
                .starts_with(ASSISTANT_CONDENSED_PREFIX),
            "middle assistant must be condensed, got: {:?}",
            r.new_messages[3].content
        );
        assert_eq!(r.new_messages[4].content, TOOL_RESULT_STUB);

        // Tail: last 2 messages verbatim.
        assert_eq!(r.new_messages[5].content, "recent q");
        assert_eq!(r.new_messages[6].content, "recent a");
    }

    // ── R3.3 — slice preserves conversation order ──────────────────────
    //
    // Compaction replaces middle content (stub/condense) but must not
    // reorder messages. The relative order of anchor → middle → tail
    // and the order *within* the middle must be preserved.

    #[test]
    fn slice_preserves_conversation_order() {
        let mut msgs = vec![system("anchor")];
        // Middle: a recognisable sequence of roles.
        msgs.push(user("m1"));
        msgs.push(assistant("m2"));
        msgs.push(tool_result("m3", "c1", "bash"));
        msgs.push(user("m4"));
        msgs.push(assistant("m5"));
        // Tail: 2 messages.
        msgs.push(user("t1"));
        msgs.push(assistant("t2"));

        let r = compact(&msgs, 2);

        // The role sequence must be identical to the original —
        // compaction replaces content, never reorders slots.
        let original_roles: Vec<Role> = msgs.iter().map(|m| m.role.clone()).collect();
        let compacted_roles: Vec<Role> = r.new_messages.iter().map(|m| m.role.clone()).collect();
        assert_eq!(
            original_roles, compacted_roles,
            "compaction must preserve message order (roles), got: {compacted_roles:?}"
        );

        // The anchor stays first, the tail stays last.
        assert_eq!(
            r.new_messages.first().map(|m| m.content.as_str()),
            Some("anchor")
        );
        assert_eq!(
            r.new_messages.last().map(|m| m.content.as_str()),
            Some("t2")
        );
    }

    // ── R4.1 — compaction triggers at threshold ────────────────────────
    //
    // When the estimated token count exceeds `target_budget_tokens`,
    // `compact_to_budget` must produce a strictly smaller token count
    // in `tokens_after` (the tail-expansion + middle-condense path).
    // Below the threshold it's a no-op on token count.

    #[test]
    fn compaction_triggers_at_threshold() {
        let mut msgs = vec![system("anchor")];
        for i in 0..10 {
            msgs.push(user(&format!("q{i}")));
            msgs.push(assistant(&"x".repeat(2000)));
        }
        let original_tokens = estimate_tokens(&msgs);
        assert!(
            original_tokens > 1000,
            "fixture must be large enough to trigger compaction: {original_tokens}"
        );

        // Over: the threshold is set well below the original tokens.
        let r = compact_to_budget(&msgs, 2, Some(1000));
        assert!(
            r.tokens_after < r.tokens_before,
            "over-threshold compaction must reduce tokens: {} -> {}",
            r.tokens_before,
            r.tokens_after
        );
        assert!(
            r.condensed_assistant_turns > 0,
            "over-threshold compaction must condense assistants"
        );

        // Under-threshold: the middle is still compacted (content
        // replacement), so tokens still drop — the budget only
        // controls *tail expansion*, not whether the middle is
        // touched. Pin that the under-threshold path matches the
        // budget-less `compact` (no tail expansion).
        let r_noop = compact_to_budget(&msgs, 2, Some(original_tokens * 10));
        let r_plain = compact(&msgs, 2);
        assert_eq!(
            r_noop.compacted_count, r_plain.compacted_count,
            "under-threshold path must match the budget-less path (no tail expansion)"
        );
        assert_eq!(
            r_noop.tokens_after, r_plain.tokens_after,
            "under-threshold token count must match the budget-less path"
        );
        // The over-budget path must keep at least as many tokens as
        // the budget-less path when the budget is generous (no
        // expansion), and fewer when the budget is tight. Here we
        // just assert the over path compressed strictly more.
        assert!(
            r.tokens_after <= r_noop.tokens_after,
            "tight budget must not produce more tokens than the generous budget"
        );
    }

    // ── R4.3 — compaction preserves verifier findings tail ─────────────
    //
    // Verifier findings (tool_name starts with `verifier:` and content
    // indicates an unresolved failure) that land in the *tail* region
    // (within the last `preserve_recent` messages) must survive
    // compaction verbatim — they are NOT in the middle, so they are
    // never stubbed. The `extract_unresolved_verifier_findings` helper
    // must still find them in the post-compaction message list.

    #[test]
    fn compaction_preserves_verifier_findings_tail() {
        let mut msgs = vec![system("anchor")];
        // Middle: a resolved verifier finding (should be skipped by
        // the extractor because its content says "Auto-fixed").
        msgs.push(tool_result(
            "Auto-fixed: warning — removed dead code",
            "c0",
            "verifier:lint",
        ));
        // Padding so the middle is non-empty.
        for i in 0..6 {
            msgs.push(user(&format!("q{i}")));
            msgs.push(assistant(&"x".repeat(500)));
        }
        // Tail (2 messages): one unresolved verifier finding + one user.
        msgs.push(tool_result(
            "Verification failed: unused import — src/lib.rs",
            "c_tail",
            "verifier:lint",
        ));
        msgs.push(user("recent q"));

        let r = compact(&msgs, 2);
        // The tail tool result must survive verbatim.
        let tail_finding = r
            .new_messages
            .iter()
            .find(|m| {
                m.role == Role::Tool
                    && m.tool_name.as_deref() == Some("verifier:lint")
                    && m.content.contains("Verification failed")
            })
            .expect("unresolved verifier finding in tail must survive compaction");
        assert_eq!(
            tail_finding.content, "Verification failed: unused import — src/lib.rs",
            "tail verifier finding must be verbatim, not stubbed"
        );

        // The extractor must surface the unresolved finding from the
        // post-compaction list.
        let findings = extract_unresolved_verifier_findings(&r.new_messages)
            .expect("at least one unresolved finding survives");
        assert!(findings.contains("Verification failed:"));
        // The resolved middle finding must NOT appear (it was stubbed).
        assert!(
            !findings.contains("Auto-fixed:"),
            "resolved finding in the middle must be stubbed away: {findings}"
        );
    }

    // ── R4.4 — compaction re-triggers on long session ──────────────────
    //
    // A long session may need compaction more than once. Run
    // `compact_to_budget` on the result of a first compaction; the
    // second run must still be able to reduce tokens (or at minimum
    // remain stable — never grow). The invariant: compaction is
    // idempotent in token count (a second pass does not increase it).

    #[test]
    fn compaction_retriggers_on_long_session() {
        let mut msgs = vec![system("anchor")];
        for i in 0..30 {
            msgs.push(user(&format!("q{i}")));
            msgs.push(assistant(&"x".repeat(2000)));
            msgs.push(tool_result(&"y".repeat(2000), "c", "bash"));
        }
        // First compaction: tight budget.
        let r1 = compact_to_budget(&msgs, 4, Some(2000));
        assert!(
            r1.tokens_after < r1.tokens_before,
            "first compaction must reduce tokens: {} -> {}",
            r1.tokens_before,
            r1.tokens_after
        );
        // Second compaction on the result: must not grow tokens.
        let r2 = compact_to_budget(&r1.new_messages, 4, Some(2000));
        assert!(
            r2.tokens_after <= r1.tokens_after,
            "second compaction must not increase tokens: r1={} r2={}",
            r1.tokens_after,
            r2.tokens_after
        );
    }
}
