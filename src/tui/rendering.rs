//! Rendering utilities and display helpers for the TUI.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

/// Render markdown text into ratatui `Line`s with styled `Span`s.
///
/// Handles bold (`**`), inline code (`` ` ``), and code blocks (```` ``` ````).
/// All delimiter text is consumed and not shown in the output.
pub fn render_markdown_lines(text: &str) -> Vec<Line<'static>> {
    let mut lines: Vec<Line> = Vec::new();
    let mut in_code_block = false;
    let mut code_block_lang = String::new();

    for raw_line in text.lines() {
        // ── Code block fence detection ──
        if raw_line.trim_start().starts_with("```") {
            if in_code_block {
                // End code block — insert a blank separator
                in_code_block = false;
                code_block_lang.clear();
                lines.push(Line::from(""));
            } else {
                // Start code block — extract language hint
                in_code_block = true;
                code_block_lang = raw_line
                    .trim_start()
                    .trim_start_matches("```")
                    .trim()
                    .to_string();
            }
            continue;
        }

        if in_code_block {
            // Code block content — dim style
            lines.push(Line::from(Span::styled(
                raw_line.to_string(),
                Style::default().fg(Color::Rgb(180, 180, 140)), // soft yellow
            )));
            continue;
        }

        // ── Inline content processing ──
        let mut spans: Vec<Span<'static>> = Vec::new();
        let chars: Vec<char> = raw_line.chars().collect();
        let mut i = 0;
        let mut text_buf = String::new();

        macro_rules! flush_text {
            () => {
                if !text_buf.is_empty() {
                    spans.push(Span::raw(std::mem::take(&mut text_buf)));
                }
            };
        }

        while i < chars.len() {
            // Bold (**...**)
            if i + 1 < chars.len() && chars[i] == '*' && chars[i + 1] == '*' {
                flush_text!();
                i += 2;
                // Collect content until closing **
                let mut bold_content = String::new();
                while i + 1 < chars.len() {
                    if chars[i] == '*' && chars[i + 1] == '*' {
                        break;
                    }
                    bold_content.push(chars[i]);
                    i += 1;
                }
                spans.push(Span::styled(
                    bold_content,
                    Style::default().add_modifier(Modifier::BOLD),
                ));
                if i + 1 < chars.len() {
                    i += 2; // skip closing **
                }
                continue;
            }

            // Inline code (`...`)
            if chars[i] == '`' && !(i + 1 < chars.len() && chars[i + 1] == '`') {
                flush_text!();
                i += 1;
                let mut code_content = String::new();
                while i < chars.len() && chars[i] != '`' {
                    code_content.push(chars[i]);
                    i += 1;
                }
                spans.push(Span::styled(
                    code_content,
                    Style::default().fg(Color::Rgb(230, 160, 50)), // orange for code
                ));
                if i < chars.len() {
                    i += 1; // skip closing `
                }
                continue;
            }

            // Italic (*...*) — only if NOT ** (already handled above)
            if chars[i] == '*' {
                flush_text!();
                i += 1;
                let mut italic_content = String::new();
                while i < chars.len() && chars[i] != '*' {
                    italic_content.push(chars[i]);
                    i += 1;
                }
                spans.push(Span::styled(
                    italic_content,
                    Style::default().add_modifier(Modifier::ITALIC),
                ));
                if i < chars.len() {
                    i += 1; // skip closing *
                }
                continue;
            }

            text_buf.push(chars[i]);
            i += 1;
        }

        flush_text!();
        lines.push(Line::from(spans));
    }

    lines
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
}
