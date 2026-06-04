/// Input bar — user command input at the bottom of the screen.
use ratatui::{
    layout::Rect,
    style::{Color, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Paragraph},
    Frame,
};

use crate::tui::app::AppState;

/// Render the input bar showing the current user input and cursor.
pub fn render_input(f: &mut Frame, area: Rect, state: &AppState) {
    let block = Block::default()
        .title(" Input ")
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
