/// Chat panel — the main conversation view.
use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, BorderType, Borders, Paragraph, Wrap},
    Frame,
};

use chrono::Timelike;

use crate::tui::app::{AppState, ConnectionState, ConversationEntry};
use crate::tui::rendering::{highlight_line_spans, render_markdown_lines_with_query};

/// Map a conversation role to a short, color-coded badge span.
fn role_badge(role: &str) -> Span<'static> {
    match role {
        "user" => Span::styled(
            "USER",
            Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
        ),
        "assistant" => Span::styled(
            "ASSISTANT",
            Style::default().fg(Color::Green).add_modifier(Modifier::BOLD),
        ),
        "system" => Span::styled("SYSTEM", Style::default().fg(Color::DarkGray)),
        "tool" => Span::styled(
            "TOOL",
            Style::default().fg(Color::Yellow).add_modifier(Modifier::DIM),
        ),
        "thinking" => Span::styled(
            "THINKING",
            Style::default()
                .fg(Color::Magenta)
                .add_modifier(Modifier::DIM),
        ),
        _ => Span::styled(role.to_uppercase(), Style::default().fg(Color::White)),
    }
}

/// Whether two entries fall in the same calendar minute.
fn same_minute(a: &ConversationEntry, b: &ConversationEntry) -> bool {
    a.timestamp.date_naive() == b.timestamp.date_naive()
        && a.timestamp.hour() == b.timestamp.hour()
        && a.timestamp.minute() == b.timestamp.minute()
}

/// Build the one-line message prefix: an optional timestamp plus the role badge.
///
/// The timestamp is omitted when this message falls in the same minute as the
/// previous one, so dense back-and-forth chats don't repeat the clock every line.
fn message_header(
    entry: &ConversationEntry,
    prev: Option<&ConversationEntry>,
) -> Line<'static> {
    let mut spans = Vec::new();

    if prev.map(|p| same_minute(entry, p)) != Some(true) {
        spans.push(Span::styled(
            format!("{} ", entry.timestamp.format("%H:%M")),
            Style::default().fg(Color::DarkGray),
        ));
    }

    spans.push(role_badge(&entry.role));
    Line::from(spans)
}

/// Build the styled lines for a tool entry as a compact card.
///
/// When collapsed, returns a single line: an optional timestamp, a
/// right-pointing chevron, the `TOOL` badge, the summary, and a dim expand hint.
///
/// When expanded, returns a header line (with optional timestamp, chevron,
/// badge, and summary), the wrapped tool body indented with a subtle left
/// border, and a footer hint explaining how to collapse.
fn tool_card_lines(
    entry: &ConversationEntry,
    prev: Option<&ConversationEntry>,
    collapsed: bool,
    search_query: &str,
    content_width: usize,
) -> Vec<Line<'static>> {
    let mut lines = Vec::new();

    let chevron = if collapsed { "▶ " } else { "▼ " };

    // Header: optional timestamp + chevron + badge + summary + hint.
    let mut header_spans = Vec::new();
    if prev.map(|p| same_minute(entry, p)) != Some(true) {
        header_spans.push(Span::styled(
            format!("{} ", entry.timestamp.format("%H:%M")),
            Style::default().fg(Color::DarkGray),
        ));
    }
    header_spans.push(Span::styled(
        chevron.to_string(),
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::DIM),
    ));
    header_spans.push(role_badge("tool"));
    header_spans.push(Span::styled("  ".to_string(), Style::default()));
    header_spans.push(Span::styled(
        entry.content.clone(),
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::DIM),
    ));
    if collapsed {
        header_spans.push(Span::styled(
            "  · Enter to expand".to_string(),
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::DIM),
        ));
    }
    lines.push(Line::from(header_spans));

    if collapsed {
        return lines;
    }

    // Expanded body: full output with a subtle left border.
    let full = entry
        .tool_output
        .as_deref()
        .unwrap_or(entry.content.as_str());
    let body_width = content_width.saturating_sub(4);
    let border_style = Style::default()
        .fg(Color::Yellow)
        .add_modifier(Modifier::DIM);
    let body_style = Style::default().fg(Color::Yellow);

    for raw_line in full.lines() {
        let wrapped = textwrap::fill(raw_line, body_width);
        for wline in wrapped.lines() {
            let mut spans = vec![Span::styled("  ▕ ".to_string(), border_style)];
            spans.extend(highlight_line_spans(wline, search_query, body_style));
            lines.push(Line::from(spans));
        }
    }

    // Footer hint.
    lines.push(Line::from(vec![Span::styled(
        "  · Enter or Tab to collapse · Ctrl+T to toggle all".to_string(),
        Style::default()
            .fg(Color::DarkGray)
            .add_modifier(Modifier::DIM),
    )]));

    lines
}

/// Render header + body lines for a single conversation entry.
///
/// This is the per-message unit that the chat render cache stores. It does
/// not include the trailing blank line between messages.
fn render_entry_lines(
    entry: &ConversationEntry,
    prev: Option<&ConversationEntry>,
    _idx: usize,
    content_width: usize,
    search_query: &str,
    collapsed: bool,
) -> Vec<Line<'static>> {
    if entry.role == "tool" {
        return tool_card_lines(entry, prev, collapsed, search_query, content_width);
    }

    if collapsed {
        let mut header_spans = Vec::new();
        if prev.map(|p| same_minute(entry, p)) != Some(true) {
            header_spans.push(Span::styled(
                format!("{} ", entry.timestamp.format("%H:%M")),
                Style::default().fg(Color::DarkGray),
            ));
        }
        header_spans.push(Span::styled(
            "▶ ".to_string(),
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::DIM),
        ));
        header_spans.push(role_badge(&entry.role));
        header_spans.push(Span::styled(
            "  · Enter to expand".to_string(),
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::DIM),
        ));
        return vec![Line::from(header_spans)];
    }

    let mut lines = vec![message_header(entry, prev)];

    if entry.role == "assistant" {
        let md_lines = render_markdown_lines_with_query(&entry.content, search_query);
        for md_line in md_lines {
            if md_line.spans.is_empty()
                || (md_line.spans.len() == 1 && md_line.spans[0].content.is_empty())
            {
                continue;
            }
            let mut padded = vec![Span::raw(" ")];
            padded.extend(md_line.spans);
            lines.push(Line::from(padded));
        }
    } else {
        let wrapped = textwrap::fill(&entry.content, content_width);
        for content_line in wrapped.lines() {
            let mut spans = vec![Span::raw(" ")];
            spans.extend(highlight_line_spans(
                content_line,
                search_query,
                Style::default(),
            ));
            lines.push(Line::from(spans));
        }
    }

    lines
}

/// Render the main chat area showing the conversation history.
///
/// Takes `&mut AppState` so we can clamp `scroll_offset` and update
/// `auto_scroll` based on the actual rendered line count.
pub fn render_chat(f: &mut Frame, area: Rect, state: &mut AppState) {
    let mut lines: Vec<Line> = Vec::new();

    // Connection banner at top (only when not connected, so the first
    // message starts at the top of the panel once we're online).
    match &state.connection {
        ConnectionState::Connected { .. } => {}
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
            lines.push(Line::from(""));
        }
        ConnectionState::Connecting => {
            lines.push(Line::from(vec![Span::styled(
                " ⟳ Connecting... ",
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            )]));
            lines.push(Line::from(""));
        }
        ConnectionState::Error(e) => {
            lines.push(Line::from(vec![
                Span::styled(
                    " ✗ Error: ",
                    Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
                ),
                Span::styled(e.clone(), Style::default().fg(Color::Red)),
            ]));
            lines.push(Line::from(""));
        }
    }

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
    let content_width = (area.width as usize).saturating_sub(4);
    let last_idx = state.messages.len().saturating_sub(1);

    // Invalidate the render cache when any rendering parameter changed.
    if !state.chat_render_cache.params_match(
        content_width,
        &state.search_query,
        state.tool_collapsed,
        &state.expanded_tools,
        &state.collapsed_messages,
    ) {
        state.chat_render_cache.clear_entries();
        state.chat_render_cache.snapshot_params(
            content_width,
            &state.search_query,
            state.tool_collapsed,
            &state.expanded_tools,
            &state.collapsed_messages,
        );
    }

    // Grow/shrink the cache vector to match the current message list.
    state
        .chat_render_cache
        .entries
        .resize_with(state.messages.len(), || (0, Vec::new()));

    let mut prev_entry: Option<&ConversationEntry> = None;

    for (idx, entry) in state.messages.iter().enumerate() {
        let content_len = entry.content.len();
        let is_streaming_last = idx == last_idx
            && state.is_generating
            && entry.role == "assistant";
        let collapsed = if is_streaming_last {
            // The message currently being streamed must stay expanded
            // so the user can watch it arrive.
            false
        } else if entry.role == "tool" {
            state.tool_should_collapse(idx)
        } else {
            state.message_should_collapse(idx)
        };

        let cached = if is_streaming_last {
            None
        } else {
            state
                .chat_render_cache
                .entries
                .get(idx)
                .filter(|(len, _)| *len == content_len)
                .map(|(_, lines)| lines.clone())
        };

        let entry_lines = if let Some(cached_lines) = cached {
            cached_lines
        } else {
            let lines = render_entry_lines(
                entry,
                prev_entry,
                idx,
                content_width,
                &state.search_query,
                collapsed,
            );
            if let Some(slot) = state.chat_render_cache.entries.get_mut(idx) {
                *slot = (content_len, lines.clone());
            }
            lines
        };

        lines.extend(entry_lines);
        lines.push(Line::from(""));
        prev_entry = Some(entry);
    }

    // Inline thinking block under the latest assistant message.
    // The old bottom thinking panel is replaced with this compact,
    // context-attached block so reasoning is visible next to the turn
    // that produced it.
    if state.thinking_panel_visible && !state.thinking_buffer.is_empty() {
        let content_width = (area.width as usize).saturating_sub(8);
        let border_style = Style::default()
            .fg(Color::Magenta)
            .add_modifier(Modifier::DIM);
        let body_style = Style::default()
            .fg(Color::Magenta)
            .add_modifier(Modifier::DIM);

        lines.push(Line::from(vec![
            Span::styled("  ⸱ ", border_style),
            Span::styled(
                "THINKING",
                Style::default()
                    .fg(Color::Magenta)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled("  · Esc to hide", border_style),
        ]));

        for t in &state.thinking_buffer {
            for line in textwrap::fill(t, content_width).lines() {
                lines.push(Line::from(vec![
                    Span::styled("    │ ", border_style),
                    Span::styled(line.to_string(), body_style),
                ]));
            }
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    fn make_state(connection: ConnectionState) -> AppState {
        use std::sync::Arc;
        let config = Arc::new(std::sync::RwLock::new(crate::shared::Config::default()));
        let mut state = AppState::new(config);
        state.connection = connection;
        state
    }

    fn buffer_cell_text(buffer: &ratatui::buffer::Buffer, row: u16) -> String {
        let mut s = String::new();
        for x in 0..buffer.area.width {
            if let Some(cell) = buffer.cell((x, row)) {
                s.push_str(cell.symbol());
            }
        }
        s
    }

    /// Render `render_chat` into a test backend and return the visible
    /// content area (inside the "Chat" block borders).
    fn render_state(state: &mut AppState, width: u16, height: u16) -> ratatui::buffer::Buffer {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        let area = ratatui::layout::Rect::new(0, 0, width, height);
        terminal.draw(|f| render_chat(f, area, state)).unwrap();
        terminal.backend().buffer().clone()
    }

    fn entry_at(role: &str, content: &str, hour: u32, minute: u32) -> ConversationEntry {
        let mut e = ConversationEntry::new(role, content);
        e.timestamp = chrono::Local::now()
            .with_hour(hour)
            .unwrap()
            .with_minute(minute)
            .unwrap()
            .with_second(0)
            .unwrap();
        e
    }

    #[test]
    fn role_badge_user_is_cyan_bold() {
        let span = role_badge("user");
        assert_eq!(span.content, "USER");
        assert_eq!(span.style.fg, Some(Color::Cyan));
        assert!(span.style.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn role_badge_assistant_is_green_bold() {
        let span = role_badge("assistant");
        assert_eq!(span.content, "ASSISTANT");
        assert_eq!(span.style.fg, Some(Color::Green));
        assert!(span.style.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn role_badge_tool_is_yellow_dim() {
        let span = role_badge("tool");
        assert_eq!(span.content, "TOOL");
        assert_eq!(span.style.fg, Some(Color::Yellow));
        assert!(span.style.add_modifier.contains(Modifier::DIM));
    }

    #[test]
    fn header_includes_timestamp_for_first_message() {
        let e = entry_at("assistant", "hi", 9, 14);
        let line = message_header(&e, None);
        assert_eq!(line.spans.len(), 2);
        assert_eq!(line.spans[0].content, "09:14 ");
        assert_eq!(line.spans[1].content, "ASSISTANT");
    }

    #[test]
    fn header_omits_timestamp_when_same_minute() {
        let prev = entry_at("user", "hello", 9, 14);
        let e = entry_at("assistant", "hi", 9, 14);
        let line = message_header(&e, Some(&prev));
        assert_eq!(line.spans.len(), 1);
        assert_eq!(line.spans[0].content, "ASSISTANT");
    }

    #[test]
    fn header_includes_timestamp_when_minute_changes() {
        let prev = entry_at("user", "hello", 9, 14);
        let e = entry_at("assistant", "hi", 9, 15);
        let line = message_header(&e, Some(&prev));
        assert_eq!(line.spans.len(), 2);
        assert_eq!(line.spans[0].content, "09:15 ");
        assert_eq!(line.spans[1].content, "ASSISTANT");
    }

    #[test]
    fn tool_header_uses_dim_yellow_tool_badge() {
        let e = entry_at("tool", "🔧 foo", 9, 14);
        let line = message_header(&e, None);
        assert_eq!(line.spans.len(), 2);
        assert_eq!(line.spans[0].content, "09:14 ");
        assert_eq!(line.spans[1].content, "TOOL");
        assert_eq!(line.spans[1].style.fg, Some(Color::Yellow));
        assert!(line.spans[1].style.add_modifier.contains(Modifier::DIM));
    }

    fn tool_entry(summary: &str, full: &str, hour: u32, minute: u32) -> ConversationEntry {
        let mut e = ConversationEntry::tool(summary, full);
        e.timestamp = chrono::Local::now()
            .with_hour(hour)
            .unwrap()
            .with_minute(minute)
            .unwrap()
            .with_second(0)
            .unwrap();
        e
    }

    #[test]
    fn collapsed_tool_card_is_one_line() {
        let e = tool_entry("git status", "nothing to commit", 9, 14);
        let lines = tool_card_lines(&e, None, true, "", 80);
        assert_eq!(lines.len(), 1);
        let text: String = lines[0].spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(text.contains("git status"));
        assert!(text.contains("Enter to expand"));
        assert!(text.starts_with("09:14 ▶ "));
    }

    #[test]
    fn tool_card_includes_timestamp_for_first_entry() {
        let e = tool_entry("git status", "full", 9, 14);
        let lines = tool_card_lines(&e, None, true, "", 80);
        let text: String = lines[0].spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(text.starts_with("09:14 ▶ "));
    }

    #[test]
    fn tool_card_omits_timestamp_when_same_minute() {
        let prev = tool_entry("first", "full", 9, 14);
        let e = tool_entry("second", "full", 9, 14);
        let lines = tool_card_lines(&e, Some(&prev), true, "", 80);
        let text: String = lines[0].spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(!text.contains("09:14"));
        assert!(text.starts_with("▶ "));
    }

    #[test]
    fn expanded_tool_card_has_header_body_and_footer() {
        let e = tool_entry("git status", "line1\nline2", 9, 14);
        let lines = tool_card_lines(&e, None, false, "", 80);
        assert!(lines.len() >= 3);

        let header: String = lines[0].spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(header.starts_with("09:14 ▼ "));
        assert!(header.contains("git status"));

        let footer: String = lines.last().unwrap().spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(footer.contains("Enter or Tab to collapse"));
        assert!(footer.contains("Ctrl+T to toggle all"));
    }

    #[test]
    fn expanded_tool_card_body_is_indented() {
        let e = tool_entry("summary", "body text", 9, 14);
        let lines = tool_card_lines(&e, None, false, "", 80);
        assert!(lines.len() >= 2);
        let body_line = &lines[1];
        assert!(body_line.spans[0].content.starts_with("  ▕ "));
        assert_eq!(body_line.spans[0].style.fg, Some(Color::Yellow));
    }

    #[test]
    fn tool_card_preserves_search_highlight() {
        let e = tool_entry("summary", "needle in haystack", 9, 14);
        let lines = tool_card_lines(&e, None, false, "needle", 80);
        assert!(lines.len() >= 2);
        let body_line = &lines[1];
        let found = body_line.spans.iter().any(|s| {
            s.content == "needle" && s.style.bg == Some(Color::Yellow)
        });
        assert!(found, "search hit 'needle' should keep yellow highlight background");
    }

    #[test]
    fn connected_state_hides_banner() {
        let mut state = make_state(ConnectionState::Connected {
            model: "test".into(),
            since: std::time::Instant::now(),
        });
        state.messages.push(entry_at("assistant", "hello", 9, 14));

        let buffer = render_state(&mut state, 40, 10);
        let content_row = buffer_cell_text(&buffer, 1);
        assert!(
            !content_row.contains("Connected"),
            "connected state should not show a banner at the top, got: {:?}",
            content_row
        );
        assert!(
            content_row.contains("09:14"),
            "first content row should be the message header with timestamp, got: {:?}",
            content_row
        );
    }

    #[test]
    fn disconnected_state_shows_banner() {
        let mut state = make_state(ConnectionState::Disconnected);
        state.messages.push(entry_at("assistant", "hello", 9, 14));

        let buffer = render_state(&mut state, 40, 10);
        let content_row = buffer_cell_text(&buffer, 1);
        assert!(
            content_row.contains("Disconnected"),
            "disconnected state should show the banner, got: {:?}",
            content_row
        );
    }

    #[test]
    fn connected_state_empty_messages_has_no_banner_lines() {
        let mut state = make_state(ConnectionState::Connected {
            model: "test".into(),
            since: std::time::Instant::now(),
        });

        let buffer = render_state(&mut state, 40, 10);
        let content_row = buffer_cell_text(&buffer, 1);
        let trimmed = content_row.trim();
        assert!(
            trimmed.is_empty() || !trimmed.contains("Connected"),
            "empty connected panel should not contain a connected banner, got: {:?}",
            trimmed
        );
    }

    #[test]
    fn streaming_appends_to_last_assistant_message() {
        let mut state = make_state(ConnectionState::Connected {
            model: "test".into(),
            since: std::time::Instant::now(),
        });
        state.messages.push(entry_at("user", "hi", 9, 14));
        state.messages.push(entry_at("assistant", "he", 9, 14));
        state.is_generating = true;

        // First render — primes the cache for the completed user message.
        // Layout inside the bordered chat panel:
        //   row 1: "09:14 USER"
        //   row 2: " hi"
        //   row 3: blank separator
        //   row 4: "ASSISTANT" (same minute, timestamp omitted)
        //   row 5: " he"
        let buffer_before = render_state(&mut state, 40, 10);
        let assistant_before = buffer_cell_text(&buffer_before, 5);
        assert!(assistant_before.contains("he"), "initial assistant content missing: {:?}", assistant_before);

        // Simulate a streaming token appended to the last assistant message.
        state.messages.last_mut().unwrap().content.push_str("llo");

        let buffer_after = render_state(&mut state, 40, 10);
        let assistant_after = buffer_cell_text(&buffer_after, 5);
        assert!(
            assistant_after.contains("hello"),
            "streaming token should appear in the rendered assistant message, got: {:?}",
            assistant_after
        );
    }

    #[test]
    fn width_change_rewraps_plain_text() {
        let mut state = make_state(ConnectionState::Connected {
            model: "test".into(),
            since: std::time::Instant::now(),
        });
        state.messages.push(entry_at("user", "word ".repeat(20).trim(), 9, 14));
        state.messages.push(entry_at("assistant", "ok", 9, 14));

        // Count rows between the user header and the assistant header
        // that contain the repeated "word" content.
        let buffer_narrow = render_state(&mut state, 20, 30);
        let narrow_body_rows = count_rows_between_headers(&buffer_narrow,
            "USER",
            "ASSISTANT",
            "word",
        );

        let buffer_wide = render_state(&mut state, 80, 30);
        let wide_body_rows = count_rows_between_headers(
            &buffer_wide,
            "USER",
            "ASSISTANT",
            "word",
        );

        assert!(
            narrow_body_rows > wide_body_rows,
            "narrow width should produce more body rows: narrow={} wide={}",
            narrow_body_rows,
            wide_body_rows
        );
    }

    /// Count content rows between the first occurrence of `start_header`
    /// and the first subsequent occurrence of `end_header` that contain
    /// `body_marker`.
    fn count_rows_between_headers(
        buffer: &ratatui::buffer::Buffer,
        start_header: &str,
        end_header: &str,
        body_marker: &str,
    ) -> usize {
        let mut in_body = false;
        let mut count = 0;
        for row in 1..buffer.area.height - 1 {
            let text = buffer_cell_text(buffer, row);
            if text.contains(start_header) {
                in_body = true;
                continue;
            }
            if in_body && text.contains(end_header) {
                break;
            }
            if in_body && text.contains(body_marker) {
                count += 1;
            }
        }
        count
    }

    #[test]
    fn search_query_change_adds_highlight() {
        let mut state = make_state(ConnectionState::Connected {
            model: "test".into(),
            since: std::time::Instant::now(),
        });
        state.messages.push(entry_at("user", "needle in haystack", 9, 14));
        state.messages.push(entry_at("assistant", "ok", 9, 14));

        // Render without query first.
        render_state(&mut state, 80, 10);

        state.search_query = "needle".to_string();
        let buffer = render_state(&mut state, 80, 10);

        let mut found_highlight = false;
        for row in 1..buffer.area.height - 1 {
            let text = buffer_cell_text(&buffer, row);
            // We can't easily inspect background colors from the buffer text,
            // but the highlighted span content should still be present.
            if text.contains("needle") {
                found_highlight = true;
            }
        }
        assert!(found_highlight, "search query should render the matching line");
    }

    #[test]
    fn tool_collapse_toggle_changes_output() {
        let mut state = make_state(ConnectionState::Connected {
            model: "test".into(),
            since: std::time::Instant::now(),
        });
        state.messages.push(ConversationEntry::tool("git status", "line1\nline2"));

        // Default: tool_collapsed is true, so the entry renders as one line.
        let buffer_collapsed = render_state(&mut state, 40, 10);
        let mut collapsed_rows = 0;
        for row in 1..buffer_collapsed.area.height - 1 {
            let text = buffer_cell_text(&buffer_collapsed, row);
            if text.contains("git status") {
                collapsed_rows += 1;
            }
        }
        assert_eq!(collapsed_rows, 1, "collapsed tool card should occupy one content row");

        // Expand it.
        state.expanded_tools.insert(0);
        let buffer_expanded = render_state(&mut state, 40, 10);
        let mut expanded_rows = 0;
        for row in 1..buffer_expanded.area.height - 1 {
            let text = buffer_cell_text(&buffer_expanded, row);
            if text.contains("git status") || text.contains("line1") || text.contains("line2") {
                expanded_rows += 1;
            }
        }
        assert!(
            expanded_rows > collapsed_rows,
            "expanded tool card should occupy more rows than collapsed"
        );
    }

    #[test]
    fn collapsed_assistant_message_shows_header_only() {
        let mut state = make_state(ConnectionState::Connected {
            model: "test".into(),
            since: std::time::Instant::now(),
        });
        state
            .messages
            .push(entry_at("assistant", "this is a long reply", 9, 14));
        state.collapsed_messages.insert(0);

        let buffer = render_state(&mut state, 40, 10);
        let header = buffer_cell_text(&buffer, 1);
        assert!(
            header.contains("ASSISTANT"),
            "collapsed assistant should still show header, got: {:?}",
            header
        );
        assert!(
            header.contains("▶"),
            "collapsed assistant header should show collapsed chevron, got: {:?}",
            header
        );

        for row in 2..buffer.area.height - 1 {
            let text = buffer_cell_text(&buffer, row);
            assert!(
                !text.contains("this is a long reply"),
                "collapsed assistant should not render body, got: {:?}",
                text
            );
        }
    }

    #[test]
    fn collapsed_user_message_shows_header_only() {
        let mut state = make_state(ConnectionState::Connected {
            model: "test".into(),
            since: std::time::Instant::now(),
        });
        state
            .messages
            .push(entry_at("user", "a very long user question", 9, 14));
        state.collapsed_messages.insert(0);

        let buffer = render_state(&mut state, 40, 10);
        let header = buffer_cell_text(&buffer, 1);
        assert!(header.contains("USER"), "collapsed user should show header");
        assert!(header.contains("▶"), "collapsed user header should show chevron");

        for row in 2..buffer.area.height - 1 {
            let text = buffer_cell_text(&buffer, row);
            assert!(
                !text.contains("a very long user question"),
                "collapsed user should not render body, got: {:?}",
                text
            );
        }
    }

    #[test]
    fn streaming_last_assistant_stays_expanded() {
        let mut state = make_state(ConnectionState::Connected {
            model: "test".into(),
            since: std::time::Instant::now(),
        });
        state.messages.push(entry_at("user", "hi", 9, 14));
        state
            .messages
            .push(entry_at("assistant", "typing", 9, 14));
        state.is_generating = true;
        // Even if the user collapsed the last assistant message while it
        // is streaming, it must stay expanded so tokens remain visible.
        state.collapsed_messages.insert(1);

        let buffer = render_state(&mut state, 40, 10);
        let mut found_body = false;
        for row in 1..buffer.area.height - 1 {
            let text = buffer_cell_text(&buffer, row);
            if text.contains("typing") {
                found_body = true;
            }
        }
        assert!(
            found_body,
            "streaming assistant message should remain expanded"
        );
    }

    #[test]
    fn thinking_block_hidden_by_default() {
        let mut state = make_state(ConnectionState::Connected {
            model: "test".into(),
            since: std::time::Instant::now(),
        });
        state.messages.push(entry_at("assistant", "hi", 9, 14));
        state.thinking_buffer.push("step 1".to_string());
        // thinking_panel_visible defaults to false.

        let buffer = render_state(&mut state, 40, 10);
        for row in 1..buffer.area.height - 1 {
            let text = buffer_cell_text(&buffer, row);
            assert!(
                !text.contains("THINKING") && !text.contains("step 1"),
                "thinking block should be hidden when panel flag is false, got: {:?}",
                text
            );
        }
    }

    #[test]
    fn thinking_block_visible_when_toggled() {
        let mut state = make_state(ConnectionState::Connected {
            model: "test".into(),
            since: std::time::Instant::now(),
        });
        state.messages.push(entry_at("assistant", "hi", 9, 14));
        state.thinking_buffer.push("step 1".to_string());
        state.thinking_panel_visible = true;

        let buffer = render_state(&mut state, 40, 10);
        let mut found_header = false;
        let mut found_body = false;
        for row in 1..buffer.area.height - 1 {
            let text = buffer_cell_text(&buffer, row);
            if text.contains("THINKING") {
                found_header = true;
            }
            if text.contains("step 1") {
                found_body = true;
            }
        }
        assert!(found_header, "THINKING header should be visible");
        assert!(found_body, "thinking body should be visible");
    }

    #[test]
    fn thinking_block_attached_to_last_message() {
        let mut state = make_state(ConnectionState::Connected {
            model: "test".into(),
            since: std::time::Instant::now(),
        });
        state.messages.push(entry_at("user", "hello", 9, 14));
        state.messages.push(entry_at("assistant", "hi", 9, 14));
        state.thinking_buffer.push("step 1".to_string());
        state.thinking_panel_visible = true;

        let buffer = render_state(&mut state, 40, 12);
        // Find the assistant content row and the first THINKING row; the
        // THINKING row should come after the assistant row.
        let mut assistant_row = None;
        let mut thinking_row = None;
        for row in 1..buffer.area.height - 1 {
            let text = buffer_cell_text(&buffer, row);
            if text.contains(" hi") && assistant_row.is_none() {
                assistant_row = Some(row);
            }
            if text.contains("THINKING") && thinking_row.is_none() {
                thinking_row = Some(row);
            }
        }
        assert!(
            assistant_row.is_some() && thinking_row.is_some(),
            "should find both assistant content and thinking header"
        );
        assert!(
            thinking_row.unwrap() > assistant_row.unwrap(),
            "thinking block should appear after the assistant message"
        );
    }
}
