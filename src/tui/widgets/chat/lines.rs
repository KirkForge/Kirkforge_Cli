//! Stateless line builders for the chat panel.
//!
//! Pure functions that turn `ConversationEntry`s into ratatui `Line`s
//! (role badges, message headers, tool cards, per-entry bodies, and the
//! full uncached line list). Extracted from `render_chat` so the widget
//! module is just cache + scroll geometry + framing, while this is the
//! "render the lines" half. Everything here is private to the `chat`
//! module.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use chrono::Timelike;

use crate::tui::app::{AppState, ConnectionState, ConversationEntry};
use crate::tui::rendering::{highlight_line_spans, render_markdown_lines_with_query};
use crate::tui::theme::Theme;

/// Check if a tool entry at `idx` is currently streaming (PTY tool).
fn idx_is_streaming_tool(state: &AppState, idx: usize, _last_idx: usize) -> bool {
    state
        .conversation
        .messages
        .get(idx)
        .map(|m| m.role == "tool" && m.streaming)
        .unwrap_or(false)
}

/// Map a conversation role to a short, color-coded badge span.
pub(super) fn role_badge(role: &str) -> Span<'static> {
    match role {
        "user" => Span::styled(
            "USER",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        "assistant" => Span::styled(
            "ASSISTANT",
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        ),
        "system" => Span::styled("SYSTEM", Style::default().fg(Color::DarkGray)),
        "tool" => Span::styled(
            "TOOL",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::DIM),
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
pub(super) fn message_header(
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

/// Render a one-line progress bar from a `PullProgress` snapshot.
///
/// Shows the status text, an ASCII bar, and a percentage/bytes label.
/// When the total is unknown, the bar is empty and the label shows
/// completed bytes or the status string.
pub(super) fn progress_line(p: &crate::tui::app::PullProgress) -> Line<'static> {
    const BAR_WIDTH: usize = 20;

    let (bar, label) = match (p.completed, p.total) {
        (Some(c), Some(t)) if t > 0 => {
            let pct = ((c as f64 / t as f64) * 100.0).min(100.0) as u8;
            let filled = ((c as f64 / t as f64) * BAR_WIDTH as f64).min(BAR_WIDTH as f64) as usize;
            let mut bar_chars: Vec<char> = vec!['['];
            bar_chars.extend(std::iter::repeat_n('█', filled));
            bar_chars.extend(std::iter::repeat_n('░', BAR_WIDTH - filled));
            bar_chars.push(']');
            let bar: String = bar_chars.into_iter().collect();
            (bar, format!("{}% ({:.1}/{:.1} MB)", pct, mb(c), mb(t)))
        }
        (Some(c), _) => {
            let bar = format!("[{}]", "░".repeat(BAR_WIDTH));
            (bar, format!("{:.1} MB downloaded", mb(c)))
        }
        _ => {
            let bar = format!("[{}]", "░".repeat(BAR_WIDTH));
            (bar, p.status.clone())
        }
    };

    Line::from(vec![
        Span::styled(
            " ⬇ ".to_string(),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(p.status.clone(), Style::default().fg(Color::Cyan)),
        Span::styled(" ".to_string(), Style::default()),
        Span::styled(bar, Style::default().fg(Color::Cyan)),
        Span::styled(" ".to_string(), Style::default()),
        Span::styled(label, Style::default().fg(Color::DarkGray)),
    ])
}

fn mb(bytes: u64) -> f64 {
    bytes as f64 / (1024.0 * 1024.0)
}

pub(super) fn tool_card_lines(
    entry: &ConversationEntry,
    prev: Option<&ConversationEntry>,
    collapsed: bool,
    search_query: &str,
    content_width: usize,
    spinner: &'static str,
    theme: &Theme,
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
    if entry.streaming {
        // Streaming indicator: spinner + the incremental output so far.
        header_spans.push(Span::styled(
            format!("{spinner} {}", entry.content),
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::DIM),
        ));
    } else {
        header_spans.push(Span::styled(
            entry.content.clone(),
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::DIM),
        ));
    }
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
            spans.extend(highlight_line_spans(wline, search_query, body_style, theme));
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
// reason: each arg is an independent rendering input; grouping would obscure the wiring.
#[allow(clippy::too_many_arguments)]
pub(super) fn render_entry_lines(
    entry: &ConversationEntry,
    prev: Option<&ConversationEntry>,
    _idx: usize,
    content_width: usize,
    search_query: &str,
    collapsed: bool,
    spinner: &'static str,
    theme: &Theme,
) -> Vec<Line<'static>> {
    if entry.role == "tool" {
        return tool_card_lines(
            entry,
            prev,
            collapsed,
            search_query,
            content_width,
            spinner,
            theme,
        );
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
        let md_lines =
            render_markdown_lines_with_query(&entry.content, search_query, content_width, theme);
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
                theme,
            ));
            lines.push(Line::from(spans));
        }
    }

    lines
}

/// Build the full list of rendered chat lines and the line index where
/// each conversation message begins. Used by `render_chat` indirectly
/// and directly by search navigation to compute a scroll offset for the
/// current match.
///
/// Unlike `render_chat`, this helper does not use the render cache and
/// does not append the "more lines below" footer or compute scroll
/// geometry. It returns the raw line list and a parallel `message_start`
/// vector such that `message_start[i]` is the line index of the first
/// rendered line belonging to `state.conversation.messages[i]`.
pub(super) fn build_chat_lines(
    state: &AppState,
    content_width: usize,
) -> (Vec<Line<'static>>, Vec<usize>) {
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut message_start: Vec<usize> = Vec::with_capacity(state.conversation.messages.len());

    // Sandbox posture banner (always visible if unsandboxed).
    if state.provider.unsandboxed {
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

    // Connection banner at top.
    match &state.provider.connection {
        ConnectionState::Connected { .. } | ConnectionState::Connecting => {}
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

    // Loading indicator.
    if state.generation.is_generating
        && state.conversation.messages.back().map(|m| m.role.as_str()) != Some("assistant")
    {
        lines.push(Line::from(vec![Span::styled(
            format!(" ⏳ {} ", state.spinner_char()),
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::DIM),
        )]));
        lines.push(Line::from(""));
    }

    // Conversation messages.
    let last_idx = state.conversation.messages.len().saturating_sub(1);
    let mut prev_entry: Option<&ConversationEntry> = None;
    let mut idx = 0;
    while idx < state.conversation.messages.len() {
        let entry = &state.conversation.messages[idx];
        message_start.push(lines.len());

        // ── Tool call grouping: collapse consecutive tool entries ──
        // Instead of rendering each ToolStart+ToolResult as a separate
        // card, group consecutive tool entries into a single collapsed
        // header: "🔧 N tool calls — Enter to expand". This prevents
        // tool cards from fragmenting the assistant's text flow.
        if entry.role == "tool" && !idx_is_streaming_tool(state, idx, last_idx) {
            // Count consecutive tool entries.
            let group_start = idx;
            let mut group_count = 1;
            while idx + 1 < state.conversation.messages.len()
                && state.conversation.messages[idx + 1].role == "tool"
                && !idx_is_streaming_tool(state, idx + 1, last_idx)
            {
                idx += 1;
                group_count += 1;
            }

            if group_count > 1 && state.tool_should_collapse(idx) {
                // Render as a single grouped header.
                let tool_names: Vec<&str> = (group_start..=idx)
                    .map(|i| {
                        let e = &state.conversation.messages[i];
                        // Tool entries store the tool name in content header
                        // like "🔧 bash" — extract it, fallback to "tool".
                        e.content.split_whitespace().nth(1).unwrap_or("tool")
                    })
                    .collect();
                let unique: Vec<&str> = {
                    let mut seen = std::collections::HashSet::new();
                    tool_names.iter().filter(|t| seen.insert(**t)).copied().collect()
                };
                let label = if unique.len() == 1 {
                    format!("{} ×{}", unique[0], group_count)
                } else {
                    format!("{} tool calls", group_count)
                };
                lines.push(Line::from(vec![
                    Span::raw(" "),
                    Span::styled(
                        "🔧 ",
                        Style::default().fg(Color::Cyan),
                    ),
                    Span::styled(
                        label,
                        Style::default()
                            .fg(Color::Cyan)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        "  · Enter to expand",
                        Style::default()
                            .fg(Color::DarkGray)
                            .add_modifier(Modifier::DIM),
                    ),
                ]));
                lines.push(Line::from(""));
                prev_entry = Some(entry);
                idx += 1;
                continue;
            }
            // group_count == 1 or expanded: fall through to normal render
        }

        let is_streaming_last =
            idx == last_idx && state.generation.is_generating && entry.role == "assistant";
        let collapsed = if is_streaming_last {
            false
        } else if entry.role == "tool" {
            state.tool_should_collapse(idx)
        } else {
            state.message_should_collapse(idx)
        };
        let entry_lines = render_entry_lines(
            entry,
            prev_entry,
            idx,
            content_width,
            &state.search.query,
            collapsed,
            state.spinner_char(),
            &state.ui.theme,
        );
        lines.extend(entry_lines);
        lines.push(Line::from(""));
        prev_entry = Some(entry);
        idx += 1;
    }

    // Inline thinking block.
    if state.generation.thinking_panel_visible && !state.generation.thinking_buffer.is_empty() {
        let thinking_width = content_width.saturating_sub(4);
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
        for t in &state.generation.thinking_buffer {
            for line in textwrap::fill(t, thinking_width).lines() {
                lines.push(Line::from(vec![
                    Span::styled("    │ ", border_style),
                    Span::styled(line.to_string(), body_style),
                ]));
            }
        }
    }

    (lines, message_start)
}
