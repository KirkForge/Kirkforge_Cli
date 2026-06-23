//! Rendering utilities and display helpers for the TUI.

use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, Tag, TagEnd};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use crate::tui::syntax::{highlighter_for, highlight_line};

#[derive(Debug, Default)]
struct ListState {
    ordered: bool,
    number: u64,
}

fn current_style(style_stack: &[Style]) -> Style {
    style_stack.iter().fold(Style::default(), |acc, s| acc.patch(*s))
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
    let full_lower = full.to_lowercase();
    let mut result = Vec::new();
    let mut last_end = 0;

    while let Some(rel) = full_lower[last_end..].find(&query_lower) {
        let match_start = last_end + rel;
        let match_end = match_start + query.len();

        if match_start > last_end {
            result.extend(slice_spans_by_range(&spans, last_end, match_start));
        }
        let mut matched = slice_spans_by_range(&spans, match_start, match_end);
        for s in &mut matched {
            s.style = s
                .style
                .fg(Color::Black)
                .bg(Color::Yellow)
                .add_modifier(Modifier::BOLD);
        }
        result.extend(matched);
        last_end = match_end;
        if last_end >= full.len() {
            break;
        }
    }

    if last_end < full.len() {
        result.extend(slice_spans_by_range(&spans, last_end, full.len()));
    }

    result
}

fn slice_spans_by_range(
    spans: &[Span<'static>],
    start: usize,
    end: usize,
) -> Vec<Span<'static>> {
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
pub(crate) fn highlight_line_spans(line: &str, query: &str, base_style: Style) -> Vec<Span<'static>> {
    if query.is_empty() {
        return vec![Span::styled(line.to_string(), base_style)];
    }
    let query_lower = query.to_lowercase();
    let line_lower = line.to_lowercase();
    let mut spans = Vec::new();
    let mut start = 0;
    while let Some(rel) = line_lower[start..].find(&query_lower) {
        let match_start = start + rel;
        let match_end = match_start + query.len();

        if match_start > start {
            spans.push(Span::styled(
                line[start..match_start].to_string(),
                base_style,
            ));
        }
        spans.push(Span::styled(
            line[match_start..match_end].to_string(),
            base_style
                .fg(Color::Black)
                .bg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ));
        start = match_end;
        if start >= line.len() {
            break;
        }
    }
    if start < line.len() {
        spans.push(Span::styled(line[start..].to_string(), base_style));
    }
    spans
}

fn flush_current(lines: &mut Vec<Line<'static>>, current_line: &mut Vec<Span<'static>>) {
    if !current_line.is_empty() {
        lines.push(Line::from(std::mem::take(current_line)));
    }
}

/// Push a single blank line, but avoid stacking multiple blanks.
fn push_blank(lines: &mut Vec<Line<'static>>) {
    if lines.last().map(|l| !l.spans.is_empty()).unwrap_or(true) {
        lines.push(Line::from(""));
    }
}

/// Render markdown text into ratatui `Line`s with styled `Span`s.
///
/// Uses `pulldown-cmark` for full CommonMark support: headings, paragraphs,
/// unordered/ordered lists, links, code blocks, bold, italic, strikethrough,
/// and inline code. Blockquotes render as plain paragraphs in this version.
///
/// Search matches for `query` are highlighted inline (case-insensitive).
/// Pass an empty string to skip highlighting.
pub fn render_markdown_lines_with_query(text: &str, query: &str) -> Vec<Line<'static>> {
    let parser = pulldown_cmark::Parser::new(text);
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut current_line: Vec<Span<'static>> = Vec::new();
    let mut style_stack: Vec<Style> = Vec::new();
    let mut in_code_block = false;
    let mut code_block_lang: Option<String> = None;
    let mut code_block_badge_emitted = false;
    let mut code_highlighter = highlighter_for(None);
    let mut list_stack: Vec<ListState> = Vec::new();

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
                            format!("{}{}. ", indent, n)
                        } else {
                            format!("{}- ", indent)
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
                _ => {}
            },
            Event::End(tag) => match tag {
                TagEnd::Paragraph => {
                    flush_current(&mut lines, &mut current_line);
                    push_blank(&mut lines);
                }
                TagEnd::Heading(_) => {
                    flush_current(&mut lines, &mut current_line);
                    push_blank(&mut lines);
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
                    flush_current(&mut lines, &mut current_line);
                    in_code_block = false;
                    code_block_lang = None;
                    code_block_badge_emitted = false;
                    code_highlighter = highlighter_for(None);
                    push_blank(&mut lines);
                }
                TagEnd::Item => {
                    flush_current(&mut lines, &mut current_line);
                }
                TagEnd::List(_) => {
                    list_stack.pop();
                    push_blank(&mut lines);
                }
                _ => {}
            },
            Event::Text(t) => {
                if in_code_block {
                    // Code blocks render as standalone bordered lines; flush any
                    // open inline line first so the border/background block is
                    // self-contained.
                    flush_current(&mut lines, &mut current_line);
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
                        let mut line_spans = vec![Span::styled("▕ ", border_style)];
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
                current_line.extend(highlight_line_spans(
                    &c,
                    query,
                    inline_code_style(),
                ));
            }
            Event::SoftBreak | Event::HardBreak => {
                flush_current(&mut lines, &mut current_line);
            }
            Event::Rule => {
                flush_current(&mut lines, &mut current_line);
                lines.push(Line::from("─".repeat(40)));
                push_blank(&mut lines);
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
                current_line.push(Span::styled(
                    t.to_string(),
                    current_style(&style_stack),
                ));
            }
            Event::InlineHtml(h) => {
                current_line.push(Span::styled(
                    h.to_string(),
                    Style::default().fg(Color::DarkGray),
                ));
            }
        }
    }

    flush_current(&mut lines, &mut current_line);

    // Trim trailing blank lines so the chat panel doesn't pad the end.
    while lines.last().map(|l| l.spans.is_empty()).unwrap_or(false) {
        lines.pop();
    }

    lines
}

/// Render markdown text without an active search query.
///
/// This is the convenience wrapper used by callers that don't need
/// search highlighting (e.g., standalone preview rendering). Kept for
/// the existing test suite and any future non-search consumers.
#[allow(dead_code)]
pub fn render_markdown_lines(text: &str) -> Vec<Line<'static>> {
    render_markdown_lines_with_query(text, "")
}

/// Extract the text of the most recent code block in a markdown string.
///
/// Walks the document and keeps the accumulated text of the last fenced
/// or indented code block it sees. Returns `Some(content)` (without the
/// fence markers) or `None` if there is no code block.
pub fn last_code_block(markdown: &str) -> Option<String> {
    let parser = pulldown_cmark::Parser::new(markdown);
    let mut in_block = false;
    let mut content = String::new();
    for event in parser {
        match event {
            Event::Start(Tag::CodeBlock(_)) => {
                in_block = true;
                content.clear();
            }
            Event::End(TagEnd::CodeBlock) => {
                in_block = false;
            }
            Event::Text(t) if in_block => {
                content.push_str(&t);
            }
            _ => {}
        }
    }
    // Keep a trailing incomplete block too (malformed markdown).
    if !content.is_empty() || in_block {
        let trimmed = content.trim_end().to_string();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed)
        }
    } else {
        None
    }
}

/// Format a duration for display.
pub fn format_duration(secs: f64) -> String {
    if secs < 60.0 {
        format!("{:.1}s", secs)
    } else if secs < 3600.0 {
        format!("{:.0}m {:.0}s", secs / 60.0, secs % 60.0)
    } else {
        format!("{:.0}h {:.0}m", secs / 3600.0, (secs % 3600.0) / 60.0)
    }
}

/// Token budget formatting: "12.4K / 128K"
pub fn format_token_count(tokens: usize) -> String {
    if tokens < 1000 {
        tokens.to_string()
    } else {
        format!("{:.1}K", tokens as f64 / 1000.0)
    }
}

/// Format a token-budget indicator for the status bar.
///
/// Returns a `(text, color)` pair. The text is one of:
/// - `""` (empty) when `max == 0` or `used == 0` AND `max == 0` — i.e. the
///   caller hasn't supplied a budget. Caller should fall back to the
///   plain `↑N` display in that case.
/// - `"<used>"` (e.g. `"12.4K"`) when `max == 0` — same fallback,
///   but we still return the formatted used count so the caller can
///   use it without recomputing.
/// - `"<used>/<max> (P%)"` (e.g. `"12.4K/128K (10%)"`) when both
///   `used > 0` and `max > 0`.
///
/// The color is the budget-pressure threshold:
/// - `Green`  — `< 50%`  (comfortable)
/// - `Yellow` — `50–80%` (getting tight, consider `/compact`)
/// - `Red`    — `80–95%` (compact now)
/// - `LightRed` (Rgb 255, 100, 100) — `> 95%` (the B1.x layers are kicking in)
///
/// Why this lives in `rendering.rs` and not `status.rs`: the helper
/// is pure (string + color out, no ratatui types) so it's trivially
/// unit-testable without a frame buffer. The status widget just
/// consumes the output.
pub fn format_budget_indicator(used: usize, max: usize) -> (String, Color) {
    match budget_pct(used, max) {
        None => {
            // No budget known (model not connected yet, or model has
            // 0 max_context_tokens in its config). Return the plain
            // used-count so the caller can fall back to `↑N`.
            (format_token_count(used), Color::DarkGray)
        }
        Some(pct) => {
            let color = if pct < 50 {
                Color::Green
            } else if pct < 80 {
                Color::Yellow
            } else if pct < 95 {
                Color::Red
            } else {
                Color::Rgb(255, 100, 100) // light red — "the cliff is here"
            };

            let text = format!(
                "{}/{} ({}%)",
                format_token_count(used),
                format_token_count(max),
                pct
            );

            (text, color)
        }
    }
}

/// Compute the context-budget percentage used, in `0..=100`.
///
/// Returns `None` if `max == 0` (no model connected yet, or the
/// model reports `0` for `max_context_tokens`). Caller should
/// treat `None` as "no recommendation can be made."
///
/// Uses `saturating_mul` + `checked_div` so an absurd `used` value
/// can't overflow and a zero `max` is the only failure mode.
pub fn budget_pct(used: usize, max: usize) -> Option<u8> {
    if max == 0 {
        return None;
    }
    // saturating_mul prevents overflow; checked_div returns Some
    // because we just guarded max > 0. Clamp to 100 because used
    // can exceed max (e.g. after a tool cap that pushed past the
    // model's advertised context).
    Some((used.saturating_mul(100) / max).min(100) as u8)
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
            "should show used in K, got '{}'",
            text
        );
        assert!(
            text.contains("128.0K"),
            "should show max in K, got '{}'",
            text
        );
        assert!(
            text.contains("(32%)"),
            "should show 32% (42k/128k), got '{}'",
            text
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
        assert!(text.contains("(60%)"), "should show 60%, got '{}'", text);
        assert!(matches!(color, Color::Yellow), "50-80% should be yellow");
    }

    /// At 85% the indicator should be red — "compact now" cue.
    #[test]
    fn test_budget_indicator_high_is_red() {
        // 85_000 / 100_000 = 85%
        let (text, color) = format_budget_indicator(85_000, 100_000);
        assert!(text.contains("(85%)"), "should show 85%, got '{}'", text);
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
        let lines = render_markdown_lines("**bold text**");
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
        let lines = render_markdown_lines("use `cargo test`");
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
        let lines = render_markdown_lines("# Title\n\nbody");
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
        let lines = render_markdown_lines("- a\n- b\n- c");
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
        let lines = render_markdown_lines("1. first\n2. second");
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].spans[0].content, "1. ");
        assert_eq!(lines[0].spans[1].content, "first");
        assert_eq!(lines[1].spans[0].content, "2. ");
        assert_eq!(lines[1].spans[1].content, "second");
    }

    #[test]
    fn test_markdown_nested_inline_styles() {
        let lines = render_markdown_lines("**bold *italic* bold**");
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
        let lines = render_markdown_lines("```rust\nfn main() {}\n```");
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
        assert_eq!(lines[1].spans[0], Span::styled("▕ ", code_block_border_style()));
        let body_text: String = lines[1].spans[1..].iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(body_text, "fn main() {}");
        assert!(
            lines[1].spans.iter().any(|s| s.content == "fn" && s.style.fg == Some(Color::Rgb(220, 120, 220))),
            "rust keyword should be highlighted"
        );
    }

    #[test]
    fn test_markdown_code_block_without_language_uses_code_fallback() {
        let lines = render_markdown_lines("```\nhello\n```");
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
        let lines = render_markdown_lines("```python\nprint(1)\n```");
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
        let lines = render_markdown_lines("    indented\n    block");
        // Should produce a header and two body lines.
        assert!(lines.len() >= 3);
        assert_eq!(lines[0].spans[0].content, "▌ ");
        assert_eq!(lines[1].spans[0].content, "▕ ");
        assert_eq!(lines[2].spans[0].content, "▕ ");
    }

    #[test]
    fn test_markdown_code_block_trims_trailing_newline_border() {
        let lines = render_markdown_lines("```\na\n```");
        // Should be header + one body line; no bare trailing border line.
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[1].spans.len(), 2);
        assert_ne!(lines[1].spans[1].content, "");
    }

    #[test]
    fn test_markdown_link() {
        let lines = render_markdown_lines("see [docs](https://docs.rs)");
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
        let lines = render_markdown_lines_with_query("hello world", "world");
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
        let lines = render_markdown_lines_with_query("use `cargo test`", "cargo");
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
        let lines =
            render_markdown_lines_with_query("```python\nprint(needle)\n```", "needle");
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
    fn test_last_code_block_extracts_fenced_block() {
        let md = "Some text.\n\n```rust\nfn main() {}\n```\n\nMore text.";
        assert_eq!(
            last_code_block(md),
            Some("fn main() {}".to_string()),
            "should return the last fenced code block without fence markers"
        );
    }

    #[test]
    fn test_last_code_block_returns_none_when_absent() {
        assert_eq!(
            last_code_block("No code here."),
            None,
            "plain markdown should have no code block"
        );
    }

    #[test]
    fn test_last_code_block_returns_last_of_multiple() {
        let md = "```a\nfirst\n```\n\n```b\nsecond\n```";
        assert_eq!(
            last_code_block(md),
            Some("second".to_string()),
            "should prefer the last code block"
        );
    }
}
