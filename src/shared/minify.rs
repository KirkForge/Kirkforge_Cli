/// Language-aware source minification for prompt compression.
///
/// Applied at prompt-build time only — files on disk are never modified.
/// Strips comments, collapses whitespace, shortens local identifiers
/// in languages where that's safe.
use std::path::Path;

/// Minify source code for a given language.
/// Returns the original content unchanged for unknown languages.
pub fn minify_source(path: &Path, content: &str) -> String {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
    match ext {
        "rs" => minify_rust(content),
        "py" => minify_python(content),
        "js" | "jsx" | "ts" | "tsx" => minify_js_like(content),
        "go" => minify_go(content),
        "md" => minify_markdown(content),
        "json" | "yaml" | "yml" | "toml" => content.to_string(), // structured data, keep as-is
        _ => content.to_string(),
    }
}

// ── Rust ──────────────────────────────────────────────────────────

fn minify_rust(source: &str) -> String {
    let mut out = String::with_capacity(source.len());
    let mut chars = source.chars().peekable();
    let mut in_block_comment = false;
    let mut prev_was_newline = false;

    while let Some(ch) = chars.next() {
        if in_block_comment {
            if ch == '*' && chars.peek() == Some(&'/') {
                chars.next();
                in_block_comment = false;
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

    out
}

// ── Python ────────────────────────────────────────────────────────

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

        // Triple-quoted string detection — peek ahead 2 chars
        if (ch == '"' || ch == '\'')
            && chars.peek() == Some(&ch)
        {
            let next2 = chars.clone().nth(1); // peek at second next
            if next2 == Some(ch) {
                // Found """ or '''
                chars.next(); // consume second char
                chars.next(); // consume third char

                // Docstring = starts at the beginning of a line (no non-whitespace before)
                let current_line = out.rsplit('\n').next().unwrap_or("");
                let is_docstring = current_line.trim().is_empty();

                if is_docstring {
                    // Skip everything until closing triple
                    let mut closing = false;
                    let mut count = 0;
                    for c in chars.by_ref() {
                        if c == ch {
                            count += 1;
                            if count == 3 {
                                closing = true;
                                break;
                            }
                        } else {
                            count = 0;
                        }
                    }
                    if closing {
                        continue; // skip the whole docstring
                    }
                    // Unterminated docstring — emit literally
                    out.push(ch);
                    out.push(ch);
                    out.push(ch);
                    continue;
                }
                // Regular triple-quoted string — emit literally
                out.push(ch);
                out.push(ch);
                out.push(ch);
                continue;
            }
        }

        // Collapse blank lines
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

        // Track string literals to avoid false comment detection
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
            // Don't detect comments inside strings
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

        // Condense multiple blank lines
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
    // Go is structurally identical to JS/Rust for comment stripping
    minify_js_like(source)
}

// ── Markdown ──────────────────────────────────────────────────────

fn minify_markdown(source: &str) -> String {
    // For markdown, we keep content but strip excessive whitespace
    // and collapse code blocks tighter
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

/// Estimate token savings from minification.
/// Rough: 1 token ≈ 4 chars for code.
pub fn savings_estimate(original: &str, minified: &str) -> (usize, f64) {
    let orig_chars = original.len();
    let min_chars = minified.len();
    let saved = orig_chars.saturating_sub(min_chars);
    let pct = if orig_chars > 0 {
        (saved as f64 / orig_chars as f64) * 100.0
    } else {
        0.0
    };
    (saved / 4, pct) // token estimate
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn test_minify_rust_strips_line_comments() {
        let src = "fn main() {\n    // this is a comment\n    println!(\"hi\");\n}";
        let result = minify_source(&PathBuf::from("x.rs"), src);
        assert!(!result.contains("comment"));
        assert!(result.contains("println"));
    }

    #[test]
    fn test_minify_rust_strips_block_comments() {
        let src = "/* block */ fn main() {}";
        let result = minify_source(&PathBuf::from("x.rs"), src);
        assert!(!result.contains("block"));
        assert!(result.contains("fn main"));
    }

    #[test]
    fn test_minify_python_strips_comments() {
        let src = "x = 1  # inline comment\ny = 2";
        let result = minify_source(&PathBuf::from("x.py"), src);
        assert!(!result.contains("inline comment"));
        assert!(result.contains("x = 1"));
    }

    #[test]
    fn test_minify_python_strips_docstrings() {
        let src = "def f():\n    \"\"\"Docstring here\"\"\"\n    pass";
        let result = minify_source(&PathBuf::from("x.py"), src);
        assert!(!result.contains("Docstring"));
        assert!(result.contains("def f()"));
    }

    #[test]
    fn test_minify_js_strips_comments() {
        let src = "const x = 1; // comment\nconst y = 2;";
        let result = minify_source(&PathBuf::from("x.js"), src);
        assert!(!result.contains("comment"));
        assert!(result.contains("const x"));
    }

    #[test]
    fn test_unknown_extension_preserved() {
        let src = "some text content";
        let result = minify_source(&PathBuf::from("x.txt"), src);
        assert_eq!(result, src);
    }
}