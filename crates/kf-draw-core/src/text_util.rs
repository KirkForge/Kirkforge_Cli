//! Text-cell helpers: grapheme segmentation, terminal cell width, padding,
//! truncation, and the per-border text-anchor helpers used by both the
//! editor state and the renderer.
//!
//! Terminal cell width uses `unicode-width`. Grapheme clusters come from
//! `unicode-segmentation`. Both are the standard Rust choices.

use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use crate::types::{Point, Rect, TextBorderMode, TextObject};

/// The number of terminal cells a string occupies.
#[inline]
pub fn visible_cell_count(text: &str) -> usize {
    UnicodeWidthStr::width(text)
}

/// Number of lines in a piece of text content. Empty / no-`\n`
/// strings are one line; a trailing `\n` does not create an empty
/// trailing line (matches `str::lines()` semantics — the trailing
/// empty line is dropped because there's nothing after the last
/// `\n`).
///
/// ponytail: single-character separator. Real word-wrapping
/// (fit-to-width, indent-aware) is a future tick; today the
/// editor and renderer treat `\n` as the only line break.
#[inline]
pub fn line_count(text: &str) -> usize {
    if text.is_empty() {
        1
    } else {
        // str::lines() omits the trailing empty line for strings
        // that end with \n — that's what we want for render
        // (a doc-ending \n shouldn't paint an empty row).
        text.lines().count().max(1)
    }
}

/// Width in terminal cells of the widest `\n`-separated line.
///
/// ponytail: walks the string once. For typical text content
/// (≤ a few hundred cells) this is free; if we ever ship huge
/// text objects we'd want to cache.
#[inline]
pub fn widest_line(text: &str) -> usize {
    if text.is_empty() {
        return 1;
    }
    text.lines()
        .map(visible_cell_count)
        .max()
        .unwrap_or(1)
        .max(1)
}

/// Split a string into grapheme clusters.
#[inline]
pub fn split_graphemes(text: &str) -> Vec<&str> {
    text.graphemes(true).collect()
}

/// Normalize a single character: the first grapheme of the input, or a
/// single space if the input is empty. Mirrors the TS `normalizeCellCharacter`
/// helper — used for the brush slot and the text object content.
pub fn normalize_cell_character(input: &str) -> &str {
    // We can't return &'str pointing into a temporary, so we return either
    // a slice of the input (if it has a grapheme) or a static " ".
    let mut iter = input.graphemes(true);
    iter.next().unwrap_or(" ")
}

/// Pad a string with spaces to the requested cell width, truncating first
/// if it already exceeds the width.
pub fn pad_to_width(text: &str, width: usize) -> String {
    let current = visible_cell_count(text);
    if current >= width {
        truncate_to_cells(text, width)
    } else {
        let mut out = String::with_capacity(text.len() + (width - current));
        out.push_str(text);
        for _ in 0..(width - current) {
            out.push(' ');
        }
        out
    }
}

/// Truncate a string to at most `width` terminal cells. The result is
/// always the longest prefix whose cell count does not exceed `width`.
pub fn truncate_to_cells(text: &str, width: usize) -> String {
    let mut out = String::with_capacity(text.len());
    let mut cells = 0usize;
    for g in text.graphemes(true) {
        let w = UnicodeWidthStr::width(g);
        if cells + w > width {
            break;
        }
        out.push_str(g);
        cells += w;
    }
    out
}

/// Where the text content characters actually render. The frame
/// decorations (single/double/underline) shift the content origin.
/// Multi-line content's origin is the first line; subsequent
/// lines stamp at `origin.y + line_index`.
pub fn get_text_content_origin(text: &TextObject) -> Point {
    if text.border == TextBorderMode::None || text.border == TextBorderMode::Underline {
        Point {
            x: text.x,
            y: text.y,
        }
    } else {
        Point {
            x: text.x,
            y: text.y + 1,
        }
    }
}

/// The rect that contains the full rendered text object (frame + content).
/// Width is the widest `\n`-separated line; height is the line count,
/// plus the border's overhead row(s) for `Single`/`Double`.
///
/// ponytail: ceil-only line stacking. The editor doesn't auto
/// word-wrap a long line into the next row — the user inserts
/// `\n` (Shift+Enter) themselves. Auto-wrap is a future tick.
pub fn get_text_render_rect(text: &TextObject) -> Rect {
    let content_width = widest_line(&text.content) as i32;
    let n_lines = line_count(&text.content) as i32;
    match text.border {
        TextBorderMode::None => Rect {
            left: text.x,
            top: text.y,
            right: text.x + content_width - 1,
            bottom: text.y + n_lines - 1,
        },
        TextBorderMode::Underline => Rect {
            left: text.x,
            top: text.y,
            right: text.x + content_width - 1,
            bottom: text.y + n_lines,
        },
        TextBorderMode::Single | TextBorderMode::Double => Rect {
            left: text.x,
            top: text.y,
            right: text.x + content_width + 1,
            bottom: text.y + 1 + n_lines,
        },
    }
}

/// The marquee selection rect for a text object — same as render rect
/// for v1. Kept as a separate function so we can diverge later (e.g.
/// include a wider hit area for an active edit cursor).
pub fn get_text_selection_bounds(text: &TextObject) -> Rect {
    get_text_render_rect(text)
}

/// Where the F2 edit cursor should be drawn for the given
/// (object, buffer, cursor_offset) triple. `cursor_offset` is
/// a byte index into `buffer` (0 = before the first char,
/// `buffer.len()` = end of buffer). The cursor sits at the
/// cell immediately *before* the byte at that index, on the
/// same row as the surrounding content.
///
/// For multi-line content, the cursor lands on the line that
/// contains `cursor_offset`, at the column right after the
/// preceding char on that line. The byte index is split into
/// (line_idx, line_byte_offset) by `buffer[..offset]`: the
/// prefix's newline count gives the 0-based line index; the
/// suffix after the last `\n` is the slice whose cell-width
/// is the column.
///
/// ponytail: byte index, not grapheme index. Inserting or
/// removing a single ASCII char is a 1-byte splice (the
/// editor's text_edit_insert / text_edit_backspace use the
/// same units). Multi-byte UTF-8 sequences (CJK, emoji) are
/// 3–4 bytes per grapheme — a single arrow-key press
/// advances the offset by one byte, which lands inside a
/// multi-byte char. The render path already handles
/// this: a `char` boundary violation produces an
/// `Option::None` from `str::chars()` and the cursor just
/// sits at the end of the previous grapheme. Future tick:
/// grapheme-aware offset (track offset in cells, splice
/// in grapheme units). Same for F2's left/right arms.
pub fn text_edit_cursor_position(text: &TextObject, buffer: &str, cursor_offset: usize) -> Point {
    let origin = get_text_content_origin(text);
    let offset = cursor_offset.min(buffer.len());
    let prefix = &buffer[..offset];
    let line_idx = prefix.matches('\n').count() as i32;
    let last_line = prefix.rfind('\n').map_or(prefix, |idx| &prefix[idx + 1..]);
    let cells = visible_cell_count(last_line) as i32;
    Point {
        x: origin.x + cells,
        y: origin.y + line_idx,
    }
}

/// Return the byte offset that the F2 cursor would land on
/// after pressing Up (`delta = -1`) or Down (`delta = 1`).
/// Preserves the column (distance from the start of the
/// current line) when the target line is at least as long
/// as the current column; clamps to the target line's end
/// otherwise — the standard editor behavior. Returns
/// `None` when there's no prior / next line (Up at line 0,
/// Down at the last line), so the caller can leave the
/// offset untouched without checking direction.
///
/// ponytail: byte index, not cell index. The column is
/// measured in bytes (one ASCII char = one byte; CJK
/// ideographs and emoji are 3–4 bytes). Pressing Down from
/// column 5 in "abcde" to a shorter line "ab" puts the
/// cursor at column 2 (the end of "ab"). The visible
/// cursor still paints at the cell immediately before the
/// byte at that offset, so a multi-byte column produces a
/// cursor somewhere in the middle of a multi-byte char —
/// same trade-off as the byte-offset Left / Right.
/// Future tick: grapheme-aware column tracking.
pub fn line_nav_offset(buffer: &str, cursor_offset: usize, delta: i32) -> Option<usize> {
    let off = cursor_offset.min(buffer.len());
    // Line index of the cursor: count '\n' in buffer[..off].
    let line_idx = buffer[..off].matches('\n').count() as i32;
    // Column within the current line: bytes since the last
    // '\n' (or since the buffer start if there's no '\n').
    let column = off - buffer[..off].rfind('\n').map_or(0, |i| i + 1);
    // Walk the line boundaries as (start, end_excl) pairs.
    // A trailing '\n' doesn't paint a phantom empty line —
    // matches `line_count` / `str::lines()` semantics.
    let mut lines: Vec<(usize, usize)> = Vec::new();
    let mut start = 0usize;
    for (i, _) in buffer.match_indices('\n') {
        lines.push((start, i));
        start = i + 1;
    }
    lines.push((start, buffer.len()));
    let target_line = line_idx + delta;
    if target_line < 0 || target_line >= lines.len() as i32 {
        return None;
    }
    let (target_start, target_end) = lines[target_line as usize];
    let target_len = target_end - target_start;
    Some(target_start + column.min(target_len))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::InkColor;

    fn make_text(content: &str, border: TextBorderMode) -> TextObject {
        TextObject {
            id: "t-1".into(),
            z: 1,
            parent_id: None,
            color: InkColor::White,
            x: 2,
            y: 3,
            content: content.into(),
            border,
        }
    }

    #[test]
    fn ascii_cell_count_is_length() {
        assert_eq!(visible_cell_count("hello"), 5);
        assert_eq!(visible_cell_count(""), 0);
    }

    #[test]
    fn cjk_cell_count_is_two_per_char() {
        // CJK ideographs occupy two cells.
        assert_eq!(visible_cell_count("日本"), 4);
    }

    #[test]
    fn split_graphemes_handles_combining() {
        // "é" can be one codepoint (U+00E9) or two (e + combining acute).
        // Both forms should split into a single grapheme.
        let single = split_graphemes("é");
        assert_eq!(single.len(), 1);
        let combined = split_graphemes("e\u{0301}");
        assert_eq!(combined.len(), 1);
    }

    #[test]
    fn split_graphemes_keeps_zwj_family_intact() {
        // 👨‍👩‍👧 = man + ZWJ + woman + ZWJ + girl.
        let g = split_graphemes("👨\u{200d}👩\u{200d}👧");
        assert_eq!(g.len(), 1);
    }

    #[test]
    fn normalize_cell_character_takes_first_grapheme() {
        assert_eq!(normalize_cell_character("a"), "a");
        assert_eq!(normalize_cell_character(""), " ");
        assert_eq!(normalize_cell_character("e\u{0301}"), "e\u{0301}");
    }

    #[test]
    fn pad_to_width_pads_short_and_truncates_long() {
        assert_eq!(pad_to_width("hi", 5), "hi   ");
        assert_eq!(pad_to_width("hello", 3), "hel");
        assert_eq!(pad_to_width("hello", 5), "hello");
    }

    #[test]
    fn truncate_to_cells_respects_cjk_width() {
        // "日本" is 4 cells; truncate to 3 should give "日" (2 cells).
        assert_eq!(truncate_to_cells("日本", 3), "日");
    }

    #[test]
    fn text_origin_below_top_for_framed_borders() {
        let t = make_text("hi", TextBorderMode::Single);
        let o = get_text_content_origin(&t);
        assert_eq!(o, Point { x: 2, y: 4 });
    }

    #[test]
    fn text_origin_at_top_for_underline_or_none() {
        let t1 = make_text("hi", TextBorderMode::Underline);
        let o1 = get_text_content_origin(&t1);
        assert_eq!(o1, Point { x: 2, y: 3 });

        let t2 = make_text("hi", TextBorderMode::None);
        let o2 = get_text_content_origin(&t2);
        assert_eq!(o2, Point { x: 2, y: 3 });
    }

    #[test]
    fn text_render_rect_grows_with_border() {
        let none = make_text("abc", TextBorderMode::None);
        assert_eq!(get_text_render_rect(&none).bottom, none.y);

        let underline = make_text("abc", TextBorderMode::Underline);
        assert_eq!(get_text_render_rect(&underline).bottom, underline.y + 1);

        let framed = make_text("abc", TextBorderMode::Single);
        let r = get_text_render_rect(&framed);
        assert_eq!(r.left, framed.x);
        assert_eq!(r.right, framed.x + 3 + 1);
        assert_eq!(r.bottom, framed.y + 2);
    }

    #[test]
    fn line_count_handles_empty_and_newlines() {
        // Empty string is one (empty) line — width math still works.
        assert_eq!(line_count(""), 1);
        // Single-line content stays one line.
        assert_eq!(line_count("hello"), 1);
        // Three explicit lines via two separators.
        assert_eq!(line_count("a\nb\nc"), 3);
        // Trailing newline doesn't add a phantom empty row —
        // matches `str::lines()` and what the renderer stamps.
        assert_eq!(line_count("a\nb\n"), 2);
        // Single trailing newline on otherwise empty content is
        // also one line (no row to paint).
        assert_eq!(line_count("\n"), 1);
    }

    #[test]
    fn widest_line_takes_max_across_lines() {
        // Single-line: width equals the line's cell count.
        assert_eq!(widest_line("hello"), 5);
        // Multi-line: take the widest.
        assert_eq!(widest_line("hi\nworld\nyo"), 5);
        // Empty → 1 (matches the max(1) inside the renderer so
        // a zero-width rect never reaches the renderer).
        assert_eq!(widest_line(""), 1);
        // CJK in only one line — width is 2 per ideograph.
        assert_eq!(widest_line("hi\n日本"), 4);
    }

    #[test]
    fn multi_line_render_rect_grows_height() {
        // Two-line content, no border: rect is two rows tall.
        let t = make_text("ab\ncd", TextBorderMode::None);
        let r = get_text_render_rect(&t);
        assert_eq!(r.top, t.y);
        assert_eq!(r.bottom, t.y + 1);
        // Widest line is "ab"/"cd", both 2 cells wide.
        assert_eq!(r.right, t.x + 2 - 1);
    }

    #[test]
    fn multi_line_render_rect_uses_widest_line_for_width() {
        // "abc" + "longer" — width is 6 (the longer line), not 7+.
        let t = make_text("abc\nlonger", TextBorderMode::None);
        let r = get_text_render_rect(&t);
        // x is 2 (default), widest line is "longer" at 6 cells.
        assert_eq!(r.right, t.x + 6 - 1);
        // Two rows.
        assert_eq!(r.bottom, t.y + 1);
    }

    #[test]
    fn multi_line_render_rect_with_underline_border() {
        // Underline extends the bottom by 1 row on top of the
        // line count, same as for single-line text.
        let t = make_text("ab\ncd", TextBorderMode::Underline);
        let r = get_text_render_rect(&t);
        assert_eq!(r.bottom, t.y + 2);
    }

    #[test]
    fn multi_line_render_rect_with_single_frame() {
        // Frame adds one row above (top border) + the line count.
        let t = make_text("ab\ncd", TextBorderMode::Single);
        let r = get_text_render_rect(&t);
        assert_eq!(r.top, t.y);
        assert_eq!(r.bottom, t.y + 1 + 2);
        // Width includes the right border.
        assert_eq!(r.right, t.x + 2 + 1);
    }

    #[test]
    fn cursor_position_at_origin_for_empty_buffer() {
        // No content typed yet: cursor sits at content_origin.
        let t = make_text("", TextBorderMode::None);
        let p = text_edit_cursor_position(&t, "", 0);
        assert_eq!(p, Point { x: t.x, y: t.y });
    }

    #[test]
    fn cursor_position_advances_with_cell_width() {
        // Each ASCII char is one cell — cursor x = origin + 3.
        let t = make_text("abc", TextBorderMode::None);
        let p = text_edit_cursor_position(&t, "abc", 3);
        assert_eq!(p, Point { x: t.x + 3, y: t.y });
    }

    #[test]
    fn cursor_position_uses_cjk_width_per_grapheme() {
        // CJK ideographs are two cells each — "日本" is 4 cells.
        let t = make_text("日本", TextBorderMode::None);
        let p = text_edit_cursor_position(&t, "日本", 6);
        assert_eq!(p, Point { x: t.x + 4, y: t.y });
    }

    #[test]
    fn cursor_position_drops_to_next_line_after_newline() {
        // Buffer "ab\ncd" — cursor on line 2, column 2.
        let t = make_text("ab\ncd", TextBorderMode::None);
        let p = text_edit_cursor_position(&t, "ab\ncd", 5);
        assert_eq!(
            p,
            Point {
                x: t.x + 2,
                y: t.y + 1
            }
        );
    }

    #[test]
    fn cursor_position_three_lines_lands_on_third() {
        // Buffer "a\nb\nc" — cursor on line 3, column 1.
        let t = make_text("a\nb\nc", TextBorderMode::None);
        let p = text_edit_cursor_position(&t, "a\nb\nc", 5);
        assert_eq!(
            p,
            Point {
                x: t.x + 1,
                y: t.y + 2
            }
        );
    }

    #[test]
    fn cursor_position_shifts_below_top_for_framed_border() {
        // Single border pushes content_origin down one row;
        // cursor follows it.
        let t = make_text("hi", TextBorderMode::Single);
        let p = text_edit_cursor_position(&t, "hi", 2);
        assert_eq!(
            p,
            Point {
                x: t.x + 2,
                y: t.y + 1
            }
        );
    }

    #[test]
    fn cursor_position_mid_buffer_returns_intermediate_cell() {
        // Buffer "abc" — offset 1 means before the 'b', so the
        // cursor sits at column 1 (right after 'a').
        let t = make_text("abc", TextBorderMode::None);
        let p = text_edit_cursor_position(&t, "abc", 1);
        assert_eq!(p, Point { x: t.x + 1, y: t.y });
    }

    #[test]
    fn cursor_position_offset_zero_sits_before_first_char() {
        // Buffer "abc" — offset 0 is the very start, before 'a'.
        let t = make_text("abc", TextBorderMode::None);
        let p = text_edit_cursor_position(&t, "abc", 0);
        assert_eq!(p, Point { x: t.x, y: t.y });
    }

    #[test]
    fn cursor_position_mid_second_line() {
        // Buffer "abc\ndef" — offset 5 (between 'd' and 'e')
        // lands on line 2, column 1.
        let t = make_text("abc\ndef", TextBorderMode::None);
        let p = text_edit_cursor_position(&t, "abc\ndef", 5);
        assert_eq!(
            p,
            Point {
                x: t.x + 1,
                y: t.y + 1
            }
        );
    }

    #[test]
    fn cursor_position_offset_past_end_clamps_to_eob() {
        // Caller passes offset > buffer.len() — should clamp
        // to the end of the buffer rather than panic.
        let t = make_text("abc", TextBorderMode::None);
        let p = text_edit_cursor_position(&t, "abc", 99);
        assert_eq!(p, Point { x: t.x + 3, y: t.y });
    }

    // -- line_nav_offset (F2 Up / Down) --------------------------
    //
    // The pure helper computes the byte offset for one step
    // of vertical navigation. Up from line 0 and Down from
    // the last line return None; the bin side treats that as
    // a no-op. Column preservation: a longer target line
    // keeps the column; a shorter target line clamps to its
    // end.

    #[test]
    fn line_nav_up_from_second_line_preserves_column() {
        // Buffer "abc\ndef" — offset 5 is between 'd' and
        // 'e' (line 2, column 1). Up → line 1, column 1
        // (between 'a' and 'b'), offset 1.
        assert_eq!(line_nav_offset("abc\ndef", 5, -1), Some(1));
    }

    #[test]
    fn line_nav_down_from_first_line_preserves_column() {
        // Buffer "abc\ndef" — offset 1 is line 1, column 1.
        // Down → line 2, column 1, offset 5.
        assert_eq!(line_nav_offset("abc\ndef", 1, 1), Some(5));
    }

    #[test]
    fn line_nav_up_from_first_line_returns_none() {
        // Already on line 0 — no prior line to walk to.
        assert_eq!(line_nav_offset("abc", 2, -1), None);
        assert_eq!(line_nav_offset("abc\ndef", 0, -1), None);
    }

    #[test]
    fn line_nav_down_from_last_line_returns_none() {
        // Buffer "abc\ndef" — last line ends at offset 7.
        // Down from any offset on line 2 → None.
        assert_eq!(line_nav_offset("abc\ndef", 7, 1), None);
    }

    #[test]
    fn line_nav_clamps_to_shorter_target_line() {
        // Buffer "abc\nabcde" — line 1 length 3, line 2
        // length 5. Cursor at offset 8 (end of "abcde",
        // column 5 on line 2). Up → line 1, but its length
        // is 3, so column clamps to 3 → offset 3 (end of
        // "abc", i.e. the '\n' byte).
        assert_eq!(line_nav_offset("abc\nabcde", 8, -1), Some(3));
    }

    #[test]
    fn line_nav_keeps_column_on_longer_target_line() {
        // Buffer "ab\nabcde" — line 1 length 2, line 2
        // length 5. Cursor at offset 2 (end of "ab", column
        // 2 on line 1). Down → line 2, column 2 (within
        // length 5) → offset 5.
        assert_eq!(line_nav_offset("ab\nabcde", 2, 1), Some(5));
    }

    #[test]
    fn line_nav_round_trip_three_lines() {
        // Buffer "abc\ndef\nghi" — start at line 2 col 2
        // (offset 6: 'a', 'b', 'c', '\n', 'd', 'e'). Up
        // then Down returns to the same offset.
        let buf = "abc\ndef\nghi";
        let start = 6;
        let up = line_nav_offset(buf, start, -1).unwrap();
        let back = line_nav_offset(buf, up, 1).unwrap();
        assert_eq!(back, start);
    }

    #[test]
    fn line_nav_three_lines_up_up_lands_on_first_line() {
        // Buffer "a\nbbc" — line 1 length 1, line 2 length 3.
        // Start at offset 5 (end of "bbc", column 3 on line
        // 2). Up → line 1 ("a"), column 3 clamped to length
        // 1 → offset 1. Demonstrates the column-clamp on a
        // shorter target line.
        let buf = "a\nbbc";
        let step1 = line_nav_offset(buf, 5, -1).unwrap();
        assert_eq!(step1, 1, "clamp from column 3 to length 1");
    }

    #[test]
    fn line_nav_on_single_line_buffer_returns_none_both_directions() {
        // Buffer without '\n' — single line, no neighbors.
        assert_eq!(line_nav_offset("abc", 1, -1), None);
        assert_eq!(line_nav_offset("abc", 1, 1), None);
    }
}
