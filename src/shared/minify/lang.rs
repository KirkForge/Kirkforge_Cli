// Per-language minification engine — pure, stateless string transforms.
//! Extracted from the VFS cache + public API in `mod.rs`.
#![allow(dead_code)] // minify_rust wrapper and Phase-10 symbols not yet wired up.

/// Minify content based on file extension (no disk caching).
pub(super) fn minify_content_by_ext(content: &str, ext: &str, preserve_tests: bool) -> String {
    match ext {
        "rs" => minify_rust_inner(content, preserve_tests),
        "py" => minify_python(content),
        "js" | "jsx" | "ts" | "tsx" => minify_js_like(content),
        "go" => minify_go(content),
        "c" | "h" | "cpp" | "hpp" | "cc" => minify_c_like(content),
        "java" => minify_java(content),
        "rb" => minify_ruby(content),
        "sh" | "bash" | "zsh" => minify_shell(content),
        "md" => minify_markdown(content),
        "json" | "yaml" | "yml" | "toml" => content.to_string(),
        _ => content.to_string(),
    }
}

// ── Helpers ─────────────────────────────────────────────────────────

/// Collapse consecutive blank lines to one, tracking state.
struct CollapseBlankLines<'a> {
    source: &'a str,
    prev_was_newline: bool,
    pos: usize,
}

impl<'a> CollapseBlankLines<'a> {
    fn new(source: &'a str) -> Self {
        Self {
            source,
            prev_was_newline: false,
            pos: 0,
        }
    }
}

impl<'a> Iterator for CollapseBlankLines<'a> {
    type Item = char;

    fn next(&mut self) -> Option<Self::Item> {
        while self.pos < self.source.len() {
            let ch = self.source[self.pos..].chars().next()?;
            let ch_len = ch.len_utf8();
            self.pos += ch_len;

            if ch == '\n' {
                if self.prev_was_newline {
                    continue; // skip this blank line
                }
                self.prev_was_newline = true;
                return Some(ch);
            } else if !ch.is_whitespace() || ch == ' ' {
                self.prev_was_newline = false;
            }
            return Some(ch);
        }
        None
    }
}

/// Strip test-only blocks (`#[cfg(test)]` or `#[test]` in Rust).
fn strip_test_blocks(source: &str) -> String {
    let mut out = String::new();
    let mut in_test_block = false;
    let mut test_started = false;
    let mut test_depth = 0usize;
    let mut brace_depth = 0usize;

    for line in source.lines() {
        let trimmed = line.trim();
        let mut suppress_line = in_test_block;

        // Detect #[cfg(test)] or #[test] attributes — only enter once
        if !in_test_block
            && (trimmed == "#[cfg(test)]"
                || trimmed == "#[test]"
                || trimmed.starts_with("#[cfg(test)]"))
        {
            in_test_block = true;
            test_started = false;
            continue;
        }

        // Track brace depth
        for ch in line.chars() {
            match ch {
                '{' => {
                    brace_depth += 1;
                    // Capture depth after the opening brace of the test block
                    if in_test_block && !test_started {
                        test_depth = brace_depth;
                        test_started = true;
                    }
                }
                '}' => {
                    brace_depth = brace_depth.saturating_sub(1);
                    if in_test_block && test_started && brace_depth < test_depth {
                        in_test_block = false;
                        test_started = false;
                        suppress_line = true;
                    }
                }
                _ => {}
            }
        }

        if suppress_line {
            continue;
        }

        out.push_str(line);
        out.push('\n');
    }
    out
}

/// Collapse consecutive blank lines directly.
pub(super) fn collapse_blank_lines(source: &str) -> String {
    let mut out = String::with_capacity(source.len());
    let mut prev_blank = false;

    for line in source.lines() {
        if line.trim().is_empty() {
            if prev_blank {
                continue;
            }
            prev_blank = true;
        } else {
            prev_blank = false;
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}

// ── Rust ──────────────────────────────────────────────────────────

fn minify_rust(source: &str) -> String {
    minify_rust_inner(source, false)
}

fn minify_rust_inner(source: &str, preserve_tests: bool) -> String {
    let mut out = String::with_capacity(source.len());
    let mut chars = source.chars().peekable();
    let mut in_block_comment = false;
    let mut in_string = false;
    let mut string_char = '"';
    let mut prev_was_newline = false;

    while let Some(ch) = chars.next() {
        if in_block_comment {
            if ch == '*' && chars.peek() == Some(&'/') {
                chars.next();
                in_block_comment = false;
            }
            continue;
        }

        // Track string literals to avoid false comment detection
        if !in_string && (ch == '"' || ch == '\'') {
            in_string = true;
            string_char = ch;
            out.push(ch);
            continue;
        }
        if in_string {
            if ch == '\\' {
                out.push(ch);
                if let Some(next) = chars.next() {
                    out.push(next);
                }
                continue;
            }
            out.push(ch);
            if ch == string_char {
                in_string = false;
            }
            continue;
        }

        // Line comment
        if ch == '/' && chars.peek() == Some(&'/') {
            while chars.next().is_some() && chars.peek() != Some(&'\n') {}
            continue;
        }

        // Block comment
        if ch == '/' && chars.peek() == Some(&'*') {
            chars.next();
            in_block_comment = true;
            continue;
        }

        // Collapse multiple blank lines to one
        if ch == '\n' {
            if prev_was_newline {
                continue;
            }
            prev_was_newline = true;
        } else if !ch.is_whitespace() || ch == ' ' {
            prev_was_newline = false;
        }

        out.push(ch);
    }

    // Apply test-block stripping as a second pass (unless preserving tests)
    let s = if preserve_tests {
        out
    } else {
        strip_test_blocks(&out)
    };
    collapse_blank_lines(&s)
}

fn minify_python(source: &str) -> String {
    let mut out = String::with_capacity(source.len());
    let mut prev_was_newline = false;
    let mut chars = source.chars().peekable();

    while let Some(ch) = chars.next() {
        // Line comment
        if ch == '#' {
            while chars.next().is_some() && chars.peek() != Some(&'\n') {}
            continue;
        }

        // Triple-quoted string detection
        if (ch == '"' || ch == '\'') && chars.peek() == Some(&ch) {
            let next2 = chars.clone().nth(1);
            if next2 == Some(ch) {
                chars.next();
                chars.next();
                let current_line = out.rsplit('\n').next().unwrap_or("");
                let is_docstring = current_line.trim().is_empty();

                if is_docstring {
                    let mut count = 0;
                    for c in chars.by_ref() {
                        if c == ch {
                            count += 1;
                            if count == 3 {
                                break;
                            }
                        } else {
                            count = 0;
                        }
                    }
                    continue;
                }
                out.push(ch);
                out.push(ch);
                out.push(ch);
                continue;
            }
        }

        if ch == '\n' {
            if prev_was_newline {
                continue;
            }
            prev_was_newline = true;
        } else if !ch.is_whitespace() {
            prev_was_newline = false;
        }

        out.push(ch);
    }

    out
}

// ── JS/TS/JSX/TSX ─────────────────────────────────────────────────

fn minify_js_like(source: &str) -> String {
    let mut out = String::with_capacity(source.len());
    let mut chars = source.chars().peekable();
    let mut in_block_comment = false;
    let mut in_string = false;
    let mut string_char = '"';
    let mut prev_was_newline = false;

    while let Some(ch) = chars.next() {
        if in_block_comment {
            if ch == '*' && chars.peek() == Some(&'/') {
                chars.next();
                in_block_comment = false;
            }
            continue;
        }

        if !in_string && (ch == '"' || ch == '\'' || ch == '`') {
            in_string = true;
            string_char = ch;
            out.push(ch);
            continue;
        }
        if in_string {
            if ch == '\\' {
                out.push(ch);
                if let Some(next) = chars.next() {
                    out.push(next);
                }
                continue;
            }
            out.push(ch);
            if ch == string_char {
                in_string = false;
            }
            continue;
        }

        if ch == '/' && chars.peek() == Some(&'/') {
            while chars.next().is_some() && chars.peek() != Some(&'\n') {}
            continue;
        }

        if ch == '/' && chars.peek() == Some(&'*') {
            chars.next();
            in_block_comment = true;
            continue;
        }

        if ch == '\n' {
            if prev_was_newline {
                continue;
            }
            prev_was_newline = true;
        } else if !ch.is_whitespace() || ch == ' ' {
            prev_was_newline = false;
        }

        out.push(ch);
    }

    out
}

// ── Go ────────────────────────────────────────────────────────────

fn minify_go(source: &str) -> String {
    minify_js_like(source)
}

// ── C/C++ / Java (string-aware comment stripper) ──────────────────

#[derive(Clone, Copy, PartialEq, Eq)]
enum CState {
    Normal,
    Str(char), // inside a "..." or '...' literal; payload = the delimiter
    Line,      // inside a // ... line comment
    Block,     // inside a /* ... */ block comment
}

/// Strip `//` line comments and `/* ... */` block comments from C-family source
/// without touching comment markers that appear inside string or char literals.
///
/// Newlines inside comments are preserved so that code on either side of a
/// multi-line comment never gets merged onto one line; `collapse_blank_lines`
/// (defined above) tidies the resulting gaps.
fn strip_c_style_comments(source: &str) -> String {
    let chars: Vec<char> = source.chars().collect();
    let n = chars.len();
    let mut out = String::with_capacity(source.len());
    let mut state = CState::Normal;
    let mut i = 0;

    while i < n {
        let c = chars[i];
        let next = if i + 1 < n { chars[i + 1] } else { '\0' };

        match state {
            CState::Normal => {
                if c == '/' && next == '/' {
                    state = CState::Line;
                    i += 2;
                    continue;
                }
                if c == '/' && next == '*' {
                    state = CState::Block;
                    i += 2;
                    continue;
                }
                if c == '"' || c == '\'' {
                    state = CState::Str(c);
                    out.push(c);
                    i += 1;
                    continue;
                }
                out.push(c);
                i += 1;
            }
            CState::Str(delim) => {
                if c == '\\' {
                    // Escape: emit the backslash and whatever it escapes verbatim,
                    // so an escaped quote can't prematurely close the literal.
                    out.push(c);
                    if i + 1 < n {
                        out.push(chars[i + 1]);
                    }
                    i += 2;
                    continue;
                }
                out.push(c);
                if c == delim {
                    state = CState::Normal;
                }
                i += 1;
            }
            CState::Line => {
                if c == '\n' {
                    out.push('\n');
                    state = CState::Normal;
                }
                i += 1;
            }
            CState::Block => {
                if c == '*' && next == '/' {
                    state = CState::Normal;
                    i += 2;
                    continue;
                }
                if c == '\n' {
                    out.push('\n');
                }
                i += 1;
            }
        }
    }

    // Match the original `trim_end()`-per-line behaviour (drops the whitespace
    // left where an inline comment used to be).
    out.lines()
        .map(|l| l.trim_end())
        .collect::<Vec<_>>()
        .join("\n")
}

fn minify_c_like(source: &str) -> String {
    collapse_blank_lines(&strip_c_style_comments(source))
}

fn minify_java(source: &str) -> String {
    // `/** ... */` Javadoc is handled by the block path (it starts with `/*`),
    // so the old redundant second strip_block_comments pass is gone.
    collapse_blank_lines(&strip_c_style_comments(source))
}

// ── Ruby ──────────────────────────────────────────────────────────

fn minify_ruby(source: &str) -> String {
    let mut out = String::new();
    for line in source.lines() {
        let trimmed = line.trim();
        // Skip comment lines and shebang
        if trimmed.starts_with('#') {
            // Check if it's a heredoc or string containing # — skip for now
            if !trimmed.starts_with("# encoding") && !trimmed.starts_with("# frozen_string_literal")
            {
                continue;
            }
        }
        out.push_str(line);
        out.push('\n');
    }
    collapse_blank_lines(&out)
}

// ── Shell ─────────────────────────────────────────────────────────

fn minify_shell(source: &str) -> String {
    let mut out = String::new();
    for line in source.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('#') && !trimmed.starts_with("#!") {
            continue; // strip comments but keep shebang
        }
        out.push_str(line);
        out.push('\n');
    }
    collapse_blank_lines(&out)
}

// ── Markdown ──────────────────────────────────────────────────────

fn minify_markdown(source: &str) -> String {
    let mut out = String::with_capacity(source.len());
    let mut prev_blank = false;
    for line in source.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            if prev_blank {
                continue;
            }
            prev_blank = true;
        } else {
            prev_blank = false;
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_strip_test_blocks_swallows_closing_brace() {
        // Regression for C13: the line containing the matching closing
        // brace of a stripped test block used to leak into the minified
        // output.
        let source = r#"pub fn add(a: i32, b: i32) -> i32 { a + b }

#[cfg(test)]
mod tests {
    #[test]
    fn test_add() {
        assert_eq!(add(1, 2), 3);
    }
}

pub fn sub(a: i32, b: i32) -> i32 { a - b }
"#;
        let out = strip_test_blocks(source);
        assert!(!out.contains("mod tests"));
        assert!(!out.contains("assert_eq"));
        assert!(
            !out.lines().any(|l| l.trim() == "}"),
            "standalone closing brace leaked: {out}"
        );
        assert!(out.contains("pub fn add"));
        assert!(out.contains("pub fn sub"));
    }

    #[test]
    fn test_strip_test_blocks_nested_braces() {
        let source = r#"#[cfg(test)]
mod tests {
    fn helper(x: i32) {
        if x > 0 {
            println!("ok");
        }
    }

    #[test]
    fn demo() {
        helper(1);
    }
}

pub const X: i32 = 1;
"#;
        let out = strip_test_blocks(source);
        assert!(!out.contains("mod tests"));
        assert!(!out.contains("helper"));
        assert!(!out.contains("demo"));
        assert!(out.contains("pub const X"));
    }
}
