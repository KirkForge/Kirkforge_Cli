//! Rendering utilities and display helpers for the TUI.

use crate::tui::search::case_fold_with_mapping;
use crate::tui::syntax::{highlight_line, highlighter_for};
use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, Tag, TagEnd};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

mod format;
mod table;
use table::{render_table, ListState, TableState};

pub use format::{budget_pct, format_budget_indicator, format_duration, format_token_count};

fn current_style(style_stack: &[Style]) -> Style {
    style_stack
        .iter()
        .fold(Style::default(), |acc, s| acc.patch(*s))
}

fn code_block_style() -> Style {
    Style::default()
        .fg(Color::Rgb(180, 180, 140)) // soft yellow
        .bg(Color::Rgb(45, 45, 40)) // dim warm background
}

fn code_block_border_style() -> Style {
    Style::default().fg(Color::DarkGray)
}

fn code_block_header_style() -> Style {
    Style::default().fg(Color::Gray)
}

fn inline_code_style() -> Style {
    Style::default().fg(Color::Rgb(230, 160, 50)) // orange
}

fn code_block_header_line(lang: &Option<String>) -> Line<'static> {
    let label = lang.as_deref().unwrap_or("code");
    Line::from(vec![
        Span::styled("▌ ", code_block_border_style()),
        Span::styled(label.to_string(), code_block_header_style()),
        Span::styled("  · copy", code_block_border_style()),
    ])
}

/// Apply search highlight to an already-styled list of spans.
///
/// Matches are shown with a yellow background and black foreground while
/// keeping the original span style on non-matching portions.
fn highlight_spans(spans: Vec<Span<'static>>, query: &str) -> Vec<Span<'static>> {
    if query.is_empty() {
        return spans;
    }
    let query_lower = query.to_lowercase();
    let full: String = spans.iter().map(|s| s.content.as_ref()).collect();
    let (folded, mapping) = case_fold_with_mapping(&full);
    let mut result = Vec::new();
    let mut folded_pos = 0;
    let mut orig_last_end = 0;

    while let Some(rel) = folded[folded_pos..].find(&query_lower) {
        let folded_start = folded_pos + rel;
        let folded_end = folded_start + query_lower.len();
        let orig_start = mapping[folded_start];
        let orig_end = mapping.get(folded_end).copied().unwrap_or(full.len());

        if orig_start > orig_last_end {
            result.extend(slice_spans_by_range(&spans, orig_last_end, orig_start));
        }
        let mut matched = slice_spans_by_range(&spans, orig_start, orig_end);
        for s in &mut matched {
            s.style = s
                .style
                .fg(Color::Black)
                .bg(Color::Yellow)
                .add_modifier(Modifier::BOLD);
        }
        result.extend(matched);
        orig_last_end = orig_end;
        folded_pos = folded_end;
        if folded_pos >= folded.len() {
            break;
        }
    }

    if orig_last_end < full.len() {
        result.extend(slice_spans_by_range(&spans, orig_last_end, full.len()));
    }

    result
}

fn slice_spans_by_range(spans: &[Span<'static>], start: usize, end: usize) -> Vec<Span<'static>> {
    let mut out = Vec::new();
    let mut offset = 0;
    for span in spans {
        let len = span.content.len();
        let span_start = offset;
        let span_end = offset + len;
        if span_end <= start || span_start >= end {
            offset += len;
            continue;
        }
        let slice_start = start.saturating_sub(span_start);
        let slice_end = (end.saturating_sub(span_start)).min(len);
        if slice_start < slice_end {
            out.push(Span::styled(
                span.content[slice_start..slice_end].to_string(),
                span.style,
            ));
        }
        offset += len;
    }
    out
}

/// Highlight every occurrence of `query` inside `line` with a
/// high-visibility background. Non-matching parts keep `base_style`.
///
/// This is a line-level highlight used for plain-text, tool-output, and
/// assistant markdown rendering. It is case-insensitive and finds the
/// same occurrences the search module reports, so the rendered chat
/// reflects the active search.
pub(crate) fn highlight_line_spans(
    line: &str,
    query: &str,
    base_style: Style,
) -> Vec<Span<'static>> {
    if query.is_empty() {
        return vec![Span::styled(line.to_string(), base_style)];
    }
    let query_lower = query.to_lowercase();
    let (folded, mapping) = case_fold_with_mapping(line);
    let mut spans = Vec::new();
    let mut folded_pos = 0;
    let mut orig_last_end = 0;
    while let Some(rel) = folded[folded_pos..].find(&query_lower) {
        let folded_start = folded_pos + rel;
        let folded_end = folded_start + query_lower.len();
        let orig_start = mapping[folded_start];
        let orig_end = mapping.get(folded_end).copied().unwrap_or(line.len());

        if orig_start > orig_last_end {
            spans.push(Span::styled(
                line[orig_last_end..orig_start].to_string(),
                base_style,
            ));
        }
        spans.push(Span::styled(
            line[orig_start..orig_end].to_string(),
            base_style
                .fg(Color::Black)
                .bg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ));
        orig_last_end = orig_end;
        folded_pos = folded_end;
        if folded_pos >= folded.len() {
            break;
        }
    }
    if orig_last_end < line.len() {
        spans.push(Span::styled(line[orig_last_end..].to_string(), base_style));
    }
    spans
}

fn blockquote_style() -> Style {
    Style::default()
        .fg(Color::Rgb(180, 180, 180))
        .add_modifier(Modifier::ITALIC)
}

fn flush_current(lines: &mut Vec<Line<'static>>, current_line: &mut Vec<Span<'static>>) {
    flush_current_with_prefix(lines, current_line, 0);
}

fn flush_current_with_prefix(
    lines: &mut Vec<Line<'static>>,
    current_line: &mut Vec<Span<'static>>,
    blockquote_depth: usize,
) {
    if current_line.is_empty() {
        return;
    }
    if blockquote_depth > 0 {
        let mut prefixed = vec![Span::styled(
            "▌".repeat(blockquote_depth) + " ",
            blockquote_style(),
        )];
        prefixed.extend(std::mem::take(current_line));
        lines.push(Line::from(prefixed));
    } else {
        lines.push(Line::from(std::mem::take(current_line)));
    }
}

fn push_blank_with_depth(lines: &mut Vec<Line<'static>>, blockquote_depth: usize) {
    if lines.last().map(|l| !l.spans.is_empty()).unwrap_or(true) {
        if blockquote_depth > 0 {
            lines.push(Line::from(Span::styled(
                "▌".repeat(blockquote_depth) + " ",
                blockquote_style(),
            )));
        } else {
            lines.push(Line::from(""));
        }
    }
}

/// Render markdown text into ratatui `Line`s with styled `Span`s.
///
/// Uses `pulldown-cmark` for full CommonMark support: headings, paragraphs,
/// unordered/ordered lists, links, code blocks, bold, italic, strikethrough,
/// inline code, blockquotes, and tables. `content_width` sizes table columns;
/// pass `0` to render cells as-is.
///
/// Search matches for `query` are highlighted inline (case-insensitive).
/// Pass an empty string to skip highlighting.
pub fn render_markdown_lines_with_query(
    text: &str,
    query: &str,
    content_width: usize,
) -> Vec<Line<'static>> {
    let options = pulldown_cmark::Options::ENABLE_TABLES;
    let parser = pulldown_cmark::Parser::new_ext(text, options);
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut current_line: Vec<Span<'static>> = Vec::new();
    let mut style_stack: Vec<Style> = Vec::new();
    let mut in_code_block = false;
    let mut code_block_lang: Option<String> = None;
    let mut code_block_badge_emitted = false;
    let mut code_highlighter = highlighter_for(None);
    let mut list_stack: Vec<ListState> = Vec::new();
    let mut blockquote_depth: usize = 0;
    let mut table_state: Option<TableState> = None;

    for event in parser {
        match event {
            Event::Start(tag) => match tag {
                Tag::Paragraph => {}
                Tag::Heading { level, .. } => {
                    let mut style = Style::default().add_modifier(Modifier::BOLD);
                    if level == HeadingLevel::H1 {
                        style = style.fg(Color::White);
                    }
                    style_stack.push(style);
                }
                Tag::Strong => {
                    style_stack.push(Style::default().add_modifier(Modifier::BOLD));
                }
                Tag::Emphasis => {
                    style_stack.push(Style::default().add_modifier(Modifier::ITALIC));
                }
                Tag::Strikethrough => {
                    style_stack.push(Style::default().add_modifier(Modifier::CROSSED_OUT));
                }
                Tag::CodeBlock(kind) => {
                    in_code_block = true;
                    code_block_badge_emitted = false;
                    code_block_lang = match kind {
                        CodeBlockKind::Fenced(lang) => {
                            let lang = lang.to_string();
                            if lang.is_empty() {
                                None
                            } else {
                                Some(lang)
                            }
                        }
                        CodeBlockKind::Indented => None,
                    };
                    code_highlighter = highlighter_for(code_block_lang.as_deref());
                }
                Tag::List(start_num) => {
                    list_stack.push(ListState {
                        ordered: start_num.is_some(),
                        number: start_num.unwrap_or(1),
                    });
                }
                Tag::Item => {
                    flush_current(&mut lines, &mut current_line);
                    let depth = list_stack.len().saturating_sub(1);
                    let indent = "  ".repeat(depth);
                    let prefix = if let Some(state) = list_stack.last_mut() {
                        if state.ordered {
                            let n = state.number;
                            state.number += 1;
                            format!("{indent}{n}. ")
                        } else {
                            format!("{indent}- ")
                        }
                    } else {
                        "- ".to_string()
                    };
                    current_line.push(Span::styled(prefix, current_style(&style_stack)));
                }
                Tag::Link { .. } => {
                    style_stack.push(
                        Style::default()
                            .fg(Color::Cyan)
                            .add_modifier(Modifier::UNDERLINED),
                    );
                }
                Tag::Image { .. } => {
                    style_stack.push(
                        Style::default()
                            .fg(Color::Magenta)
                            .add_modifier(Modifier::ITALIC),
                    );
                }
                Tag::BlockQuote(_) => {
                    blockquote_depth += 1;
                    style_stack.push(blockquote_style());
                }
                Tag::Table(alignments) => {
                    flush_current_with_prefix(&mut lines, &mut current_line, blockquote_depth);
                    table_state = Some(TableState {
                        alignments,
                        ..Default::default()
                    });
                }
                Tag::TableHead => {
                    if let Some(ref mut t) = table_state {
                        t.start_cell();
                    }
                }
                Tag::TableRow => {
                    if let Some(ref mut t) = table_state {
                        t.current_row.clear();
                    }
                }
                Tag::TableCell => {
                    if let Some(ref mut t) = table_state {
                        t.start_cell();
                    }
                }
                _ => {}
            },
            Event::End(tag) => match tag {
                TagEnd::Paragraph => {
                    flush_current_with_prefix(&mut lines, &mut current_line, blockquote_depth);
                    push_blank_with_depth(&mut lines, blockquote_depth);
                }
                TagEnd::Heading(_) => {
                    flush_current_with_prefix(&mut lines, &mut current_line, blockquote_depth);
                    push_blank_with_depth(&mut lines, blockquote_depth);
                    style_stack.pop();
                }
                TagEnd::Strong
                | TagEnd::Emphasis
                | TagEnd::Strikethrough
                | TagEnd::Link
                | TagEnd::Image => {
                    style_stack.pop();
                }
                TagEnd::CodeBlock => {
                    flush_current_with_prefix(&mut lines, &mut current_line, blockquote_depth);
                    in_code_block = false;
                    code_block_lang = None;
                    code_block_badge_emitted = false;
                    code_highlighter = highlighter_for(None);
                    push_blank_with_depth(&mut lines, blockquote_depth);
                }
                TagEnd::Item => {
                    flush_current_with_prefix(&mut lines, &mut current_line, blockquote_depth);
                }
                TagEnd::List(_) => {
                    list_stack.pop();
                    push_blank_with_depth(&mut lines, blockquote_depth);
                }
                TagEnd::BlockQuote(_) => {
                    flush_current_with_prefix(&mut lines, &mut current_line, blockquote_depth);
                    blockquote_depth = blockquote_depth.saturating_sub(1);
                    style_stack.pop();
                    push_blank_with_depth(&mut lines, blockquote_depth);
                }
                TagEnd::Table => {
                    if let Some(t) = table_state.take() {
                        let table_lines = render_table(t, query, content_width);
                        for line in table_lines {
                            lines.push(line);
                        }
                        push_blank_with_depth(&mut lines, blockquote_depth);
                    }
                }
                TagEnd::TableHead => {
                    if let Some(ref mut t) = table_state {
                        t.end_row();
                    }
                }
                TagEnd::TableRow => {
                    if let Some(ref mut t) = table_state {
                        t.end_row();
                    }
                }
                TagEnd::TableCell => {
                    if let Some(ref mut t) = table_state {
                        t.end_cell();
                    }
                }
                _ => {}
            },
            Event::Text(t) => {
                if let Some(ref mut table) = table_state {
                    table.current_cell.push_str(&t);
                } else if in_code_block {
                    // Code blocks render as standalone bordered lines; flush any
                    // open inline line first so the border/background block is
                    // self-contained.
                    flush_current_with_prefix(&mut lines, &mut current_line, blockquote_depth);
                    if !code_block_badge_emitted {
                        code_block_badge_emitted = true;
                        lines.push(code_block_header_line(&code_block_lang));
                    }
                    let style = code_block_style();
                    let border_style = code_block_border_style();
                    let text = t.to_string();
                    let parts: Vec<&str> = text.split('\n').collect();
                    for (i, chunk) in parts.iter().enumerate() {
                        let is_last = i == parts.len().saturating_sub(1);
                        if is_last && chunk.is_empty() {
                            // Trailing newline at the closing fence produces a
                            // bare border line; skip it.
                            continue;
                        }
                        let mut line_spans = if blockquote_depth > 0 {
                            vec![Span::styled(
                                "▌".repeat(blockquote_depth) + " ",
                                blockquote_style(),
                            )]
                        } else {
                            Vec::new()
                        };
                        line_spans.push(Span::styled("▕ ", border_style));
                        if !chunk.is_empty() {
                            let code_spans = highlight_line(&mut code_highlighter, chunk, style);
                            if query.is_empty() {
                                line_spans.extend(code_spans);
                            } else {
                                line_spans.extend(highlight_spans(code_spans, query));
                            }
                        }
                        lines.push(Line::from(line_spans));
                    }
                } else {
                    current_line.extend(highlight_line_spans(
                        &t,
                        query,
                        current_style(&style_stack),
                    ));
                }
            }
            Event::Code(c) => {
                current_line.extend(highlight_line_spans(&c, query, inline_code_style()));
            }
            Event::SoftBreak | Event::HardBreak => {
                flush_current_with_prefix(&mut lines, &mut current_line, blockquote_depth);
            }
            Event::Rule => {
                flush_current_with_prefix(&mut lines, &mut current_line, blockquote_depth);
                let rule_width = content_width.max(1);
                lines.push(Line::from("─".repeat(rule_width)));
                push_blank_with_depth(&mut lines, blockquote_depth);
            }
            Event::Html(h) => {
                current_line.push(Span::styled(
                    h.to_string(),
                    Style::default().fg(Color::DarkGray),
                ));
            }
            Event::FootnoteReference(r) => {
                current_line.push(Span::styled(
                    r.to_string(),
                    Style::default().fg(Color::DarkGray),
                ));
            }
            Event::TaskListMarker(checked) => {
                let marker = if checked { "[x] " } else { "[ ] " };
                current_line.push(Span::styled(
                    marker.to_string(),
                    Style::default().fg(Color::Yellow),
                ));
            }
            Event::InlineMath(t) | Event::DisplayMath(t) => {
                current_line.push(Span::styled(t.to_string(), current_style(&style_stack)));
            }
            Event::InlineHtml(h) => {
                current_line.push(Span::styled(
                    h.to_string(),
                    Style::default().fg(Color::DarkGray),
                ));
            }
        }
    }

    flush_current_with_prefix(&mut lines, &mut current_line, blockquote_depth);

    // Trim trailing blank lines so the chat panel doesn't pad the end.
    // Blockquote blank lines carry a "▌ " prefix span, so we treat any
    // line whose spans are empty or consist only of prefix bars as blank.
    while lines.last().map(is_visual_blank).unwrap_or(false) {
        lines.pop();
    }

    lines
}

/// True if a line is visually blank (empty or only blockquote prefix bars).
fn is_visual_blank(line: &Line) -> bool {
    line.spans.iter().all(|s| {
        let trimmed = s.content.trim();
        trimmed.is_empty() || trimmed.chars().all(|c| c == '▌' || c == ' ')
    })
}

/// Extract the text of every code block in a markdown string, in order.
///
/// Returns a vector of block contents (without fence markers) for all
/// fenced or indented code blocks. Used by the TUI's per-block copy
/// keybinding so `Ctrl+Shift+B` can cycle through blocks instead of
/// always copying only the last one.
pub fn all_code_blocks(markdown: &str) -> Vec<String> {
    let parser = pulldown_cmark::Parser::new(markdown);
    let mut in_block = false;
    let mut content = String::new();
    let mut blocks = Vec::new();
    for event in parser {
        match event {
            Event::Start(Tag::CodeBlock(_)) => {
                in_block = true;
                content.clear();
            }
            Event::End(TagEnd::CodeBlock) => {
                in_block = false;
                let trimmed = content.trim_end().to_string();
                if !trimmed.is_empty() {
                    blocks.push(trimmed);
                }
                content.clear();
            }
            Event::Text(t) if in_block => {
                content.push_str(&t);
            }
            _ => {}
        }
    }
    // Keep a trailing incomplete block too (malformed markdown).
    if in_block {
        let trimmed = content.trim_end().to_string();
        if !trimmed.is_empty() {
            blocks.push(trimmed);
        }
    }
    blocks
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Budget indicator (v1.2-p6) ───────────────────────────────────
    //
    // The status bar uses `format_budget_indicator(used, max)` to show
    // the user how full the model's context window is, with a color
    // signal that tells them when `/compact` is a good idea. These
    // tests pin down the three behaviors the status widget depends on.

    /// When `max == 0` (model not connected, or model has no
    /// `max_context_tokens` configured), the helper returns the plain
    /// used-count with `DarkGray` — the caller falls back to the
    /// old-style `↑N` display.
    #[test]
    fn test_budget_indicator_no_max_falls_back() {
        let (text, color) = format_budget_indicator(12_345, 0);
        assert_eq!(text, "12.3K");
        // DarkGray is the "neutral / unknown" cue. We don't pin
        // the exact variant here because ratatui's Color enum
        // doesn't impl PartialEq — we just verify the helper
        // returned *something* non-default.
        let _ = color;
    }

    /// At 33% budget use, the indicator is green and shows both
    /// the absolute count and the percentage.
    #[test]
    fn test_budget_indicator_comfortable_is_green() {
        let (text, color) = format_budget_indicator(42_000, 128_000);
        assert!(
            text.contains("42.0K"),
            "should show used in K, got '{text}'"
        );
        assert!(
            text.contains("128.0K"),
            "should show max in K, got '{text}'"
        );
        assert!(
            text.contains("(32%)"),
            "should show 32% (42k/128k), got '{text}'"
        );
        assert!(
            matches!(color, Color::Green),
            "comfortable use should be green"
        );
    }

    /// At 60% (mid-range) the indicator should be yellow.
    /// The TUI uses this as the "consider /compact" cue.
    #[test]
    fn test_budget_indicator_tight_is_yellow() {
        // 60_000 / 100_000 = exactly 60%
        let (text, color) = format_budget_indicator(60_000, 100_000);
        assert!(text.contains("(60%)"), "should show 60%, got '{text}'");
        assert!(matches!(color, Color::Yellow), "50-80% should be yellow");
    }

    /// At 85% the indicator should be red — "compact now" cue.
    #[test]
    fn test_budget_indicator_high_is_red() {
        // 85_000 / 100_000 = 85%
        let (text, color) = format_budget_indicator(85_000, 100_000);
        assert!(text.contains("(85%)"), "should show 85%, got '{text}'");
        assert!(matches!(color, Color::Red), "80-95% should be red");
    }

    /// `budget_pct` is the shared helper behind `format_budget_indicator`
    /// and the `/status` recommendation. It's `None` when `max == 0`
    /// (no model connected yet) and clamps to `100` when `used > max`.
    /// Regression guard for the clippy::manual_checked_ops lint that
    /// fired in the v1.2-p8 cycle when the call site in `commands.rs`
    /// was reimplementing the same arithmetic.
    #[test]
    fn test_budget_pct_basic() {
        assert_eq!(budget_pct(0, 100), Some(0));
        assert_eq!(budget_pct(50, 100), Some(50));
        assert_eq!(budget_pct(85, 100), Some(85));
        assert_eq!(budget_pct(100, 100), Some(100));
    }

    #[test]
    fn test_budget_pct_max_zero_is_none() {
        // No model connected yet, or model has 0 max_context_tokens.
        // Caller treats None as "no recommendation can be made."
        assert_eq!(budget_pct(0, 0), None);
        assert_eq!(budget_pct(42_000, 0), None);
    }

    #[test]
    fn test_budget_pct_clamps_to_100() {
        // Used can exceed max (e.g. after a tool cap that pushed past
        // the model's advertised context). The percentage must clamp,
        // not wrap or overflow.
        assert_eq!(budget_pct(150, 100), Some(100));
        assert_eq!(budget_pct(usize::MAX, 100), Some(100));
    }

    #[test]
    fn test_budget_pct_no_overflow_on_huge_used() {
        // saturating_mul guard: usize::MAX * 100 would overflow without it.
        // With saturating_mul, used_saturating_mul(100) caps at usize::MAX,
        // divided by max still yields a value <= 100 once clamped.
        let p = budget_pct(usize::MAX, 1);
        assert_eq!(p, Some(100));
    }

    // ── Markdown renderer (Step 1 of TUI chat polish) ──────────────────

    #[test]
    fn test_markdown_bold() {
        let lines = render_markdown_lines_with_query("**bold text**", "", 80);
        assert_eq!(lines.len(), 1);
        assert_eq!(
            lines[0].spans,
            vec![Span::styled(
                "bold text",
                Style::default().add_modifier(Modifier::BOLD)
            )]
        );
    }

    #[test]
    fn test_markdown_inline_code() {
        let lines = render_markdown_lines_with_query("use `cargo test`", "", 80);
        assert_eq!(lines.len(), 1);
        assert_eq!(
            lines[0].spans,
            vec![
                Span::raw("use "),
                Span::styled("cargo test", inline_code_style()),
            ]
        );
    }

    #[test]
    fn test_markdown_heading() {
        let lines = render_markdown_lines_with_query("# Title\n\nbody", "", 80);
        assert_eq!(lines.len(), 3);
        assert_eq!(
            lines[0].spans,
            vec![Span::styled(
                "Title",
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD)
            )]
        );
        assert!(lines[1].spans.is_empty());
        assert_eq!(lines[2].spans, vec![Span::raw("body")]);
    }

    #[test]
    fn test_markdown_unordered_list() {
        let lines = render_markdown_lines_with_query("- a\n- b\n- c", "", 80);
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0].spans[0].content, "- ");
        assert_eq!(lines[0].spans[1].content, "a");
        assert_eq!(lines[1].spans[0].content, "- ");
        assert_eq!(lines[1].spans[1].content, "b");
        assert_eq!(lines[2].spans[0].content, "- ");
        assert_eq!(lines[2].spans[1].content, "c");
    }

    #[test]
    fn test_markdown_ordered_list() {
        let lines = render_markdown_lines_with_query("1. first\n2. second", "", 80);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].spans[0].content, "1. ");
        assert_eq!(lines[0].spans[1].content, "first");
        assert_eq!(lines[1].spans[0].content, "2. ");
        assert_eq!(lines[1].spans[1].content, "second");
    }

    #[test]
    fn test_markdown_nested_inline_styles() {
        let lines = render_markdown_lines_with_query("**bold *italic* bold**", "", 80);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].spans.len(), 3);
        assert_eq!(
            lines[0].spans[0],
            Span::styled("bold ", Style::default().add_modifier(Modifier::BOLD))
        );
        assert_eq!(
            lines[0].spans[1],
            Span::styled(
                "italic",
                Style::default()
                    .add_modifier(Modifier::BOLD)
                    .add_modifier(Modifier::ITALIC)
            )
        );
        assert_eq!(
            lines[0].spans[2],
            Span::styled(" bold", Style::default().add_modifier(Modifier::BOLD))
        );
    }

    #[test]
    fn test_markdown_code_block_with_lang_badge() {
        let lines = render_markdown_lines_with_query("```rust\nfn main() {}\n```", "", 80);
        assert_eq!(lines.len(), 2);
        assert_eq!(
            lines[0].spans,
            vec![
                Span::styled("▌ ", code_block_border_style()),
                Span::styled("rust", code_block_header_style()),
                Span::styled("  · copy", code_block_border_style()),
            ]
        );
        // Body is syntax highlighted: `fn` keyword plus the rest as plain code.
        assert_eq!(
            lines[1].spans[0],
            Span::styled("▕ ", code_block_border_style())
        );
        let body_text: String = lines[1].spans[1..]
            .iter()
            .map(|s| s.content.as_ref())
            .collect();
        assert_eq!(body_text, "fn main() {}");
        assert!(
            lines[1]
                .spans
                .iter()
                .any(|s| s.content == "fn" && s.style.fg == Some(Color::Rgb(220, 120, 220))),
            "rust keyword should be highlighted"
        );
    }

    #[test]
    fn test_markdown_code_block_without_language_uses_code_fallback() {
        let lines = render_markdown_lines_with_query("```\nhello\n```", "", 80);
        assert_eq!(lines.len(), 2);
        assert_eq!(
            lines[0].spans,
            vec![
                Span::styled("▌ ", code_block_border_style()),
                Span::styled("code", code_block_header_style()),
                Span::styled("  · copy", code_block_border_style()),
            ]
        );
        assert_eq!(
            lines[1].spans,
            vec![
                Span::styled("▕ ", code_block_border_style()),
                Span::styled("hello", code_block_style()),
            ]
        );
    }

    #[test]
    fn test_markdown_code_block_body_has_background() {
        let lines = render_markdown_lines_with_query("```python\nprint(1)\n```", "", 80);
        let body = &lines[1];
        assert_eq!(body.spans[0].content, "▕ ");
        // Syntax highlighting splits `print(1)` into several spans; every
        // content span should still carry the code-block background.
        for span in &body.spans[1..] {
            assert!(
                matches!(span.style.bg, Some(Color::Rgb(45, 45, 40))),
                "span '{}' should have dim background, got {:?}",
                span.content,
                span.style.bg
            );
        }
    }

    #[test]
    fn test_markdown_indented_code_block_gets_border() {
        let lines = render_markdown_lines_with_query("    indented\n    block", "", 80);
        // Should produce a header and two body lines.
        assert!(lines.len() >= 3);
        assert_eq!(lines[0].spans[0].content, "▌ ");
        assert_eq!(lines[1].spans[0].content, "▕ ");
        assert_eq!(lines[2].spans[0].content, "▕ ");
    }

    #[test]
    fn test_markdown_code_block_trims_trailing_newline_border() {
        let lines = render_markdown_lines_with_query("```\na\n```", "", 80);
        // Should be header + one body line; no bare trailing border line.
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[1].spans.len(), 2);
        assert_ne!(lines[1].spans[1].content, "");
    }

    #[test]
    fn test_markdown_link() {
        let lines = render_markdown_lines_with_query("see [docs](https://docs.rs)", "", 80);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].spans.len(), 2);
        assert_eq!(lines[0].spans[0].content, "see ");
        assert_eq!(
            lines[0].spans[1],
            Span::styled(
                "docs",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::UNDERLINED)
            )
        );
    }

    #[test]
    fn test_markdown_search_highlight_in_plain_text() {
        let lines = render_markdown_lines_with_query("hello world", "world", 80);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].spans.len(), 2);
        assert_eq!(lines[0].spans[0].content, "hello ");
        assert_eq!(lines[0].spans[1].content, "world");
        assert!(
            matches!(lines[0].spans[1].style.bg, Some(Color::Yellow)),
            "search match should have yellow background"
        );
    }

    #[test]
    fn test_markdown_search_highlight_in_inline_code() {
        let lines = render_markdown_lines_with_query("use `cargo test`", "cargo", 80);
        assert_eq!(lines.len(), 1);
        // "use " + "cargo" (highlight) + " test" (plain inline code style)
        assert_eq!(lines[0].spans[0].content, "use ");
        assert_eq!(lines[0].spans[1].content, "cargo");
        assert!(
            matches!(lines[0].spans[1].style.bg, Some(Color::Yellow)),
            "inline code search match should be highlighted"
        );
        assert_eq!(lines[0].spans[2].content, " test");
    }

    #[test]
    fn test_markdown_search_highlight_in_code_block() {
        let lines = render_markdown_lines_with_query("```python\nprint(needle)\n```", "needle", 80);
        // Header + one body line.
        assert_eq!(lines.len(), 2);
        let body = &lines[1];
        assert_eq!(body.spans[0].content, "▕ ");
        assert_eq!(body.spans[1].content, "print(");
        assert_eq!(body.spans[2].content, "needle");
        assert!(
            matches!(body.spans[2].style.bg, Some(Color::Yellow)),
            "code-block search match should be highlighted"
        );
        assert_eq!(body.spans[3].content, ")");
    }

    #[test]
    fn test_markdown_search_highlight_unicode_case_folding() {
        // `İ` lowercases to two bytes/characters (`i` + combining dot).
        // A naive `to_lowercase()` search reports byte offsets into the
        // folded string, which would slice the original mid-character and
        // panic. The mapping-aware renderer must align to original byte
        // boundaries.
        let lines = render_markdown_lines_with_query("İstanbul", "stan", 80);
        assert_eq!(lines.len(), 1);
        let spans: Vec<String> = lines[0]
            .spans
            .iter()
            .map(|s| s.content.to_string())
            .collect();
        assert_eq!(
            spans,
            vec!["İ".to_string(), "stan".to_string(), "bul".to_string()]
        );
    }

    #[test]
    fn test_code_block_search_highlight_unicode_case_folding() {
        // Code-block highlighting uses `highlight_spans` on the already
        // syntax-highlighted spans, so it must also translate folded offsets
        // back to original byte boundaries.
        let lines = render_markdown_lines_with_query("```\nİstanbul\n```", "stan", 80);
        assert!(lines.len() >= 2);
        let body = &lines[1];
        let spans: Vec<String> = body.spans.iter().map(|s| s.content.to_string()).collect();
        assert!(spans.contains(&"İ".to_string()));
        assert!(spans.contains(&"stan".to_string()));
        assert!(spans.contains(&"bul".to_string()));
    }

    #[test]
    fn test_all_code_blocks_returns_multiple_blocks() {
        let md = "```a\nfirst\n```\n\n```b\nsecond\n```";
        assert_eq!(
            all_code_blocks(md),
            vec!["first".to_string(), "second".to_string()]
        );
    }

    #[test]
    fn test_all_code_blocks_skips_empty_blocks() {
        let md = "```\n```\n\n```\ncontent\n```";
        assert_eq!(all_code_blocks(md), vec!["content".to_string()]);
    }

    #[test]
    fn test_all_code_blocks_returns_none_for_plain_text() {
        assert!(all_code_blocks("No code here.").is_empty());
    }

    #[test]
    fn test_markdown_blockquote_renders_with_bar_and_muted_style() {
        let lines = render_markdown_lines_with_query("> quoted line", "", 80);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].spans[0].content, "▌ ");
        assert_eq!(lines[0].spans[1].content, "quoted line");
        assert!(
            lines[0].spans[1].style.add_modifier == Modifier::ITALIC,
            "blockquote text should be italic"
        );
    }

    #[test]
    fn test_markdown_nested_blockquote_renders_multiple_bars() {
        let lines = render_markdown_lines_with_query(">> nested quote", "", 80);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].spans[0].content, "▌▌ ");
    }

    #[test]
    fn test_markdown_table_renders_grid() {
        let md = "| Name | Value |\n|------|-------|\n| foo  | bar   |";
        let lines = render_markdown_lines_with_query(md, "", 80);
        assert!(
            lines.iter().any(|l| l
                .spans
                .iter()
                .map(|s| s.content.as_ref())
                .collect::<String>()
                == "| Name | Value |"),
            "expected header row, got: {lines:?}"
        );
        assert!(
            lines.iter().any(|l| l
                .spans
                .iter()
                .map(|s| s.content.as_ref())
                .collect::<String>()
                == "|----|-----|"),
            "expected separator, got: {lines:?}"
        );
        assert!(
            lines.iter().any(|l| l
                .spans
                .iter()
                .map(|s| s.content.as_ref())
                .collect::<String>()
                == "| foo  | bar   |"),
            "expected body row, got: {lines:?}"
        );
    }

    #[test]
    fn test_markdown_table_search_highlight() {
        let md = "| A | B |\n|---|---|\n| x | y |";
        let lines = render_markdown_lines_with_query(md, "y", 80);
        assert!(lines.iter().any(|l| l
            .spans
            .iter()
            .any(|s| s.content == "y" && matches!(s.style.bg, Some(Color::Yellow)))));
    }

    /// Edge case: entirely empty input should not panic.
    #[test]
    fn test_markdown_empty_input() {
        let lines = render_markdown_lines_with_query("", "", 80);
        assert!(lines.is_empty() || (lines.len() == 1 && lines[0].spans.is_empty()));
    }

    /// Edge case: nested lists render correct indentation and numbering,
    /// including a blank separator line before the next top-level item.
    #[test]
    fn test_markdown_nested_ordered_list() {
        let lines = render_markdown_lines_with_query(
            "1. outer\n   1. inner a\n   2. inner b\n2. next",
            "",
            80,
        );
        assert_eq!(lines.len(), 5);
        assert_eq!(lines[0].spans[0].content, "1. ");
        assert_eq!(lines[0].spans[1].content, "outer");
        assert_eq!(lines[1].spans[0].content, "  1. ");
        assert_eq!(lines[1].spans[1].content, "inner a");
        assert_eq!(lines[2].spans[0].content, "  2. ");
        assert_eq!(lines[2].spans[1].content, "inner b");
        assert!(lines[3].spans.is_empty());
        assert_eq!(lines[4].spans[0].content, "2. ");
        assert_eq!(lines[4].spans[1].content, "next");
    }

    /// Edge case: bold + italic inline combination is rendered as a
    /// single span with both modifiers.
    #[test]
    fn test_markdown_bold_italic_combined() {
        let lines = render_markdown_lines_with_query("***bold italic*** plain", "", 80);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].spans.len(), 2);
        assert_eq!(
            lines[0].spans[0],
            Span::styled(
                "bold italic",
                Style::default()
                    .add_modifier(Modifier::BOLD)
                    .add_modifier(Modifier::ITALIC)
            )
        );
        assert_eq!(lines[0].spans[1].content, " plain");
    }

    /// Edge case: long inline code should not be truncated or split into
    /// multiple lines unless wrapping is implemented; here we just verify
    /// the whole content survives in one line.
    #[test]
    fn test_markdown_long_inline_code_survives() {
        let long = "a".repeat(200);
        let lines = render_markdown_lines_with_query(&format!("`{long}`"), "", 80);
        assert_eq!(lines.len(), 1);
        let text: String = lines[0].spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, long);
    }

    /// Edge case: search highlight should still split inside a bold span.
    #[test]
    fn test_markdown_search_highlight_inside_bold() {
        let lines = render_markdown_lines_with_query("**hello world**", "world", 80);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].spans.len(), 2);
        assert_eq!(lines[0].spans[0].content, "hello ");
        assert_eq!(lines[0].spans[1].content, "world");
        assert!(
            lines[0].spans[1].style.bg == Some(Color::Yellow),
            "search match should have yellow background"
        );
    }

    /// Horizontal rules should span the available content width, not a
    /// hard-coded 40 columns (P6).
    #[test]
    fn test_markdown_rule_scales_with_content_width() {
        let lines = render_markdown_lines_with_query("before\n\n---\n\nafter", "", 30);
        let rule_line = lines
            .iter()
            .find(|l| l.spans.len() == 1 && l.spans[0].content.starts_with('─'))
            .expect("rule line present");
        assert_eq!(rule_line.spans[0].content.chars().count(), 30);
    }
}
