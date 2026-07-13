//! Pure text-navigation and search-direction helpers for the input-mode
//! key handler. Extracted from the parent module so the big
//! `handle_input_key` state machine stays focused on key routing.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// Delete the word (or whitespace run) immediately before `cursor_byte`.
///
/// Returns the updated input string and the new char-index cursor position.
///
/// Behaviour mirrors readline-style `backward-kill-word`:
/// - If the cursor is preceded by whitespace, the whitespace run is deleted.
/// - If the cursor is preceded by a word, the word is deleted, and any
///   whitespace separating it from a previous word is deleted too.
/// - Leading whitespace before the first word is preserved (so "   hello|"
///   becomes "   |", not "|").
pub(super) fn delete_word_backward(input: &str, cursor_byte: usize) -> (String, usize) {
    let cur = cursor_byte.min(input.len());
    let before = &input[..cur];

    if before.is_empty() {
        return (input.to_string(), 0);
    }

    let ends_with_ws = before.chars().last().is_some_and(|c| c.is_whitespace());

    let new_byte = if ends_with_ws {
        // Cursor is in a trailing whitespace run: kill back to the previous
        // non-whitespace character (or the start of the line).
        before
            .rfind(|c: char| !c.is_whitespace())
            .map(|pos| {
                // `rfind` returned a char boundary; the char must exist.
                // We defensively fall back to `pos` rather than panic on
                // an empty slice (which cannot happen for valid UTF-8).
                let Some(ch) = before[pos..].chars().next() else {
                    return pos;
                };
                pos + ch.len_utf8()
            })
            .unwrap_or(0)
    } else {
        // Cursor is at the end of a word. Find the word's start, then decide
        // whether to also kill the preceding whitespace run.
        match before.rfind(|c: char| c.is_whitespace()) {
            Some(pos) => {
                // `rfind` returned a char boundary; fall back to `pos`
                // if the slice is somehow empty.
                let Some(ch) = before[pos..].chars().next() else {
                    return (input[..pos].to_string(), input[..pos].chars().count());
                };
                let word_start = pos + ch.len_utf8();
                let has_prev_word = before[..word_start].chars().any(|c| !c.is_whitespace());
                if has_prev_word {
                    before[..word_start]
                        .rfind(|c: char| !c.is_whitespace())
                        .map(|prev_pos| {
                            // `rfind` returned a char boundary; fall back to
                            // `prev_pos` if the slice is somehow empty.
                            let Some(prev_ch) = before[prev_pos..].chars().next() else {
                                return prev_pos;
                            };
                            prev_pos + prev_ch.len_utf8()
                        })
                        .unwrap_or(0)
                } else {
                    word_start
                }
            }
            None => 0,
        }
    };

    let mut new_input = input[..new_byte].to_string();
    new_input.push_str(&input[cur..]);
    let new_cursor = new_input[..new_byte].chars().count();
    (new_input, new_cursor)
}

/// Return the byte bounds (start, end) of the current line's content,
/// excluding the surrounding `\n` characters. `cursor_byte` must be a
/// valid char boundary inside `input`.
pub(super) fn current_line_bounds(input: &str, cursor_byte: usize) -> (usize, usize) {
    let line_start = input[..cursor_byte].rfind('\n').map(|p| p + 1).unwrap_or(0);
    let line_end = input[cursor_byte..]
        .find('\n')
        .map(|p| cursor_byte + p)
        .unwrap_or(input.len());
    (line_start, line_end)
}

/// Convert a `(line, column)` position into the corresponding char-index
/// cursor position. Columns beyond the line length are clamped to the
/// line length (i.e. the `\n` position or the end of the string).
pub(super) fn char_index_for_line_col(input: &str, target_line: usize, target_col: usize) -> usize {
    let mut idx = 0usize;
    for (line_no, line) in input.split('\n').enumerate() {
        if line_no == target_line {
            return idx + target_col.min(line.chars().count());
        }
        idx += line.chars().count() + 1; // +1 for the newline itself
    }
    input.chars().count()
}

/// Direction for post-search match navigation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SearchDirection {
    Next,
    Prev,
}

/// Determine whether `key` is a search-navigation gesture.
///
/// Only plain `n` (next) and `Shift+N` (previous) count; combinations like
/// `Ctrl+n` are left for regular key handling.
pub(super) fn search_nav_direction(key: &KeyEvent) -> Option<SearchDirection> {
    match key.code {
        KeyCode::Char('n') if key.modifiers == KeyModifiers::NONE => Some(SearchDirection::Next),
        KeyCode::Char('N') if key.modifiers == KeyModifiers::SHIFT => Some(SearchDirection::Prev),
        _ => None,
    }
}
