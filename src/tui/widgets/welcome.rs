//! Welcome screen — centered banner shown on a fresh session.
//!
//! Rendered when `messages.is_empty() && input.is_empty()`. Any keystroke
//! into the input dismisses it (the welcome is purely a render gate,
//! not a mode). Ctrl+O opens a directory picker overlay (via
//! FileCompleter in pick_directory mode).

use crate::tui::app::AppState;
use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

/// Render the welcome screen in the main content area.
pub fn render_welcome(f: &mut Frame, area: Rect, state: &AppState) {
    let mut lines = Vec::new();

    // Vertical padding to center the block.
    let center_pad = area.height.saturating_sub(12) / 2;
    for _ in 0..center_pad {
        lines.push(Line::from(""));
    }

    // Banner
    lines.push(Line::from(Span::styled(
        "  k i r k f o r g e",
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    )));
    lines.push(Line::from(""));

    // CWD
    let cwd = state.ui.cwd.display().to_string();
    lines.push(Line::from(vec![
        Span::styled("  cwd: ", Style::default().fg(Color::DarkGray)),
        Span::styled(cwd, Style::default().fg(Color::White)),
    ]));
    lines.push(Line::from(""));

    // Session info
    if !state.session.session_id.is_empty() {
        lines.push(Line::from(vec![
            Span::styled("  session: ", Style::default().fg(Color::DarkGray)),
            Span::styled(&state.session.session_id, Style::default().fg(Color::White)),
        ]));
    }
    lines.push(Line::from(""));

    // Recent forks (if available from the fork manager)
    if let Some(ref fm) = state.session.fork_manager {
        let forks = fm.list();
        if !forks.is_empty() {
            lines.push(Line::from(Span::styled(
                "  Recent:",
                Style::default().fg(Color::DarkGray),
            )));
            for fork in forks.iter().take(5) {
                lines.push(Line::from(Span::styled(
                    format!("    {} — {}", fork.id, fork.label),
                    Style::default().fg(Color::DarkGray),
                )));
            }
        }
    }

    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "  Type a message or /help for commands. Ctrl+O to open a project.",
        Style::default().fg(Color::DarkGray),
    )));

    let paragraph = Paragraph::new(lines);
    f.render_widget(paragraph, area);
}
