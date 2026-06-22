// Public/future surface in a binary crate: suppress dead-code warnings for pub items.
#![allow(dead_code)]

/// Language-aware source minification for prompt compression.
///
/// Applied at prompt-build time only — files on disk are never modified.
/// Strips comments, collapses whitespace, shortens local identifiers
/// in languages where that's safe.
///
/// Maintains a VFS cache keyed by (path, mtime) to avoid re-minifying
/// the same file across multiple turns.
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};
use std::time::SystemTime;

/// Thread-safe VFS minification cache.
static VFS_CACHE: LazyLock<Mutex<HashMap<(PathBuf, u64), String>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Cache capacity — limits memory growth.
const CACHE_CAPACITY: usize = 200;

/// Minify source code for a given language.
///
/// Results are cached in the VFS cache (keyed by path + mtime).
/// Returns the original content unchanged for unknown languages.
///
/// When `preserve_tests` is true, test-only blocks are preserved
/// (e.g. `#[cfg(test)]` in Rust). Use this for minifying conversation
/// history where the model has already seen the test code.
pub fn minify_source(path: &Path, content: &str) -> String {
    minify_source_impl(path, content, false)
}

/// Like `minify_source` but preserves test blocks — safe for
/// conversation history where the model has seen the content.
pub fn minify_source_safe(path: &Path, content: &str) -> String {
    minify_source_impl(path, content, true)
}

fn minify_source_impl(path: &Path, content: &str, preserve_tests: bool) -> String {
    // Check cache first (only for files that actually exist on disk)
    let mtime = match std::fs::metadata(path)
        .ok()
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
    {
        Some(m) => m,
        None => {
            // File doesn't exist on disk — skip caching, just minify directly
            let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
            return minify_content_by_ext(content, ext, preserve_tests);
        }
    };

    // Check cache
    {
        let cache = VFS_CACHE.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(cached) = cache.get(&(path.to_path_buf(), mtime)) {
            return cached.clone();
        }
    }

    // Minify
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
    let result = minify_content_by_ext(content, ext, preserve_tests);

    // Store in cache (only for non-preserve mode — safe variants skip cache)
    if !preserve_tests {
        let mut cache = VFS_CACHE.lock().unwrap_or_else(|e| e.into_inner());
        cache.insert((path.to_path_buf(), mtime), result.clone());
        if cache.len() > CACHE_CAPACITY {
            let target = CACHE_CAPACITY / 2;
            let keys: Vec<_> = cache.keys().take(target).cloned().collect();
            for k in &keys {
                cache.remove(k);
            }
        }
    }

    result
}

/// Minify content based on file extension (no disk caching).
fn minify_content_by_ext(content: &str, ext: &str, preserve_tests: bool) -> String {
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

/// Clear the VFS minification cache.
pub fn clear_minify_cache() {
    let mut cache = VFS_CACHE.lock().unwrap_or_else(|e| e.into_inner());
    cache.clear();
}

/// Remove cache entries for a specific file path.
pub fn invalidate_minify_cache(path: &Path) {
    let mut cache = VFS_CACHE.lock().unwrap_or_else(|e| e.into_inner());
    cache.retain(|(p, _), _| p != path);
}

/// Get current cache size.
pub fn minify_cache_size() -> usize {
    let cache = VFS_CACHE.lock().unwrap_or_else(|e| e.into_inner());
    cache.len()
}

/// Check whether the cache contains an entry for `path`.
///
/// Uses the **latest** `mtime` for the path on disk as the cache key
/// (which is what `minify_source` does internally), so callers don't
/// have to know the exact timestamp. Returns `false` for paths that
/// don't exist on disk — those entries are never cached by
/// `minify_source` (it falls back to direct minification).
///
/// Added for race-free test assertions: tests that need to assert
/// "this path got cached" no longer have to inspect the global
/// cache size, which is racy under `cargo test`'s default parallel
/// execution. Returns `false` for paths that are not on disk or
/// have no current cache entry.
pub fn cache_contains(path: &Path) -> bool {
    let mtime = match std::fs::metadata(path)
        .ok()
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
    {
        Some(m) => m,
        None => return false,
    };
    let cache = VFS_CACHE.lock().unwrap_or_else(|e| e.into_inner());
    cache.contains_key(&(path.to_path_buf(), mtime))
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
                    }
                }
                _ => {}
            }
        }

        if in_test_block {
            continue;
        }

        out.push_str(line);
        out.push('\n');
    }
    out
}

/// Collapse consecutive blank lines directly.
fn collapse_blank_lines(source: &str) -> String {
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

/// Estimate token savings from minification. 1 token ≈ 4 chars for code.
pub fn savings_estimate(original: &str, minified: &str) -> (usize, f64) {
    let orig_chars = original.len();
    let min_chars = minified.len();
    let saved = orig_chars.saturating_sub(min_chars);
    let pct = if orig_chars > 0 {
        (saved as f64 / orig_chars as f64) * 100.0
    } else {
        0.0
    };
    (saved / 4, pct)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    // ── Rust ────────────────────────────────────────────────────────

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
    fn test_minify_rust_strips_test_blocks() {
        let src = "fn add(x: i32) -> i32 { x + 1 }\n\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn test_add() {\n        assert_eq!(add(1), 2);\n    }\n}\n";
        let result = minify_source(&PathBuf::from("x.rs"), src);
        assert!(result.contains("fn add"));
        assert!(!result.contains("#[cfg(test)]"));
        assert!(!result.contains("test_add"));
    }

    #[test]
    fn test_minify_rust_preserves_struct_literals() {
        let src = "let s = S { field: \"// not a comment\" };";
        let result = minify_source(&PathBuf::from("x.rs"), src);
        assert!(result.contains("field"));
    }

    // ── Python ──────────────────────────────────────────────────────

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

    // ── JS/TS ───────────────────────────────────────────────────────

    #[test]
    fn test_minify_js_strips_comments() {
        let src = "const x = 1; // comment\nconst y = 2;";
        let result = minify_source(&PathBuf::from("x.js"), src);
        assert!(!result.contains("comment"));
        assert!(result.contains("const x"));
    }

    #[test]
    fn test_minify_ts_preserves_template_strings() {
        let src = "let s = `hello // not a comment`;";
        let result = minify_source(&PathBuf::from("x.ts"), src);
        assert!(result.contains("hello"));
    }

    // ── C/C++ ───────────────────────────────────────────────────────

    #[test]
    fn test_minify_c_strips_comments() {
        let src = "int main() {\n    // comment\n    return 0;\n}";
        let result = minify_source(&PathBuf::from("x.c"), src);
        assert!(!result.contains("comment"));
        assert!(result.contains("return 0"));
    }

    #[test]
    fn test_minify_cpp_strips_block_comments() {
        let src = "/* block */ int x = 1;";
        let result = minify_source(&PathBuf::from("x.cpp"), src);
        assert!(!result.contains("block"));
        assert!(result.contains("int x"));
    }

    // ── C/C++ string-awareness regression tests ─────────────────────

    #[test]
    fn test_c_keeps_block_comment_marker_inside_string() {
        let src = r#"char *s = "/* not a comment */"; int x = 5;"#;
        let result = minify_source(&PathBuf::from("x.c"), src);
        assert!(
            result.contains(r#""/* not a comment */""#),
            "must not eat /* inside a string literal"
        );
    }

    #[test]
    fn test_c_keeps_double_slash_inside_string_url() {
        let src = r#"char *u = "http://example.com"; // real comment"#;
        let result = minify_source(&PathBuf::from("x.c"), src);
        assert!(
            result.contains(r#""http://example.com""#),
            "must keep URL with // inside string"
        );
        assert!(
            !result.contains("comment"),
            "must still strip real line comment"
        );
    }

    #[test]
    fn test_c_keeps_char_literal_of_a_quote() {
        let src = "char c = '\"'; // gone";
        let result = minify_source(&PathBuf::from("x.c"), src);
        assert!(
            result.contains("'\"'"),
            "char literal containing a double-quote must survive"
        );
    }

    #[test]
    fn test_c_line_marker_inside_block_comment_is_inert() {
        let src = "/* // not a line comment */ y();";
        let result = minify_source(&PathBuf::from("x.c"), src);
        assert!(result.contains("y();"));
    }

    #[test]
    fn test_c_block_marker_inside_line_comment_is_inert() {
        let src = "x(); // /* not a block start";
        let result = minify_source(&PathBuf::from("x.c"), src);
        assert!(result.contains("x();"));
        assert!(!result.contains("not a block"));
    }

    // ── Java ────────────────────────────────────────────────────────

    #[test]
    fn test_minify_java_strips_comments() {
        let src = "class Main {\n    // comment\n    int x = 1;\n}";
        let result = minify_source(&PathBuf::from("Main.java"), src);
        assert!(!result.contains("comment"));
        assert!(result.contains("int x"));
    }

    #[test]
    fn test_minify_java_strips_javadoc() {
        let src = "/** Javadoc */\nclass Main {}";
        let result = minify_source(&PathBuf::from("Main.java"), src);
        assert!(!result.contains("Javadoc"));
        assert!(result.contains("class Main"));
    }

    // ── Ruby ────────────────────────────────────────────────────────

    #[test]
    fn test_minify_ruby_strips_comments() {
        let src = "x = 1 # comment\ny = 2";
        // Our ruby minifier strips whole-line comments starting with #
        // (not inline — we're conservative)
        let result = minify_source(&PathBuf::from("x.rb"), src);
        assert!(result.contains("x = 1"));
        assert!(result.contains("y = 2"));
        // Blank lines collapsed
        assert!(!result.contains("\n\n\n"));
    }

    // ── Shell ───────────────────────────────────────────────────────

    #[test]
    fn test_minify_shell_strips_comments() {
        let src = "#!/bin/sh\n# comment\necho hello";
        let result = minify_source(&PathBuf::from("x.sh"), src);
        assert!(result.contains("#!/bin/sh"));
        assert!(!result.contains("comment"));
        assert!(result.contains("echo hello"));
    }

    // ── VFS Cache ───────────────────────────────────────────────────

    #[test]
    fn test_cache_results() {
        // Use a path unique to this test so we can assert THIS entry
        // exists in the cache, not the global size. The previous
        // version compared `minify_cache_size() > start_size`, which
        // is racy: parallel tests can both add AND evict entries, so
        // the size can decrease between two reads even when the test
        // is doing the right thing. Asserting on a specific key
        // removes the race entirely.
        let tmp = std::env::temp_dir().join(format!(
            "kirkforge_minify_cache_test_{}.txt",
            std::process::id()
        ));
        std::fs::write(&tmp, "x = 1 # comment").unwrap();

        // Sanity: the cache should not already contain this path
        // (a different process can't be touching this exact path
        // in the same test run, since the temp filename is unique
        // to this test).
        assert!(
            !cache_contains(&tmp),
            "cache unexpectedly contains the test path before minify_source"
        );

        // Touch the file to make sure mtime is current and unique
        // to this test (avoids a stale mtime colliding with a
        // previous test that used the same temp filename).
        let _ = std::fs::write(&tmp, "x = 1 # comment v2");
        let _ = minify_source(&tmp, "x = 1 # comment v2");

        // The path should now be in the cache. This is the only
        // assertion that matters — global size is irrelevant.
        assert!(
            cache_contains(&tmp),
            "cache should contain the minify result for the test path"
        );

        // Invalidate by path — the entry for THIS path should be gone
        // even if other parallel tests have populated other entries.
        invalidate_minify_cache(&tmp);
        assert!(
            !cache_contains(&tmp),
            "invalidate_minify_cache should remove the path's entry"
        );

        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn test_clear_cache() {
        clear_minify_cache();
        // May be populated by parallel tests immediately after clear,
        // so just verify the function doesn't panic
        let _ = minify_cache_size();
    }

    /// `cache_contains` returns false for a path that hasn't been
    /// minified yet (and doesn't even exist on disk).
    #[test]
    fn test_cache_contains_false_for_nonexistent_path() {
        let path = PathBuf::from("/tmp/this_path_definitely_does_not_exist_xyz123.rs");
        assert!(!cache_contains(&path));
    }

    /// `cache_contains` returns true after a minify, false after invalidate.
    ///
    /// Clears the global cache first so parallel tests that have already
    /// filled it cannot trigger an LRU-style eviction between the minify
    /// and the contains check.
    #[test]
    fn test_cache_contains_round_trip() {
        clear_minify_cache();

        let tmp = std::env::temp_dir().join("kirkforge_minify_cache_contains_test.rs");
        let _ = std::fs::remove_file(&tmp);
        std::fs::write(&tmp, "fn main() {}").unwrap();

        // Sanity
        assert!(!cache_contains(&tmp));
        let _ = minify_source(&tmp, "fn main() {}");
        assert!(cache_contains(&tmp), "should be cached after minify");
        invalidate_minify_cache(&tmp);
        assert!(!cache_contains(&tmp), "should be evicted after invalidate");

        let _ = std::fs::remove_file(&tmp);
    }

    // ── General ─────────────────────────────────────────────────────

    #[test]
    fn test_unknown_extension_preserved() {
        let src = "some text content";
        let result = minify_source(&PathBuf::from("x.txt"), src);
        assert_eq!(result, src);
    }

    #[test]
    fn test_savings_estimate() {
        let orig = "hello world";
        let min = "hello";
        let (saved, pct) = savings_estimate(orig, min);
        assert!(saved > 0);
        assert!(pct > 0.0);
    }

    #[test]
    fn test_collapse_blank_lines() {
        let src = "a\n\n\nb";
        let result = collapse_blank_lines(src);
        assert_eq!(result, "a\n\nb\n");
    }

    #[test]
    fn test_extra_newline_at_end_handled() {
        // Ensure trailing newlines don't cause issues
        let src = "x = 1;\ny = 2;\n\n";
        let result = collapse_blank_lines(src);
        assert!(result.contains("x = 1"));
        assert!(result.contains("y = 2"));
    }
}
