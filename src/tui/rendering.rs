//! Rendering utilities — syntax highlighting, markdown rendering,
//! and display helpers for the TUI.

use std::sync::OnceLock;
use syntect::easy::HighlightLines;
use syntect::highlighting::ThemeSet;
use syntect::parsing::SyntaxSet;
use syntect::util::LinesWithEndings;

/// Global syntax set (loaded once).
fn syntax_set() -> &'static SyntaxSet {
    static SET: OnceLock<SyntaxSet> = OnceLock::new();
    SET.get_or_init(SyntaxSet::load_defaults_newlines)
}

/// Global theme set (loaded once).
fn theme_set() -> &'static ThemeSet {
    static SET: OnceLock<ThemeSet> = OnceLock::new();
    SET.get_or_init(ThemeSet::load_defaults)
}

/// Apply syntax highlighting to source code.
/// Returns a string with ANSI escape codes.
pub fn highlight_code(code: &str, extension: &str) -> String {
    let ss = syntax_set();
    let ts = theme_set();

    let syntax = ss
        .find_syntax_by_extension(extension)
        .unwrap_or_else(|| ss.find_syntax_plain_text());

    let theme = &ts.themes["base16-ocean.dark"];

    let mut highlighter = HighlightLines::new(syntax, theme);
    let mut out = String::new();

    for line in LinesWithEndings::from(code) {
        if let Ok(ranges) = highlighter.highlight_line(line, ss) {
            out.push_str(&syntect::util::as_24_bit_terminal_escaped(
                &ranges[..],
                false,
            ));
        } else {
            out.push_str(line);
        }
    }

    out
}

/// Convert Markdown to basic ANSI-rendered text.
/// This is a minimal parser — bold, italic, code, code blocks.
/// No tables, blockquotes, or headings (ratatui handles size, we handle emphasis).
pub fn render_markdown(text: &str) -> String {
    let mut out = String::new();
    let mut in_code_block = false;

    for line in text.lines() {
        if line.trim_start().starts_with("```") {
            if in_code_block {
                out.push_str("\x1b[0m\n"); // reset
                in_code_block = false;
            } else {
                out.push_str("\x1b[38;5;244m"); // dim gray for code fence
                in_code_block = true;
            }
            out.push('\n');
            continue;
        }

        if in_code_block {
            out.push_str(&format!("\x1b[38;5;187m{}\x1b[0m\n", line)); // soft yellow
            continue;
        }

        // Inline processing
        let mut i = 0;
        let chars: Vec<char> = line.chars().collect();
        while i < chars.len() {
            if i + 1 < chars.len() {
                if chars[i] == '*' && chars[i + 1] == '*' {
                    out.push_str("\x1b[1m"); // bold
                    i += 2;
                    continue;
                }
                if chars[i] == '*' && chars[i + 1] != ' ' {
                    out.push_str("\x1b[3m"); // italic
                    i += 1;
                    continue;
                }
            }
            if chars[i] == '`' {
                out.push_str("\x1b[38;5;215m"); // orange for inline code
                i += 1;
                // find closing backtick
                while i < chars.len() && chars[i] != '`' {
                    out.push(chars[i]);
                    i += 1;
                }
                out.push_str("\x1b[0m");
                if i < chars.len() {
                    i += 1;
                }
                continue;
            }
            out.push(chars[i]);
            i += 1;
        }
        out.push('\n');
    }

    out
}

/// Truncate text to fit within a width, adding ellipsis if truncated.
pub fn truncate(text: &str, max_width: usize) -> String {
    if text.len() <= max_width {
        text.to_string()
    } else {
        let mut t = text[..max_width.saturating_sub(3)].to_string();
        t.push_str("...");
        t
    }
}

/// Format a file size for display.
pub fn format_size(bytes: usize) -> String {
    if bytes < 1024 {
        format!("{} B", bytes)
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_markdown_bold() {
        let result = render_markdown("hello **world**");
        assert!(result.contains("\x1b[1m"));
        assert!(result.contains("world"));
    }

    #[test]
    fn test_markdown_code() {
        let result = render_markdown("use `std::fs`");
        assert!(result.contains("\x1b[38;5;215m"));
        assert!(result.contains("std::fs"));
    }

    #[test]
    fn test_markdown_code_block() {
        let result = render_markdown("```rust\nfn main() {}\n```");
        assert!(result.contains("fn main()"));
    }

    #[test]
    fn test_truncate_short() {
        assert_eq!(truncate("hello", 10), "hello");
    }

    #[test]
    fn test_truncate_long() {
        let t = truncate("hello world this is long", 10);
        assert!(t.ends_with("..."));
        assert!(t.len() <= 13);
    }
}
