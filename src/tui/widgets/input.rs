/// Input bar — user command input at the bottom of the screen.
use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Paragraph, Wrap},
    Frame,
};

use crate::tui::app::AppState;

/// Render the input bar showing the current user input and cursor.
///
/// v1.2-p11: the input box is now multi-line. It grows from one row up to
/// the height of `area`, showing as many lines as fit. The cursor is drawn
/// on the current line; the view scrolls to keep the cursor visible when
/// the buffer contains more lines than the visible area.
pub fn render_input(f: &mut Frame, area: Rect, state: &AppState) {
    // Search mode overrides the normal input — the input box
    // becomes a search bar with a different border color and a
    // live match counter.
    if state.search.mode {
        render_search_bar(f, area, state);
        return;
    }

    // Title: just "Input" in normal mode. The search-mode match counter
    // "(N / M matches)" is the only decoration that carries information
    // the user can't already see in the bar itself; the normal-mode line
    // count and paste-flash were noise (the wrapped lines are visible in
    // the box, and the paste already shows up as text). Gate the counter
    // on search.mode (not just matches being non-empty) so stale matches
    // from a prior search don't leak into the normal-mode title.
    let block = Block::default()
        .title(if state.search.mode {
            let total = state.search.matches.len();
            let cur = if total == 0 {
                0
            } else {
                state.search.match_idx + 1
            };
            format!(" Input  ({cur} / {total} matches) ")
        } else {
            " Input ".to_string()
        })
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(Color::Green));

    let visible_rows = area.height.saturating_sub(2) as usize;

    let mut display_text: Vec<Line> = if state.conversation.input.is_empty() {
        vec![Line::from(Span::styled(
            " Type a message or /help for commands...",
            Style::default().fg(Color::DarkGray),
        ))]
    } else {
        let lines: Vec<&str> = state.conversation.input.split('\n').collect();
        let (cursor_line, cursor_col) = state.cursor_line_col();

        // Keep the cursor line visible when there are more lines than rows.
        let first_visible = if lines.len() <= visible_rows {
            0
        } else {
            cursor_line
                .saturating_sub(visible_rows - 1)
                .min(lines.len().saturating_sub(visible_rows))
        };

        lines
            .iter()
            .enumerate()
            .skip(first_visible)
            .take(visible_rows)
            .map(|(idx, line)| {
                if idx == cursor_line {
                    render_cursor_line(line, cursor_col)
                } else {
                    Line::from(line.to_string())
                }
            })
            .collect()
    };

    // WO 14.6: one-line completion suggestions shown above the input
    // text when Tab produced multiple matches (slash commands or
    // @-mention paths). Dim so it reads as a hint, not input.
    if !state.conversation.completion_suggestions.is_empty() {
        display_text.insert(
            0,
            render_suggestions(&state.conversation.completion_suggestions),
        );
    }

    // `.wrap` is required for a long line to wrap within the grown box;
    // without it Paragraph truncates each Line to one row and the wrapped
    // text stays invisible even with the correct box height (WO 30.0.12).
    let paragraph = Paragraph::new(display_text)
        .block(block)
        .wrap(Wrap { trim: false });
    f.render_widget(paragraph, area);
}

fn render_suggestions(suggestions: &[String]) -> Line<'static> {
    let joined = suggestions.join("  ");
    Line::from(Span::styled(
        format!(" {joined} "),
        Style::default()
            .fg(Color::DarkGray)
            .add_modifier(Modifier::DIM),
    ))
}

/// Render the line that currently holds the cursor. The cursor is a
/// green block that REPLACES the character at the cursor position (a
/// solid `█` at end-of-line, or the char under the cursor in green) —
/// NOT a trailing block appended after the char, which doubled the
/// char visually and made mid-text editing look corrupted.
///
/// The cursor uses `Color::Green` foreground (matching the input box
/// border) rather than reverse video (`bg=White, fg=Black`). Reverse
/// video relies on the terminal supporting background colors; on
/// terminals that don't (or that render bg as the default), the
/// cursor was invisible — the same root-cause class as the invisible
/// textbox. A green `█` on the default background is universally
/// supported on xterm-256color and every common terminal.
fn render_cursor_line(line: &str, col: usize) -> Line<'static> {
    let before: String = line.chars().take(col).collect();
    let after: String = line.chars().skip(col).collect();
    let cursor = Style::default().fg(Color::Green);

    let mut spans = Vec::new();
    if !before.is_empty() {
        spans.push(Span::raw(before));
    }
    if after.is_empty() {
        // Cursor at end of line: solid green block, no leading space.
        spans.push(Span::styled("█", cursor));
    } else {
        // Cursor on a char: render that char in green (it replaces
        // the char visually, like a real terminal cursor), then the
        // rest of the line normally. No trailing block.
        let first = after.chars().next().expect("after is non-empty");
        let rest: String = after.chars().skip(1).collect();
        spans.push(Span::styled(first.to_string(), cursor));
        if !rest.is_empty() {
            spans.push(Span::raw(rest));
        }
    }
    Line::from(spans)
}

/// Render the input bar in search mode.
///
/// Yellow border, "Search:" prompt, the live query string, and a
/// match counter. A trailing hint reminds the user how to commit /
/// cancel.
fn render_search_bar(f: &mut Frame, area: Rect, state: &AppState) {
    let block = Block::default()
        .title(" Search (Ctrl+F) ")
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        );

    // Match counter is shown in the corner: " 3 / 12 " or " 0 / 0 ".
    let (cur, total) = if state.search.matches.is_empty() {
        (0, 0)
    } else {
        (state.search.match_idx + 1, state.search.matches.len())
    };
    let counter = format!(" {cur} / {total} ");

    let mut spans = vec![
        Span::styled(
            " 🔍 ",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            state.search.query.clone(),
            Style::default().fg(Color::White),
        ),
        Span::styled("█", Style::default().fg(Color::Yellow)),
        Span::styled(format!("  {counter}"), Style::default().fg(Color::DarkGray)),
    ];
    // Hint at the trailing edge.
    spans.push(Span::styled(
        "  Enter=navigate  Esc=cancel ",
        Style::default()
            .fg(Color::DarkGray)
            .add_modifier(Modifier::DIM),
    ));

    let paragraph = Paragraph::new(Line::from(spans)).block(block);
    f.render_widget(paragraph, area);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Cursor at end of an empty line: a single block span, no leading
    /// space, no extra spans. The prior code emitted `" █"` (block with
    /// a leading space) which shifted the cursor one cell right of the
    /// true insertion point on an empty line.
    #[test]
    fn cursor_line_renders_block_at_end_of_empty_line() {
        let line = render_cursor_line("", 0);
        assert_eq!(line.spans.len(), 1, "empty line should be one span");
        assert_eq!(line.spans[0].content, "█");
        assert!(
            matches!(line.spans[0].style.fg, Some(Color::Green)),
            "block cursor should be green fg (universally supported, not reverse video)"
        );
        assert!(
            line.spans[0].style.bg.is_none(),
            "block cursor should not set a background (reverse video is not portable)"
        );
    }

    /// Cursor at end of a non-empty line ("abc|"): the text before the
    /// cursor renders as one raw span, then a single block span — no
    /// trailing char+block. The prior code rendered `"abc"` then a
    /// redundant block, which was fine here, but this pins that the
    /// block is its own span (not concatenated onto the text span).
    #[test]
    fn cursor_line_renders_block_at_end_of_nonempty_line() {
        let line = render_cursor_line("abc", 3);
        assert_eq!(line.spans.len(), 2, "should be [before, block]");
        assert_eq!(line.spans[0].content, "abc");
        assert_eq!(line.spans[1].content, "█");
        assert!(
            matches!(line.spans[1].style.fg, Some(Color::Green)),
            "block cursor should be green fg"
        );
    }

    /// Cursor on a char mid-line ("a|bc"): the char under the cursor
    /// renders in green (it REPLACES the char visually, like a real
    /// terminal cursor), the rest renders normally. The prior code
    /// emitted `"a"` + `"b█"` + `"c"` — the char under the cursor AND a
    /// trailing block, which doubled the char visually and made mid-text
    /// editing look corrupted. This pins the fix: one green span for
    /// the char under the cursor, no trailing block.
    #[test]
    fn cursor_line_renders_green_char_under_cursor() {
        let line = render_cursor_line("abc", 1);
        // [before="a"], [char-under-cursor="b" green], [rest="c"]
        assert_eq!(line.spans.len(), 3, "should be [before, green-char, rest]");
        assert_eq!(line.spans[0].content, "a");
        assert_eq!(line.spans[1].content, "b");
        assert!(
            matches!(line.spans[1].style.fg, Some(Color::Green)),
            "char under cursor should be green fg (portable cursor render)"
        );
        assert!(
            line.spans[1].style.bg.is_none(),
            "char under cursor should not set a background (reverse video is not portable)"
        );
        assert_eq!(line.spans[2].content, "c");
        // The rest span must NOT carry the green style.
        assert!(
            line.spans[2].style.fg.is_none(),
            "rest of line should not be green"
        );
    }

    /// Cursor at start of line ("|abc"): no `before` span, the first
    /// char is green, the rest is raw. Guards the col==0 edge.
    #[test]
    fn cursor_line_at_start_renders_first_char_green() {
        let line = render_cursor_line("abc", 0);
        assert_eq!(line.spans.len(), 2, "should be [green-char, rest]");
        assert_eq!(line.spans[0].content, "a");
        assert!(
            matches!(line.spans[0].style.fg, Some(Color::Green)),
            "first char under cursor should be green fg"
        );
        assert_eq!(line.spans[1].content, "bc");
    }

    /// The non-cursor spans must reconstruct the original line — the
    /// cursor render must not drop or duplicate any text character. The
    /// block cursor span at end-of-line represents the cursor itself,
    /// not text, so it is excluded from the reconstruction. This catches
    /// the prior bug where the char under the cursor was emitted twice
    /// (once as text, once in the block): at col 1 on "abc" the prior
    /// code joined to "ab█c" — the 'b' appeared once but a spurious
    /// block was inserted, and the join length was wrong. Here the
    /// reverse-video span carries the char under the cursor (so it is
    /// part of the reconstruction), and any trailing block at EOL is
    /// the cursor-only span.
    #[test]
    fn cursor_line_preserves_all_text_characters() {
        // At end-of-line: spans are [before, block]. The block is the
        // cursor — join the non-block spans to get the text.
        let line = render_cursor_line("abc", 3);
        let joined: String = line
            .spans
            .iter()
            .filter(|s| s.content != "█")
            .map(|s| s.content.as_ref())
            .collect();
        assert_eq!(joined, "abc", "EOL cursor: text must be preserved");

        // Mid-line: spans are [before, reverse-char, rest]. The
        // reverse-char span carries a real text char, so it IS part of
        // the reconstruction. No block is emitted mid-line.
        let line = render_cursor_line("abc", 1);
        let joined: String = line
            .spans
            .iter()
            .filter(|s| s.content != "█")
            .map(|s| s.content.as_ref())
            .collect();
        assert_eq!(
            joined, "abc",
            "mid-line cursor: text must be preserved, no duplicated char"
        );

        // At start: spans are [reverse-char, rest].
        let line = render_cursor_line("abc", 0);
        let joined: String = line
            .spans
            .iter()
            .filter(|s| s.content != "█")
            .map(|s| s.content.as_ref())
            .collect();
        assert_eq!(joined, "abc", "start cursor: text must be preserved");

        // Empty line: just the block.
        let line = render_cursor_line("", 0);
        let joined: String = line
            .spans
            .iter()
            .filter(|s| s.content != "█")
            .map(|s| s.content.as_ref())
            .collect();
        assert_eq!(joined, "", "empty line: no text spans");
    }
}
