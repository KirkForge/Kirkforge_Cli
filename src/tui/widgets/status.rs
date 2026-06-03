/// Status bar — model info, token counts, connection state.
use crate::tui::app::{AppState, ConnectionState};
use crate::tui::rendering::{format_duration, format_token_count};
use ratatui::{
    layout::Rect,
    style::{Color, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

/// Render the status bar at the bottom of the screen.
pub fn render_status(f: &mut Frame, area: Rect, state: &AppState) {
    let left_info = match &state.connection {
        ConnectionState::Disconnected => {
            Span::styled(" ⚡ Disconnected ", Style::default().fg(Color::Red))
        }
        ConnectionState::Connecting => {
            Span::styled(" ⟳ Connecting... ", Style::default().fg(Color::Yellow))
        }
        ConnectionState::Connected { model, .. } => {
            Span::styled(
                format!(" ◆ {} ", model),
                Style::default().fg(Color::Green),
            )
        }
        ConnectionState::Error(e) => {
            Span::styled(format!(" ✗ {} ", e), Style::default().fg(Color::Red))
        }
    };

    let elapsed = format_duration(state.session_started.elapsed().as_secs_f64());
    let right_info = format!(
        " ↑{} ↓{} │ {} ",
        format_token_count(state.tokens_sent),
        format_token_count(state.tokens_received),
        elapsed,
    );

    let left_len = left_info.content.len();
    let space = if area.width as usize > left_len + right_info.len() + 2 {
        area.width as usize - left_len - right_info.len()
    } else {
        1
    };

    let spacing = " ".repeat(space);

    let line = Line::from(vec![
        left_info,
        Span::styled(spacing, Style::default()),
        Span::styled(right_info, Style::default().fg(Color::DarkGray)),
    ]);

    let paragraph = Paragraph::new(line)
        .style(Style::default().bg(Color::Black).fg(Color::White));
    f.render_widget(paragraph, area);
}