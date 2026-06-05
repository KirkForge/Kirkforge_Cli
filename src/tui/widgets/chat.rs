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
///
/// Takes `&mut AppState` so we can clamp `scroll_offset` and update
/// `auto_scroll` based on the actual rendered line count.
pub fn render_chat(f: &mut Frame, area: Rect, state: &mut AppState) {
    let mut lines: Vec<Line> = Vec::new();

    // Connection banner at top
    match &state.connection {
        ConnectionState::Disconnected => {
            lines.push(Line::from(vec![
                Span::styled(
                    " ⚡ Disconnected ",
                    Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    "Press Enter to start a session, or type /connect <model>",
                    Style::default().fg(Color::DarkGray),
                ),
            ]));
        }
        ConnectionState::Connecting => {
            lines.push(Line::from(vec![Span::styled(
                " ⟳ Connecting... ",
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            )]));
        }
        ConnectionState::Connected { model, .. } => {
            lines.push(Line::from(vec![
                Span::styled(" ◆ ", Style::default().fg(Color::Green)),
                Span::styled("Connected: ", Style::default().fg(Color::DarkGray)),
                Span::styled(
                    model.clone(),
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ),
            ]));
        }
        ConnectionState::Error(e) => {
            lines.push(Line::from(vec![
                Span::styled(
                    " ✗ Error: ",
                    Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
                ),
                Span::styled(e.clone(), Style::default().fg(Color::Red)),
            ]));
        }
    }

    // Empty line after banner
    lines.push(Line::from(""));

    // Loading indicator: show a dim spinner when waiting for first token
    if state.is_generating && state.messages.last().map(|m| m.role.as_str()) != Some("assistant") {
        lines.push(Line::from(vec![Span::styled(
            format!(" ⏳ {} ", state.spinner_char()),
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::DIM),
        )]));
        lines.push(Line::from(""));
    }

    // Conversation messages
    for (idx, entry) in state.messages.iter().enumerate() {
        let role_style = match entry.role.as_str() {
            "user" => Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
            "assistant" => Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
            "system" => Style::default().fg(Color::DarkGray),
            "tool" => Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::DIM),
            "thinking" => Style::default()
                .fg(Color::Magenta)
                .add_modifier(Modifier::DIM),
            _ => Style::default().fg(Color::White),
        };

        let time_str = entry.timestamp.format("%H:%M:%S").to_string();
        let content_width = (area.width as usize).saturating_sub(4);

        // ── Tool entries: collapse vs expand ─────────────────
        // When collapsed (default), show only the summary line in a
        // dim yellow box. When expanded, show the full stored output.
        if entry.role == "tool" {
            if state.tool_should_collapse(idx) {
                // Collapsed: one-line summary in a subtle box
                let expanded_marker = if state.tool_collapsed {
                    ""
                } else {
                    " (showing all)"
                };
                lines.push(Line::from(vec![
                    Span::styled(
                        format!(" {} ", time_str),
                        Style::default().fg(Color::DarkGray),
                    ),
                    Span::styled(
                        "tool",
                        Style::default()
                            .fg(Color::Yellow)
                            .add_modifier(Modifier::DIM),
                    ),
                ]));
                lines.push(Line::from(vec![Span::styled(
                    format!("  ┌─ {}{} ", entry.content, expanded_marker),
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::DIM),
                )]));
                lines.push(Line::from(vec![Span::styled(
                    "  └─ [Enter or Tab to expand, Ctrl+T to toggle all]",
                    Style::default()
                        .fg(Color::DarkGray)
                        .add_modifier(Modifier::DIM),
                )]));
                lines.push(Line::from(""));
                continue;
            }
            // Expanded: render the full tool output (from sidecar if present)
            let full = entry
                .tool_output
                .as_deref()
                .unwrap_or(entry.content.as_str());
            lines.push(Line::from(vec![
                Span::styled(
                    format!(" {} ", time_str),
                    Style::default().fg(Color::DarkGray),
                ),
                Span::styled(
                    "tool",
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    " (expanded — Enter or Tab to collapse)",
                    Style::default()
                        .fg(Color::DarkGray)
                        .add_modifier(Modifier::DIM),
                ),
            ]));
            // Open box
            lines.push(Line::from(vec![Span::styled(
                "  ┌─",
                Style::default().fg(Color::Yellow),
            )]));
            for content_line in full.lines() {
                let wrapped = textwrap::fill(content_line, content_width.saturating_sub(4));
                for wline in wrapped.lines() {
                    lines.push(Line::from(vec![Span::styled(
                        format!("  │ {}", wline),
                        Style::default()
                            .fg(Color::Yellow)
                            .add_modifier(Modifier::DIM),
                    )]));
                }
            }
            // Close box
            lines.push(Line::from(vec![Span::styled(
                "  └─",
                Style::default().fg(Color::Yellow),
            )]));
            lines.push(Line::from(""));
            continue;
        }

        lines.push(Line::from(vec![
            Span::styled(
                format!(" {} ", time_str),
                Style::default().fg(Color::DarkGray),
            ),
            Span::styled(entry.role.to_string(), role_style),
        ]));

        // Assistant messages get markdown rendering; everything else is plain text
        if entry.role == "assistant" {
            // Render markdown into ratatui styled Lines (bold, code, code blocks)
            let md_lines = crate::tui::rendering::render_markdown_lines(&entry.content);
            for md_line in md_lines {
                if md_line.spans.is_empty()
                    || (md_line.spans.len() == 1 && md_line.spans[0].content.is_empty())
                {
                    continue;
                }
                // Prepend a space for padding
                let mut padded = vec![Span::raw(" ")];
                padded.extend(md_line.spans);
                lines.push(Line::from(padded));
            }
        } else {
            // Plain text wrapping for user/system/thinking messages
            let wrapped = textwrap::fill(&entry.content, content_width);
            for content_line in wrapped.lines() {
                lines.push(Line::from(Span::styled(
                    format!(" {}", content_line),
                    Style::default(),
                )));
            }
        }
        lines.push(Line::from(""));
    }

    // Thinking panel (collapsible)
    if state.thinking_panel_visible && !state.thinking_buffer.is_empty() {
        lines.push(Line::from(vec![Span::styled(
            " ── Thinking ──",
            Style::default()
                .fg(Color::Magenta)
                .add_modifier(Modifier::DIM),
        )]));
        for t in &state.thinking_buffer {
            let content_width = (area.width as usize).saturating_sub(4);
            for line in textwrap::fill(t, content_width).lines() {
                lines.push(Line::from(Span::styled(
                    format!(" {}", line),
                    Style::default()
                        .fg(Color::Magenta)
                        .add_modifier(Modifier::DIM),
                )));
            }
        }
        lines.push(Line::from(""));
    }

    // ── Compute scroll geometry ─────────────────────────────
    let visible_height = (area.height as usize).saturating_sub(3);
    let total_lines = lines.len();
    let max_scroll = total_lines.saturating_sub(visible_height);

    // Publish max_scroll to AppState so key handlers (PgUp/PgDn) can
    // clamp immediately without waiting for the next render.
    state.max_scroll = max_scroll;

    // Auto-scroll: if enabled, pin to the bottom (latest messages).
    if state.auto_scroll {
        state.scroll_offset = max_scroll;
    } else if state.scroll_offset >= max_scroll {
        // User scrolled all the way back to the bottom — re-enable auto-scroll
        state.auto_scroll = true;
        state.scroll_offset = max_scroll;
    }

    // Clamp: if content shrunk (e.g. cleared), snap back
    if state.scroll_offset > max_scroll {
        state.scroll_offset = max_scroll;
    }

    // Only show scroll indicator when content is hidden below the viewport
    let lines_remaining = max_scroll.saturating_sub(state.scroll_offset);
    if lines_remaining > 0 {
        lines.push(Line::from(vec![Span::styled(
            format!(
                " ↓ {} more lines below (↓/PgDn to scroll, ↑/PgUp for history) ",
                lines_remaining
            ),
            Style::default().fg(Color::DarkGray),
        )]));
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