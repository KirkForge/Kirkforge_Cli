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

#[derive(Debug, Clone)]
pub struct CompactionResult {
    pub new_messages: Vec<Message>,
    pub dropped_tool_results: usize,
    pub condensed_assistant_turns: usize,
    pub original_count: usize,
    pub compacted_count: usize,
}

/// Marker text substituted for tool results in the middle region. Kept
/// in sync with the same marker used by `PromptBuilder::stub_old_tool_results`
/// (`session/prompt/mod.rs`) so the model sees consistent stub language
/// whether the trimming happened at compaction time or at request-build
/// time.
pub const TOOL_RESULT_STUB: &str =
    "[previous tool result omitted to save budget — see TUI history]";

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
pub const DEFAULT_PRESERVE_RECENT: usize = 8;

/// Naive compaction. Always succeeds; never panics.
///
/// `preserve_recent` is the number of trailing messages to keep
/// verbatim. The minimum effective value is 1 (always keep at least
/// the final message so the live turn isn't lost). When `messages` is
/// shorter than `preserve_recent + 1` the operation is a no-op.
///
/// Returns `original_count == compacted_count` only on the no-op
/// case (history shorter than the tail, so there's nothing in the
/// middle to compact). Every other invocation reduces
/// `compacted_count` below `original_count` and bumps at least one
/// of the work-counters.
pub fn compact(messages: &[Message], preserve_recent: usize) -> CompactionResult {
    let original_count = messages.len();
    let preserve_recent = preserve_recent.max(1);

    // Empty / trivial input — nothing to do.
    if messages.len() <= preserve_recent {
        return CompactionResult {
            new_messages: messages.to_vec(),
            dropped_tool_results: 0,
            condensed_assistant_turns: 0,
            original_count,
            compacted_count: messages.len(),
        };
    }

    // Anchor: a leading system message, if present.
    let anchor = if !messages.is_empty() && matches!(messages[0].role, Role::System) {
        1
    } else {
        0
    };

    // Tail: the last `preserve_recent` messages, verbatim.
    let working_set_start = messages.len() - preserve_recent;

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
                // Stub the content. Preserve tool_name + tool_call_id
                // so the TUI can still render a meaningful header
                // ("🔧 bash — [previous tool result omitted …]").
                let mut stub = msg.clone();
                stub.content = TOOL_RESULT_STUB.to_string();
                new_messages.push(stub);
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
                        "{}{}{}",
                        ASSISTANT_CONDENSED_PREFIX, original_chars, ASSISTANT_CONDENSED_SUFFIX,
                    );
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
    CompactionResult {
        new_messages,
        dropped_tool_results,
        condensed_assistant_turns,
        original_count,
        compacted_count,
    }
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
                user(&format!("q{}", i))
            } else {
                assistant(&format!("a{}", i))
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
            msgs.push(user(&format!("q{}", i)));
            msgs.push(assistant(&format!("a{}", i)));
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
            msgs.push(user(&format!("q{}", i)));
            msgs.push(assistant(&format!("a{}", i)));
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
            msgs.push(user(&format!("q{}", i)));
            msgs.push(assistant(&format!("a{}", i)));
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
            msgs.push(assistant(&format!("a{}", i)));
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
            msgs.push(user(&format!("q{}", i)));
            msgs.push(assistant(&format!("{} ", "x".repeat(2000)))); // 2k chars
            msgs.push(tool_result(&"y".repeat(5000), "c", "bash")); // 5k chars
        }
        let original_chars: usize = msgs.iter().map(|m| m.content.len()).sum();
        let r = compact(&msgs, DEFAULT_PRESERVE_RECENT);
        let compacted_chars: usize = r.new_messages.iter().map(|m| m.content.len()).sum();
        assert!(
            compacted_chars < original_chars,
            "compaction should reduce char count: {} -> {}",
            original_chars,
            compacted_chars
        );
    }
}
