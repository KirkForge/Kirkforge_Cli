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

        // Split at cursor position
        let before = &input[..state.cursor_position];
        let after = &input[state.cursor_position..];

        spans.push(Span::raw(before.to_string()));
        // Always show a cursor, even if empty
        spans.push(Span::styled(
            if after.is_empty() {
                " █".to_string()
            } else {
                format!("{}█", &after[0..1])
            },
            Style::default(),
        ));
        if after.len() > 1 {
            spans.push(Span::raw(after[1..].to_string()));
        }

        vec![Line::from(spans)]
    };

    let paragraph = Paragraph::new(display_text).block(block);
    f.render_widget(paragraph, area);
}
