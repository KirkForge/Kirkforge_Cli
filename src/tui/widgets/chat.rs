/// Chat panel — the main conversation view.
use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, BorderType, Borders, Paragraph, Wrap},
    Frame,
};

use crate::tui::app::{AppState, ConnectionState};

/// Render the main chat area showing the conversation history.
pub fn render_chat(f: &mut Frame, area: Rect, state: &AppState) {
    let mut lines: Vec<Line> = Vec::new();

    // Connection banner at top
    match &state.connection {
        ConnectionState::Disconnected => {
            lines.push(Line::from(vec![
                Span::styled(" ⚡ Disconnected ", Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)),
                Span::styled("Press Enter to start a session, or type /connect <model>", Style::default().fg(Color::DarkGray)),
            ]));
        }
        ConnectionState::Connecting => {
            lines.push(Line::from(vec![
                Span::styled(" ⟳ Connecting... ", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
            ]));
        }
        ConnectionState::Connected { model, .. } => {
            lines.push(Line::from(vec![
                Span::styled(" ◆ ", Style::default().fg(Color::Green)),
                Span::styled("Connected: ", Style::default().fg(Color::DarkGray)),
                Span::styled(model.clone(), Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
            ]));
        }
        ConnectionState::Error(e) => {
            lines.push(Line::from(vec![
                Span::styled(" ✗ Error: ", Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)),
                Span::styled(e.clone(), Style::default().fg(Color::Red)),
            ]));
        }
    }

    // Empty line after banner
    lines.push(Line::from(""));

    // Conversation messages
    for entry in &state.messages {
        let role_style = match entry.role.as_str() {
            "user" => Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
            "assistant" => Style::default().fg(Color::Green).add_modifier(Modifier::BOLD),
            "system" => Style::default().fg(Color::DarkGray),
            "tool" => Style::default().fg(Color::Yellow),
            "thinking" => Style::default().fg(Color::Magenta).add_modifier(Modifier::DIM),
            _ => Style::default().fg(Color::White),
        };

        let time_str = entry.timestamp.format("%H:%M:%S").to_string();

        lines.push(Line::from(vec![
            Span::styled(format!(" {} ", time_str), Style::default().fg(Color::DarkGray)),
            Span::styled(entry.role.to_string(), role_style),
        ]));

        // Wrap content to available width
        let content_width = (area.width as usize).saturating_sub(4);
        let wrapped = textwrap::fill(&entry.content, content_width);

        for content_line in wrapped.lines() {
            lines.push(Line::from(Span::styled(
                format!(" {}", content_line),
                Style::default(),
            )));
        }
        lines.push(Line::from(""));
    }

    // Thinking panel (collapsible)
    if state.thinking_panel_visible && !state.thinking_buffer.is_empty() {
        lines.push(Line::from(vec![
            Span::styled(" ── Thinking ──", Style::default().fg(Color::Magenta).add_modifier(Modifier::DIM)),
        ]));
        for t in &state.thinking_buffer {
            let content_width = (area.width as usize).saturating_sub(4);
            for line in textwrap::fill(t, content_width).lines() {
                lines.push(Line::from(Span::styled(
                    format!(" {}", line),
                    Style::default().fg(Color::Magenta).add_modifier(Modifier::DIM),
                )));
            }
        }
        lines.push(Line::from(""));
    }

    // Scroll position indicator
    let _total_lines = lines.len();
    if state.scroll_offset > 0 {
        lines.push(Line::from(vec![
            Span::styled(
                format!(" ↑ {} more lines above ", state.scroll_offset),
                Style::default().fg(Color::DarkGray),
            ),
        ]));
    }

    let text = Text::from(lines);

    let block = Block::default()
        .title(" Chat ")
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(Color::Blue));

    let paragraph = Paragraph::new(text)
        .block(block)
        .wrap(Wrap { trim: false })
        .scroll((state.scroll_offset as u16, 0));

    f.render_widget(paragraph, area);
}