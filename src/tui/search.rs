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
//! list of match locations. The TUI calls it when the user commits
//! a search (Enter in search mode) and stores the result on
//! `AppState`.
//!
//! `navigate_next` / `navigate_prev` wrap-around the match index
//! for the `n` / `N` keys. They're pure too — given the current
//! index and a length, return the new index.
//!
//! Search is case-insensitive substring. We don't try to be smart
//! about word boundaries or regex — the user typing "fn " is a
//! good enough way to find `fn` definitions. Tool entries
//! (`role == "tool"`) are searched in both their `content` (summary)
//! and, when present, their `tool_output` (full body). Matches in
//! `tool_output` are tagged so a future renderer can decide whether
//! to auto-expand the tool entry when jumping to that match.

use crate::tui::app::ConversationEntry;

/// Which text source within a conversation entry a match came from.
/// A future chat renderer can use this to expand a collapsed tool
/// entry when the user jumps to a match that lives in `tool_output`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchSource {
    /// Match found in the primary `content` field (message text or
    /// tool summary).
    Content,
    /// Match found in the sidecar `tool_output` field (full tool
    /// result).
    ToolOutput,
}

/// One match location: which message in `state.messages`, the byte
/// offset of the match's first character within that source, and
/// which source the match came from.
pub type MatchPos = (usize, usize, SearchSource);

/// Append all case-insensitive substring matches of `needle` in
/// `haystack` to `out`, tagging them with `source`.
///
/// Byte offsets in the returned `MatchPos` are into the **original**
/// `haystack`, not into a lowercased copy. This matters for Unicode
/// text where `to_lowercase()` can change byte length (e.g. `İ` →
/// `i̇`). We build a case-folded copy of `haystack` together with a
/// per-byte mapping back to the original byte index, then translate
/// matches in the folded string into original-string positions.
fn find_matches(
    out: &mut Vec<MatchPos>,
    message_index: usize,
    haystack: &str,
    needle: &str,
    source: SearchSource,
) {
    if needle.is_empty() {
        return;
    }
    let (haystack_folded, mapping) = case_fold_with_mapping(haystack);
    let mut start = 0;
    while let Some(rel) = haystack_folded[start..].find(needle) {
        let folded_offset = start + rel;
        let original_offset = mapping[folded_offset];
        out.push((message_index, original_offset, source));
        start += rel + needle.len();
        // Defensive: if the needle is the whole rest of the
        // string, we still need to advance to avoid an infinite
        // loop on empty haystack or zero-width matches.
        if start >= haystack_folded.len() {
            break;
        }
    }
}

/// Build a case-folded copy of `s` and a byte-offset mapping.
///
/// `mapping[i]` is the byte offset in the original string of the
/// character that produced the folded byte at index `i`. Multi-
/// character or multi-byte lowercase expansions (e.g. `İ` → `i` +
/// combining dot, 2 bytes) map every produced byte back to the same
/// source byte.
pub(crate) fn case_fold_with_mapping(s: &str) -> (String, Vec<usize>) {
    let mut folded = String::with_capacity(s.len());
    let mut mapping = Vec::with_capacity(s.len());
    for (byte_idx, c) in s.char_indices() {
        let start = folded.len();
        for lc in c.to_lowercase() {
            folded.push(lc);
        }
        // Push one original-byte offset for every UTF-8 byte the
        // lowercased char(s) contributed, so `mapping` stays aligned
        // with `folded` as a byte string.
        for _ in start..folded.len() {
            mapping.push(byte_idx);
        }
    }
    (folded, mapping)
}

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
        find_matches(&mut out, i, &entry.content, &needle, SearchSource::Content);
        if let Some(ref tool_output) = entry.tool_output {
            find_matches(&mut out, i, tool_output, &needle, SearchSource::ToolOutput);
        }
    }
    out
}

/// Return the next match index, wrapping around at the end.
/// Returns `None` when there are no matches, so callers cannot
/// accidentally index an empty `search_matches` vector.
pub fn navigate_next(current: usize, matches_len: usize) -> Option<usize> {
    if matches_len == 0 {
        return None;
    }
    Some((current + 1) % matches_len)
}

/// Return the previous match index, wrapping around at 0.
pub fn navigate_prev(current: usize, matches_len: usize) -> Option<usize> {
    if matches_len == 0 {
        return None;
    }
    Some(if current == 0 {
        matches_len - 1
    } else {
        current - 1
    })
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
    fn tool(summary: &str, full: &str) -> ConversationEntry {
        ConversationEntry::tool(summary, full)
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
        assert_eq!(m, vec![(0, 6, SearchSource::Content)]);
    }

    /// Case-insensitive: "WORLD" matches "world".
    #[test]
    fn test_case_insensitive() {
        let msgs = vec![user("hello WORLD")];
        let m = compute_matches(&msgs, "world");
        assert_eq!(m, vec![(0, 6, SearchSource::Content)]);
    }

    /// Multiple matches within a single message advance past each.
    #[test]
    fn test_multiple_within_message() {
        let msgs = vec![user("foo bar foo bar foo")];
        let m = compute_matches(&msgs, "foo");
        assert_eq!(
            m,
            vec![
                (0, 0, SearchSource::Content),
                (0, 8, SearchSource::Content),
                (0, 16, SearchSource::Content)
            ]
        );
    }

    /// Matches across multiple messages, in document order.
    #[test]
    fn test_matches_across_messages() {
        let msgs = vec![user("hello"), asst("hello to you")];
        let m = compute_matches(&msgs, "hello");
        assert_eq!(
            m,
            vec![(0, 0, SearchSource::Content), (1, 0, SearchSource::Content)]
        );
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
        assert_eq!(m, vec![(0, 7, SearchSource::Content)]);
    }

    /// Regression: case-folding a Unicode character can change its
    /// byte length (e.g. `İ` → `i̇`). The returned byte offset must be
    /// into the original string, not into a lowercased copy.
    #[test]
    fn test_unicode_case_folding_byte_offset() {
        let msgs = vec![user("İstanbul")];
        let m = compute_matches(&msgs, "stan");
        // Original string: İ (2 bytes) + stanbul (7 bytes).
        // The match starts right after İ, at byte offset 2.
        // A naive `to_lowercase()` search would report the offset in
        // the folded string (3), which points inside the multi-byte
        // İ in the original.
        assert_eq!(m, vec![(0, 2, SearchSource::Content)]);
    }

    #[test]
    fn test_tool_output_searched() {
        let msgs = vec![tool("summary line", "full output line")];
        let m = compute_matches(&msgs, "line");
        assert_eq!(
            m,
            vec![
                (0, 8, SearchSource::Content),
                (0, 12, SearchSource::ToolOutput)
            ]
        );
    }

    /// A match only in the tool output is still returned.
    #[test]
    fn test_match_only_in_tool_output() {
        let msgs = vec![tool("summary", "the secret value is here")];
        let m = compute_matches(&msgs, "secret");
        assert_eq!(m, vec![(0, 4, SearchSource::ToolOutput)]);
    }

    /// `navigate_next` wraps around at the end.
    #[test]
    fn test_navigate_next_wraps() {
        assert_eq!(navigate_next(0, 3), Some(1));
        assert_eq!(navigate_next(1, 3), Some(2));
        assert_eq!(navigate_next(2, 3), Some(0));
    }

    /// `navigate_next` on an empty list returns `None`.
    #[test]
    fn test_navigate_next_empty() {
        assert_eq!(navigate_next(0, 0), None);
        assert_eq!(navigate_next(5, 0), None);
    }

    /// `navigate_prev` wraps around at 0.
    #[test]
    fn test_navigate_prev_wraps() {
        assert_eq!(navigate_prev(0, 3), Some(2));
        assert_eq!(navigate_prev(1, 3), Some(0));
        assert_eq!(navigate_prev(2, 3), Some(1));
    }

    /// `navigate_prev` on an empty list returns `None`.
    #[test]
    fn test_navigate_prev_empty() {
        assert_eq!(navigate_prev(0, 0), None);
        assert_eq!(navigate_prev(5, 0), None);
    }
}
