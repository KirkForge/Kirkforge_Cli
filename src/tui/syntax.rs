//! Lightweight syntax highlighting for code blocks in the TUI.
//!
//! This is intentionally dependency-free. A full `syntect` integration
//! would give more accurate colouring, but it also adds several MB of
//! syntax/theme dumps to the binary — a poor fit for the project's
//! "potato hardware" / small-static-binary goal. Instead we use a
//! simple state-machine highlighter that covers comments, strings,
//! numbers, and a curated keyword set for the most common languages.

use ratatui::style::{Color, Style};
use ratatui::text::Span;
use std::collections::HashSet;

/// A code highlighter keeps per-line state (e.g. mid-string or
/// mid-block-comment) so multi-line constructs render correctly.
#[derive(Debug, Clone, Default)]
pub struct Highlighter {
    state: State,
    language: Language,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum State {
    #[default]
    Normal,
    InString {
        quote: char,
        escaped: bool,
    },
    InBlockComment {
        closer: &'static str,
        matched: usize,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum Language {
    #[default]
    Unknown,
    Rust,
    Python,
    JavaScript,
    TypeScript,
    Go,
    C,
    Cpp,
    Java,
    Shell,
    Json,
    Yaml,
    Toml,
    Markdown,
}

impl Language {
    fn from_str(lang: &str) -> Self {
        match lang.to_ascii_lowercase().as_str() {
            "rust" | "rs" => Language::Rust,
            "python" | "py" => Language::Python,
            "javascript" | "js" | "jsx" => Language::JavaScript,
            "typescript" | "ts" | "tsx" => Language::TypeScript,
            "go" | "golang" => Language::Go,
            "c" => Language::C,
            "cpp" | "c++" | "cxx" => Language::Cpp,
            "java" => Language::Java,
            "shell" | "sh" | "bash" | "zsh" | "fish" => Language::Shell,
            "json" => Language::Json,
            "yaml" | "yml" => Language::Yaml,
            "toml" => Language::Toml,
            "markdown" | "md" => Language::Markdown,
            _ => Language::Unknown,
        }
    }

    fn line_comment(&self) -> Option<&'static str> {
        match self {
            Language::Shell | Language::Python | Language::Yaml | Language::Toml => Some("#"),
            Language::Rust
            | Language::JavaScript
            | Language::TypeScript
            | Language::Go
            | Language::C
            | Language::Cpp
            | Language::Java
            | Language::Json => Some("//"),
            Language::Markdown | Language::Unknown => None,
        }
    }

    fn block_comment(&self) -> Option<(&'static str, &'static str)> {
        match self {
            Language::Rust
            | Language::JavaScript
            | Language::TypeScript
            | Language::Go
            | Language::C
            | Language::Cpp
            | Language::Java
            | Language::Markdown => Some(("/*", "*/")),
            Language::Python => Some(("\"\"\"", "\"\"\"")),
            Language::Shell
            | Language::Json
            | Language::Yaml
            | Language::Toml
            | Language::Unknown => None,
        }
    }

    fn string_quotes(&self) -> &[char] {
        match self {
            Language::Shell => &['"', '\''],
            Language::Python => &['"', '\''],
            _ => &['"', '\'', '`'],
        }
    }

    fn keywords(&self) -> &'static [&'static str] {
        match self {
            Language::Rust => &[
                "as", "async", "await", "break", "const", "continue", "crate", "dyn", "else",
                "enum", "extern", "false", "fn", "for", "if", "impl", "in", "let", "loop", "match",
                "mod", "move", "mut", "pub", "ref", "return", "self", "Self", "static", "struct",
                "super", "trait", "true", "type", "union", "unsafe", "use", "where", "while",
            ],
            Language::Python => &[
                "False", "None", "True", "and", "as", "assert", "async", "await", "break", "class",
                "continue", "def", "del", "elif", "else", "except", "finally", "for", "from",
                "global", "if", "import", "in", "is", "lambda", "nonlocal", "not", "or", "pass",
                "raise", "return", "try", "while", "with", "yield",
            ],
            Language::JavaScript | Language::TypeScript => &[
                "async",
                "await",
                "break",
                "case",
                "catch",
                "class",
                "const",
                "continue",
                "debugger",
                "default",
                "delete",
                "do",
                "else",
                "enum",
                "export",
                "extends",
                "false",
                "finally",
                "for",
                "function",
                "if",
                "import",
                "in",
                "instanceof",
                "let",
                "new",
                "null",
                "of",
                "return",
                "static",
                "super",
                "switch",
                "this",
                "throw",
                "true",
                "try",
                "typeof",
                "undefined",
                "var",
                "void",
                "while",
                "with",
                "yield",
            ],
            Language::Go => &[
                "break",
                "case",
                "chan",
                "const",
                "continue",
                "default",
                "defer",
                "else",
                "fallthrough",
                "for",
                "func",
                "go",
                "goto",
                "if",
                "import",
                "interface",
                "map",
                "package",
                "range",
                "return",
                "select",
                "struct",
                "switch",
                "true",
                "false",
                "nil",
                "type",
                "var",
            ],
            Language::C | Language::Cpp => &[
                "alignas",
                "alignof",
                "auto",
                "bool",
                "break",
                "case",
                "catch",
                "char",
                "class",
                "const",
                "constexpr",
                "continue",
                "default",
                "delete",
                "do",
                "double",
                "else",
                "enum",
                "explicit",
                "export",
                "extern",
                "false",
                "float",
                "for",
                "friend",
                "goto",
                "if",
                "inline",
                "int",
                "long",
                "mutable",
                "namespace",
                "new",
                "noexcept",
                "nullptr",
                "operator",
                "private",
                "protected",
                "public",
                "register",
                "reinterpret_cast",
                "return",
                "short",
                "signed",
                "sizeof",
                "static",
                "static_assert",
                "static_cast",
                "struct",
                "switch",
                "template",
                "this",
                "thread_local",
                "throw",
                "true",
                "try",
                "typedef",
                "typeid",
                "typename",
                "union",
                "unsigned",
                "using",
                "virtual",
                "void",
                "volatile",
                "wchar_t",
                "while",
            ],
            Language::Java => &[
                "abstract",
                "assert",
                "boolean",
                "break",
                "byte",
                "case",
                "catch",
                "char",
                "class",
                "const",
                "continue",
                "default",
                "do",
                "double",
                "else",
                "enum",
                "extends",
                "false",
                "final",
                "finally",
                "float",
                "for",
                "goto",
                "if",
                "implements",
                "import",
                "instanceof",
                "int",
                "interface",
                "long",
                "native",
                "new",
                "null",
                "package",
                "private",
                "protected",
                "public",
                "return",
                "short",
                "static",
                "strictfp",
                "super",
                "switch",
                "synchronized",
                "this",
                "throw",
                "throws",
                "transient",
                "true",
                "try",
                "void",
                "volatile",
                "while",
            ],
            Language::Shell => &[
                "alias", "break", "case", "continue", "do", "done", "echo", "elif", "else", "esac",
                "exit", "export", "false", "fi", "for", "function", "if", "in", "local", "printf",
                "read", "readonly", "return", "select", "shift", "source", "then", "true",
                "typeset", "unset", "until", "while",
            ],
            Language::Json
            | Language::Yaml
            | Language::Toml
            | Language::Markdown
            | Language::Unknown => &[],
        }
    }

    fn keyword_set(&self) -> HashSet<&'static str> {
        self.keywords().iter().copied().collect()
    }
}

/// Highlight one line of code, updating the highlighter state.
///
/// Returns styled spans using `base_style` as the neutral code style.
/// The background from `base_style` is preserved for every span.
pub fn highlight_line(
    highlighter: &mut Highlighter,
    line: &str,
    base_style: Style,
) -> Vec<Span<'static>> {
    if matches!(highlighter.language, Language::Unknown | Language::Markdown) {
        return vec![Span::styled(line.to_string(), base_style)];
    }

    let keywords = highlighter.language.keyword_set();
    let quotes = highlighter.language.string_quotes();
    let line_comment = highlighter.language.line_comment();
    let block_comment = highlighter.language.block_comment();

    let mut spans = Vec::new();
    let mut buf = String::new();
    let mut i = 0;

    while i < line.len() {
        let rest = &line[i..];

        // Try to exit an active block comment first.
        if let State::InBlockComment { closer, matched } = highlighter.state {
            if let Some(pos) = rest.find(&closer[matched..]) {
                buf.push_str(&rest[..pos + (closer.len() - matched)]);
                flush_buf(&mut buf, &mut spans, comment_style(base_style));
                i += pos + (closer.len() - matched);
                highlighter.state = State::Normal;
                continue;
            } else {
                buf.push_str(rest);
                flush_buf(&mut buf, &mut spans, comment_style(base_style));
                let matched = prefix_match_len(rest, closer);
                highlighter.state = State::InBlockComment { closer, matched };
                i = line.len();
                continue;
            }
        }

        // Inside a string: look for the closing quote, honouring backslash escapes.
        if let State::InString { quote, escaped } = highlighter.state {
            let mut esc = escaped;
            let mut found_end = false;
            let mut pos = 0;
            for (idx, ch) in rest.char_indices() {
                if esc {
                    esc = false;
                    continue;
                }
                if ch == '\\' {
                    esc = true;
                    continue;
                }
                if ch == quote {
                    pos = idx + ch.len_utf8();
                    found_end = true;
                    break;
                }
            }
            buf.push_str(if found_end { &rest[..pos] } else { rest });
            flush_buf(&mut buf, &mut spans, string_style(base_style));
            i += if found_end { pos } else { rest.len() };
            highlighter.state = if found_end {
                State::Normal
            } else {
                State::InString {
                    quote,
                    escaped: esc,
                }
            };
            continue;
        }

        // Normal state: detect comments, strings, and tokens.
        // Line comment.
        if let Some(marker) = line_comment {
            if rest.starts_with(marker) {
                flush_buf(&mut buf, &mut spans, base_style);
                buf.push_str(rest);
                flush_buf(&mut buf, &mut spans, comment_style(base_style));
                i = line.len();
                continue;
            }
        }

        // Block comment start.
        if let Some((opener, closer)) = block_comment {
            if rest.starts_with(opener) {
                flush_buf(&mut buf, &mut spans, base_style);
                highlighter.state = State::InBlockComment { closer, matched: 0 };
                continue;
            }
        }

        // String start: consume the opening quote as a string-styled span,
        // then look for the closing quote in the remaining text.
        if let Some(&quote) = quotes.iter().find(|&&q| rest.starts_with(q)) {
            flush_buf(&mut buf, &mut spans, base_style);
            spans.push(Span::styled(quote.to_string(), string_style(base_style)));
            highlighter.state = State::InString {
                quote,
                escaped: false,
            };
            i += quote.len_utf8();
            continue;
        }

        // Number literal.
        if starts_number(rest) {
            flush_buf(&mut buf, &mut spans, base_style);
            let end = number_end(rest);
            let num = rest[..end].to_string();
            spans.push(Span::styled(num, number_style(base_style)));
            i += end;
            continue;
        }

        // Identifier / keyword / punctuation.
        if let Some((token, len)) = take_token(rest) {
            if keywords.contains(token.as_str()) {
                flush_buf(&mut buf, &mut spans, base_style);
                spans.push(Span::styled(token, keyword_style(base_style)));
            } else {
                buf.push_str(&token);
            }
            i += len;
            continue;
        }

        // Single non-identifier character.
        if let Some(ch) = rest.chars().next() {
            buf.push(ch);
            i += ch.len_utf8();
        } else {
            // Defensive: `rest` should never be empty here because the
            // loop guard checks `i < line.len()`, but malformed UTF-8 or
            // an off-by-one in byte-vs-char indexing could land here.
            // Break rather than panic so the TUI stays alive.
            break;
        }
    }

    flush_buf(&mut buf, &mut spans, base_style);
    spans
}

/// Create a highlighter for a given language tag (e.g. `"rust"`, `"python"`).
pub fn highlighter_for(lang: Option<&str>) -> Highlighter {
    Highlighter {
        language: lang.map(Language::from_str).unwrap_or(Language::Unknown),
        state: State::Normal,
    }
}

/// Length (in bytes) of the longest prefix of `prefix` that appears at
/// the end of `s`. Used when a block-comment closer is split across a
/// line boundary so the highlighter can resume searching for the rest.
fn prefix_match_len(s: &str, prefix: &str) -> usize {
    let mut last = 0;
    for (idx, ch) in prefix.char_indices() {
        let end = idx + ch.len_utf8();
        if s.ends_with(&prefix[..end]) {
            last = end;
        }
    }
    last
}

fn flush_buf(buf: &mut String, spans: &mut Vec<Span<'static>>, style: Style) {
    if !buf.is_empty() {
        spans.push(Span::styled(std::mem::take(buf), style));
    }
}

fn keyword_style(base: Style) -> Style {
    base.fg(Color::Rgb(220, 120, 220)) // soft magenta
}

fn string_style(base: Style) -> Style {
    base.fg(Color::Rgb(130, 200, 130)) // soft green
}

fn comment_style(base: Style) -> Style {
    base.fg(Color::DarkGray)
}

fn number_style(base: Style) -> Style {
    base.fg(Color::Rgb(230, 180, 90)) // soft amber
}

fn starts_number(s: &str) -> bool {
    let first = s.chars().next().unwrap_or('\0');
    first.is_ascii_digit() || (first == '.' && s.chars().nth(1).is_some_and(|c| c.is_ascii_digit()))
}

fn number_end(s: &str) -> usize {
    let mut saw_dot = false;
    let mut saw_exp = false;
    let mut i = 0;
    for ch in s.chars() {
        if ch.is_ascii_digit() {
            i += ch.len_utf8();
            continue;
        }
        if ch == '_' {
            i += ch.len_utf8();
            continue;
        }
        if ch == '.' && !saw_dot && !saw_exp {
            saw_dot = true;
            i += ch.len_utf8();
            continue;
        }
        if (ch == 'e' || ch == 'E') && !saw_exp && i > 0 {
            saw_exp = true;
            i += ch.len_utf8();
            // Allow a sign after the exponent.
            if let Some(next) = s[i..].chars().next() {
                if next == '+' || next == '-' {
                    i += next.len_utf8();
                }
            }
            continue;
        }
        break;
    }
    // At minimum consume the digit(s) we already verified exist.
    i.max(1)
}

fn take_token(s: &str) -> Option<(String, usize)> {
    let first = s.chars().next()?;
    if first.is_alphabetic() || first == '_' || first == '$' {
        let mut len = first.len_utf8();
        for ch in s[len..].chars() {
            if ch.is_alphanumeric() || ch == '_' || ch == '$' {
                len += ch.len_utf8();
            } else {
                break;
            }
        }
        Some((s[..len].to_string(), len))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rust_keywords_and_strings() {
        let mut h = highlighter_for(Some("rust"));
        let spans = highlight_line(&mut h, r#"let x = "hello";"#, code_block_style());
        let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, r#"let x = "hello";"#);
        assert!(spans
            .iter()
            .any(|s| s.content == "let" && s.style.fg == Some(Color::Rgb(220, 120, 220))));
        // String highlighting splits into opening quote, content, and closing quote spans.
        let string_text: String = spans
            .iter()
            .filter(|s| s.style.fg == Some(Color::Rgb(130, 200, 130)))
            .map(|s| s.content.as_ref())
            .collect();
        assert_eq!(string_text, "\"hello\"");
    }

    #[test]
    fn line_comment_greys_out_rest() {
        let mut h = highlighter_for(Some("rust"));
        let spans = highlight_line(&mut h, "let x = 1; // comment", code_block_style());
        let comment_span = spans
            .iter()
            .find(|s| s.content.contains("// comment"))
            .unwrap();
        assert_eq!(comment_span.style.fg, Some(Color::DarkGray));
    }

    #[test]
    fn multi_line_string_state() {
        let mut h = highlighter_for(Some("python"));
        let first = highlight_line(&mut h, "x = \"line one", code_block_style());
        let first_text: String = first.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(first_text, "x = \"line one");

        let second = highlight_line(&mut h, "line two\"", code_block_style());
        let second_text: String = second.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(second_text, "line two\"");
        assert!(second
            .iter()
            .any(|s| s.style.fg == Some(Color::Rgb(130, 200, 130))));
    }

    #[test]
    fn number_literals_colored() {
        let mut h = highlighter_for(Some("rust"));
        let spans = highlight_line(&mut h, "let n = 42;", code_block_style());
        let num = spans.iter().find(|s| s.content == "42").unwrap();
        assert_eq!(num.style.fg, Some(Color::Rgb(230, 180, 90)));
    }

    #[test]
    fn unknown_language_returns_plain_span() {
        let mut h = highlighter_for(Some("brainfuck"));
        let spans = highlight_line(&mut h, ">++++++++[<+++++++++>-]", code_block_style());
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].content, ">++++++++[<+++++++++>-]");
    }

    #[test]
    fn block_comment_closer_split_across_lines() {
        let mut h = highlighter_for(Some("rust"));
        let first = highlight_line(&mut h, "/* hello *", code_block_style());
        let first_text: String = first.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(first_text, "/* hello *");
        assert!(first.iter().any(|s| s.style.fg == Some(Color::DarkGray)));

        // The closing `/` arrives on the next line. With the bug, the
        // highlighter never noticed the partial `*` and stayed stuck in
        // block-comment mode, so ` world` would still be dark gray.
        let second = highlight_line(&mut h, "/ world", code_block_style());
        let second_text: String = second.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(second_text, "/ world");
        assert!(
            second
                .iter()
                .any(|s| s.content == " world" && s.style.fg != Some(Color::DarkGray)),
            "text after a split closer should return to normal highlighting"
        );
    }

    fn code_block_style() -> Style {
        Style::default()
            .fg(Color::Rgb(180, 180, 140))
            .bg(Color::Rgb(45, 45, 40))
    }
}
