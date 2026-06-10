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
            format!(" Input  ({} / {} matches) ", cur, total)
        } else {
            " Input ".to_string()
        })
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(Color::Green));

    // Show a placeholder when empty
    let display_text = if state.input.is_empty() {
        vec![Line::from(Span::styled(
            " Type a message or /help for commands...",
            Style::default().fg(Color::DarkGray),
        ))]
    } else {
        // Show the input with cursor indicator
        let mut spans = Vec::new();
        let input = &state.input;

        // Convert char-index cursor to byte offset for safe slicing
        let byte_pos = state.cursor_byte();

        // Split at cursor position using byte offset
        let before = &input[..byte_pos];
        let after = &input[byte_pos..];

        spans.push(Span::raw(before.to_string()));
        // Always show a cursor, even if empty
        spans.push(Span::styled(
            if after.is_empty() {
                " █".to_string()
            } else {
                // Get the first char (safe — byte_pos is on a char boundary since
                // cursor_byte() walks char_indices)
                let first_char = after.chars().next().unwrap_or(' ');
                format!("{}█", first_char)
            },
            Style::default(),
        ));
        if !after.is_empty() {
            // Skip the first multi-byte-safe char
            let char_len = after.chars().next().map(|c| c.len_utf8()).unwrap_or(0);
            spans.push(Span::raw(after[char_len..].to_string()));
        }

        vec![Line::from(spans)]
    };

    let paragraph = Paragraph::new(display_text).block(block);
    f.render_widget(paragraph, area);
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
    let counter = format!(" {} / {} ", cur, total);

    let mut spans = vec![
        Span::styled(
            " 🔍 ",
            Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            state.search_query.clone(),
            Style::default().fg(Color::White),
        ),
        Span::styled(
            "█",
            Style::default().fg(Color::Yellow),
        ),
        Span::styled(
            format!("  {}", counter),
            Style::default().fg(Color::DarkGray),
        ),
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
