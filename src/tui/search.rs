//! Conversation search — Ctrl+F in the TUI input box.
//!
//! Review.md gap #4: previously the user had no way to find a word
//! in a long chat history other than scrolling. For a session with
//! 50+ turns that's a 30-second scroll. This module powers the
//! `Ctrl+F` overlay and the `n` / `Shift+N` match cycling.
//!
//! # Design
//!
//! `compute_matches` is a pure function: it takes a query string and
//! the conversation (a slice of `ConversationEntry`) and returns a
//! list of `(message_index, byte_offset)` tuples. The TUI calls it
//! when the user commits a search (Enter in search mode) and stores
//! the result on `AppState`.
//!
//! `navigate_next` / `navigate_prev` wrap-around the match index
//! for the `n` / `N` keys. They're pure too — given the current
//! index and a length, return the new index.
//!
//! Search is case-insensitive substring. We don't try to be smart
//! about word boundaries or regex — the user typing "fn " is a
//! good enough way to find `fn` definitions. Tool entries
//! (`role == "tool"`) are searched in their `content` field but
//! NOT in `tool_output` (the sidecar) — that would surface matches
//! the user can't see in the chat (since tool entries are
//! collapsed by default).

use crate::tui::app::ConversationEntry;

/// One match location: which message in `state.messages` and the
/// byte offset of the match's first character within that message's
/// `content` field.
pub type MatchPos = (usize, usize);

/// Find all case-insensitive substring matches of `query` in the
/// conversation. Returns matches in document order (message index
/// first, then offset within that message). An empty query returns
/// no matches (clearing prior results is the caller's job).
pub fn compute_matches(messages: &[ConversationEntry], query: &str) -> Vec<MatchPos> {
    let q = query.trim();
    if q.is_empty() {
        return Vec::new();
    }
    let needle = q.to_lowercase();
    let mut out = Vec::new();
    for (i, entry) in messages.iter().enumerate() {
        // Skip tool entries' sidecar output — see module docs.
        let haystack = entry.content.to_lowercase();
        let mut start = 0;
        while let Some(rel) = haystack[start..].find(&needle) {
            out.push((i, start + rel));
            start += rel + needle.len();
            // Defensive: if the needle is the whole rest of the
            // string, we still need to advance to avoid an infinite
            // loop on empty haystack or zero-width matches.
            if start >= haystack.len() {
                break;
            }
        }
    }
    out
}

/// Return the next match index, wrapping around at the end.
/// Returns `None` if `matches_len == 0` (caller should not call).
pub fn navigate_next(current: usize, matches_len: usize) -> usize {
    if matches_len == 0 {
        return 0;
    }
    (current + 1) % matches_len
}

/// Return the previous match index, wrapping around at 0.
pub fn navigate_prev(current: usize, matches_len: usize) -> usize {
    if matches_len == 0 {
        return 0;
    }
    if current == 0 {
        matches_len - 1
    } else {
        current - 1
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::app::ConversationEntry;

    fn user(text: &str) -> ConversationEntry {
        ConversationEntry::new("user", text)
    }
    fn asst(text: &str) -> ConversationEntry {
        ConversationEntry::new("assistant", text)
    }

    /// Empty query returns no matches.
    #[test]
    fn test_empty_query_no_matches() {
        let msgs = vec![user("hello world")];
        assert!(compute_matches(&msgs, "").is_empty());
        assert!(compute_matches(&msgs, "   ").is_empty());
    }

    /// Single occurrence in a single message.
    #[test]
    fn test_single_match() {
        let msgs = vec![user("hello world")];
        let m = compute_matches(&msgs, "world");
        assert_eq!(m, vec![(0, 6)]);
    }

    /// Case-insensitive: "WORLD" matches "world".
    #[test]
    fn test_case_insensitive() {
        let msgs = vec![user("hello WORLD")];
        let m = compute_matches(&msgs, "world");
        assert_eq!(m, vec![(0, 6)]);
    }

    /// Multiple matches within a single message advance past each.
    #[test]
    fn test_multiple_within_message() {
        let msgs = vec![user("foo bar foo bar foo")];
        let m = compute_matches(&msgs, "foo");
        assert_eq!(m, vec![(0, 0), (0, 8), (0, 16)]);
    }

    /// Matches across multiple messages, in document order.
    #[test]
    fn test_matches_across_messages() {
        let msgs = vec![user("hello"), asst("hello to you")];
        let m = compute_matches(&msgs, "hello");
        assert_eq!(m, vec![(0, 0), (1, 0)]);
    }

    /// No match returns an empty list.
    #[test]
    fn test_no_match() {
        let msgs = vec![user("hello world")];
        assert!(compute_matches(&msgs, "xyzzy").is_empty());
    }

    /// Unicode is preserved: matching "café" finds it in the haystack
    /// at a byte offset that's correct for the byte position, even
    /// though that's mid-codepoint. The renderer doesn't slice on
    /// the byte offset — it scans the haystack string. We just
    /// return the position; consumption is by content scan.
    #[test]
    fn test_unicode_match() {
        let msgs = vec![user("I love café food")];
        let m = compute_matches(&msgs, "café");
        // First char is "I" (1 byte) + space (1) + "love" (4) +
        // space (1) = 7. café is 5 bytes (c=1, a=1, f=1, é=2).
        assert_eq!(m, vec![(0, 7)]);
    }

    /// `navigate_next` wraps around at the end.
    #[test]
    fn test_navigate_next_wraps() {
        assert_eq!(navigate_next(0, 3), 1);
        assert_eq!(navigate_next(1, 3), 2);
        assert_eq!(navigate_next(2, 3), 0);
    }

    /// `navigate_next` on an empty list returns 0.
    #[test]
    fn test_navigate_next_empty() {
        assert_eq!(navigate_next(0, 0), 0);
        assert_eq!(navigate_next(5, 0), 0);
    }

    /// `navigate_prev` wraps around at 0.
    #[test]
    fn test_navigate_prev_wraps() {
        assert_eq!(navigate_prev(0, 3), 2);
        assert_eq!(navigate_prev(1, 3), 0);
        assert_eq!(navigate_prev(2, 3), 1);
    }

    /// `navigate_prev` on an empty list returns 0.
    #[test]
    fn test_navigate_prev_empty() {
        assert_eq!(navigate_prev(0, 0), 0);
        assert_eq!(navigate_prev(5, 0), 0);
    }
}
