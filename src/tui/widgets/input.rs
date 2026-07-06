/// Input bar — user command input at the bottom of the screen.
use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Paragraph},
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
    if state.search_mode {
        render_search_bar(f, area, state);
        return;
    }

    let block = Block::default()
        .title(if !state.search_matches.is_empty() {
            let total = state.search_matches.len();
            let cur = state.search_match_idx + 1;
            format!(" Input  ({cur} / {total} matches) ")
        } else if state.input.contains('\n') {
            format!(" Input  ({} lines) ", state.input_line_count())
        } else {
            " Input ".to_string()
        })
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(Color::Green));

    let visible_rows = area.height.saturating_sub(2) as usize;

    let display_text: Vec<Line> = if state.input.is_empty() {
        vec![Line::from(Span::styled(
            " Type a message or /help for commands...",
            Style::default().fg(Color::DarkGray),
        ))]
    } else {
        let lines: Vec<&str> = state.input.split('\n').collect();
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

    let paragraph = Paragraph::new(display_text).block(block);
    f.render_widget(paragraph, area);
}

/// Render the line that currently holds the cursor, with a block cursor.
fn render_cursor_line(line: &str, col: usize) -> Line<'static> {
    let mut spans = Vec::new();
    let before: String = line.chars().take(col).collect();
    let after: String = line.chars().skip(col).collect();

    spans.push(Span::raw(before));
    // Always show a cursor, even if the line is empty.
    spans.push(Span::styled(
        if after.is_empty() {
            " █".to_string()
        } else {
            // Highlight the first char after the cursor and append the
            // block cursor marker so the insertion point is unambiguous.
            let first = after.chars().next().unwrap_or(' ');
            format!("{first}█")
        },
        Style::default(),
    ));
    if !after.is_empty() {
        let char_len = after.chars().next().map(|c| c.len_utf8()).unwrap_or(0);
        spans.push(Span::raw(after[char_len..].to_string()));
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
    let (cur, total) = if state.search_matches.is_empty() {
        (0, 0)
    } else {
        (state.search_match_idx + 1, state.search_matches.len())
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
            state.search_query.clone(),
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
