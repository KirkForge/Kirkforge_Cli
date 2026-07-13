/// Chat panel — the main conversation view.
use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, BorderType, Borders, Paragraph, Wrap},
    Frame,
};

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use crate::tui::app::{AppState, ConnectionState, ConversationEntry};

mod lines;
use lines::{build_chat_lines, progress_line, render_entry_lines};

/// Compute a scroll offset that shows the current search match.
///
/// - If the current match is inside a collapsed tool entry's
///   `tool_output`, the entry is expanded first.
/// - Returns `Some(line_index)` to scroll to, or `None` if there are
///   no committed matches or the match message no longer exists.
///
/// The returned offset is a raw line index into the chat content area,
/// before the "more lines below" footer is added. The caller should
/// clamp it against `max_scroll` before assigning to `scroll_offset`.
pub fn scroll_offset_for_search_match(state: &mut AppState, content_width: usize) -> Option<usize> {
    let (msg_idx, _byte_offset, source) = state.search_matches.get(state.search_match_idx)?;
    let msg_idx = *msg_idx;
    if msg_idx >= state.messages.len() {
        return None;
    }

    // Expand the tool card when the match lives in the hidden body.
    if *source == crate::tui::search::SearchSource::ToolOutput {
        let entry = &state.messages[msg_idx];
        if entry.role == "tool" && entry.tool_output.is_some() {
            state.expanded_tools.insert(msg_idx);
        }
    }

    let (_lines, message_start) = build_chat_lines(state, content_width);
    let mut target = message_start.get(msg_idx).copied()?;

    // For tool-output matches, skip the header line so the body
    // (where the highlighted text is) is visible instead of just the
    // collapsed summary.
    if *source == crate::tui::search::SearchSource::ToolOutput {
        target += 1;
    }
    Some(target)
}

/// Render the main chat area showing the conversation history.
///
/// Takes `&mut AppState` so we can clamp `scroll_offset` and update
/// `auto_scroll` based on the actual rendered line count.
pub fn render_chat(f: &mut Frame, area: Rect, state: &mut AppState) {
    let mut lines: Vec<Line> = Vec::new();

    // Sandbox posture banner (always visible if unsandboxed).
    if state.unsandboxed {
        lines.push(Line::from(vec![
            Span::styled(
                " ⚠️  Unsandboxed ",
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                "PathGuard: model writes are not restricted to any directory tree.",
                Style::default().fg(Color::Yellow),
            ),
        ]));
        lines.push(Line::from(""));
    }

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

    // Pull-progress bar (gap #22): rendered above the conversation so
    // it is always visible while a model is being downloaded.
    if let Some(ref p) = state.pull_progress {
        lines.push(progress_line(p));
        lines.push(Line::from(""));
    }

    // Conversation messages
    let content_width = (area.width as usize).saturating_sub(4);
    state.last_content_width = content_width;
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
        let content_hash = {
            let mut hasher = DefaultHasher::new();
            (&entry.content, &entry.tool_output).hash(&mut hasher);
            hasher.finish()
        };
        let is_streaming_last = idx == last_idx && state.is_generating && entry.role == "assistant";
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
                .filter(|(len, _)| *len == content_hash)
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
                *slot = (content_hash, lines.clone());
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
                " ↓ {lines_remaining} more lines below (↓/PgDn to scroll, ↑/PgUp for history) "
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
    use super::lines::{message_header, role_badge, tool_card_lines};
    use super::*;
    use chrono::Timelike;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    fn make_state(connection: ConnectionState) -> AppState {
        use std::sync::Arc;
        let config = Arc::new(std::sync::RwLock::new(crate::shared::Config::default()));
        let mut state = AppState::new(config);
        state.connection = connection;
        state.unsandboxed = false;
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

        let footer: String = lines
            .last()
            .unwrap()
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect();
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
        let found = body_line
            .spans
            .iter()
            .any(|s| s.content == "needle" && s.style.bg == Some(Color::Yellow));
        assert!(
            found,
            "search hit 'needle' should keep yellow highlight background"
        );
    }

    #[test]
    fn search_match_in_tool_output_expands_and_scrolls() {
        let mut state = make_state(ConnectionState::Connected {
            model: "test".into(),
            since: std::time::Instant::now(),
        });
        state.tool_collapsed = true;
        state.last_content_width = 80;
        state
            .messages
            .push(entry_at("assistant", "check the tool output", 9, 14));
        state
            .messages
            .push(tool_entry("tool summary", "hidden needle value", 9, 14));
        state.search_query = "needle".into();
        state.search_matches = crate::tui::search::compute_matches(&state.messages, "needle");
        state.search_match_idx = 0;

        let offset = scroll_offset_for_search_match(&mut state, 80).expect("match exists");

        assert!(
            state.expanded_tools.contains(&1),
            "tool card at message 1 should be expanded for a ToolOutput match"
        );
        // The assistant message contributes a header + wrapped body + blank,
        // then the tool entry header is after that. The returned offset should
        // point at the tool body, not the collapsed summary header.
        assert!(
            offset > 0,
            "scroll offset should be past the assistant message"
        );
    }

    #[test]
    fn search_match_in_content_does_not_expand_tool() {
        let mut state = make_state(ConnectionState::Connected {
            model: "test".into(),
            since: std::time::Instant::now(),
        });
        state.tool_collapsed = true;
        state.last_content_width = 80;
        state
            .messages
            .push(tool_entry("needle summary", "hidden body", 9, 14));
        state.search_query = "needle".into();
        state.search_matches = crate::tui::search::compute_matches(&state.messages, "needle");
        state.search_match_idx = 0;

        let offset = scroll_offset_for_search_match(&mut state, 80).expect("match exists");

        assert!(
            !state.expanded_tools.contains(&0),
            "content match should not force tool expansion"
        );
        assert_eq!(offset, 0, "first message starts at line 0 without banners");
    }

    #[test]
    fn scroll_offset_for_search_match_returns_none_when_empty() {
        let mut state = make_state(ConnectionState::Connected {
            model: "test".into(),
            since: std::time::Instant::now(),
        });
        state.last_content_width = 80;
        state.search_matches.clear();
        state.search_match_idx = 0;
        assert_eq!(scroll_offset_for_search_match(&mut state, 80), None);
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
            "connected state should not show a banner at the top, got: {content_row:?}"
        );
        assert!(
            content_row.contains("09:14"),
            "first content row should be the message header with timestamp, got: {content_row:?}"
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
            "disconnected state should show the banner, got: {content_row:?}"
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
            "empty connected panel should not contain a connected banner, got: {trimmed:?}"
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
        assert!(
            assistant_before.contains("he"),
            "initial assistant content missing: {assistant_before:?}"
        );

        // Simulate a streaming token appended to the last assistant message.
        state.messages.last_mut().unwrap().content.push_str("llo");

        let buffer_after = render_state(&mut state, 40, 10);
        let assistant_after = buffer_cell_text(&buffer_after, 5);
        assert!(
            assistant_after.contains("hello"),
            "streaming token should appear in the rendered assistant message, got: {assistant_after:?}"
        );
    }

    #[test]
    fn width_change_rewraps_plain_text() {
        let mut state = make_state(ConnectionState::Connected {
            model: "test".into(),
            since: std::time::Instant::now(),
        });
        state
            .messages
            .push(entry_at("user", "word ".repeat(20).trim(), 9, 14));
        state.messages.push(entry_at("assistant", "ok", 9, 14));

        // Count rows between the user header and the assistant header
        // that contain the repeated "word" content.
        let buffer_narrow = render_state(&mut state, 20, 30);
        let narrow_body_rows =
            count_rows_between_headers(&buffer_narrow, "USER", "ASSISTANT", "word");

        let buffer_wide = render_state(&mut state, 80, 30);
        let wide_body_rows = count_rows_between_headers(&buffer_wide, "USER", "ASSISTANT", "word");

        assert!(
            narrow_body_rows > wide_body_rows,
            "narrow width should produce more body rows: narrow={narrow_body_rows} wide={wide_body_rows}"
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
    fn progress_bar_renders_percentage_and_bar() {
        let mut state = make_state(ConnectionState::Connected {
            model: "test".into(),
            since: std::time::Instant::now(),
        });
        state.pull_progress = Some(crate::tui::app::PullProgress {
            status: "downloading".into(),
            completed: Some(128 * 1024 * 1024),
            total: Some(512 * 1024 * 1024),
        });

        let buffer = render_state(&mut state, 80, 8);
        let top = buffer_cell_text(&buffer, 1);
        assert!(
            top.contains("downloading"),
            "progress line should contain status, got: {top:?}"
        );
        assert!(
            top.contains("25%"),
            "progress line should contain 25% label, got: {top:?}"
        );
        assert!(
            top.contains("128.0/512.0 MB"),
            "progress line should contain MB label, got: {top:?}"
        );
        assert!(
            top.contains('█'),
            "progress line should contain filled bar characters, got: {top:?}"
        );
        assert!(
            top.contains('░'),
            "progress line should contain empty bar characters, got: {top:?}"
        );
    }

    #[test]
    fn progress_bar_renders_status_when_total_unknown() {
        let mut state = make_state(ConnectionState::Connected {
            model: "test".into(),
            since: std::time::Instant::now(),
        });
        state.pull_progress = Some(crate::tui::app::PullProgress {
            status: "pulling manifest".into(),
            completed: None,
            total: None,
        });

        let buffer = render_state(&mut state, 80, 8);
        let top = buffer_cell_text(&buffer, 1);
        assert!(
            top.contains("pulling manifest"),
            "progress line should show status when total unknown, got: {top:?}"
        );
    }

    #[test]
    fn search_query_change_adds_highlight() {
        let mut state = make_state(ConnectionState::Connected {
            model: "test".into(),
            since: std::time::Instant::now(),
        });
        state
            .messages
            .push(entry_at("user", "needle in haystack", 9, 14));
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
        assert!(
            found_highlight,
            "search query should render the matching line"
        );
    }

    #[test]
    fn tool_collapse_toggle_changes_output() {
        let mut state = make_state(ConnectionState::Connected {
            model: "test".into(),
            since: std::time::Instant::now(),
        });
        state
            .messages
            .push(ConversationEntry::tool("git status", "line1\nline2"));

        // Default: tool_collapsed is true, so the entry renders as one line.
        let buffer_collapsed = render_state(&mut state, 40, 10);
        let mut collapsed_rows = 0;
        for row in 1..buffer_collapsed.area.height - 1 {
            let text = buffer_cell_text(&buffer_collapsed, row);
            if text.contains("git status") {
                collapsed_rows += 1;
            }
        }
        assert_eq!(
            collapsed_rows, 1,
            "collapsed tool card should occupy one content row"
        );

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
            "collapsed assistant should still show header, got: {header:?}"
        );
        assert!(
            header.contains("▶"),
            "collapsed assistant header should show collapsed chevron, got: {header:?}"
        );

        for row in 2..buffer.area.height - 1 {
            let text = buffer_cell_text(&buffer, row);
            assert!(
                !text.contains("this is a long reply"),
                "collapsed assistant should not render body, got: {text:?}"
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
        assert!(
            header.contains("▶"),
            "collapsed user header should show chevron"
        );

        for row in 2..buffer.area.height - 1 {
            let text = buffer_cell_text(&buffer, row);
            assert!(
                !text.contains("a very long user question"),
                "collapsed user should not render body, got: {text:?}"
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
        state.messages.push(entry_at("assistant", "typing", 9, 14));
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
                "thinking block should be hidden when panel flag is false, got: {text:?}"
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
