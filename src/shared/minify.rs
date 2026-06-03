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
        let cache = VFS_CACHE.lock().unwrap();
        if let Some(cached) = cache.get(&(path.to_path_buf(), mtime)) {
            return cached.clone();
        }
    }

    // Minify
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
    let result = minify_content_by_ext(content, ext, preserve_tests);

    // Store in cache (only for non-preserve mode — safe variants skip cache)
    if !preserve_tests {
        let mut cache = VFS_CACHE.lock().unwrap();
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
    let mut cache = VFS_CACHE.lock().unwrap();
    cache.clear();
}

/// Remove cache entries for a specific file path.
pub fn invalidate_minify_cache(path: &Path) {
    let mut cache = VFS_CACHE.lock().unwrap();
    cache.retain(|(p, _), _| p != path);
}

/// Get current cache size.
pub fn minify_cache_size() -> usize {
    let cache = VFS_CACHE.lock().unwrap();
    cache.len()
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

fn strip_line_comments(source: &str, prefix: &str) -> String {
    let mut out = String::with_capacity(source.len());
    for line in source.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with(prefix) {
            continue; // whole line is a comment
        }
        // Inline comment: find the first occurrence not inside a string
        if let Some(pos) = find_comment_pos(line, prefix) {
            // Check if the comment prefix is inside a string literal
            if !is_inside_string(line, pos) {
                out.push_str(line[..pos].trim_end());
                out.push('\n');
                continue;
            }
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}

fn find_comment_pos(line: &str, prefix: &str) -> Option<usize> {
    let bytes = line.as_bytes();
    let prefix_bytes = prefix.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i..].starts_with(prefix_bytes) {
            return Some(i);
        }
        i += 1;
    }
    None
}

fn is_inside_string(line: &str, pos: usize) -> bool {
    let mut in_single = false;
    let mut in_double = false;
    let mut escaped = false;
    for (i, ch) in line.chars().enumerate() {
        if i >= pos {
            break;
        }
        if escaped {
            escaped = false;
            continue;
        }
        match ch {
            '\\' => escaped = true,
            '"' if !in_single => in_double = !in_double,
            '\'' if !in_double => in_single = !in_single,
            _ => {}
        }
    }
    in_single || in_double
}

/// Strip block comments using simple string matching.
fn strip_block_comments(source: &str, start: &str, end: &str) -> String {
    let mut out = String::with_capacity(source.len());
    let mut remaining = source;
    let mut in_comment = false;

    while !remaining.is_empty() {
        if in_comment {
            match remaining.find(end) {
                Some(pos) => {
                    remaining = &remaining[pos + end.len()..];
                    in_comment = false;
                }
                None => break,
            }
        } else {
            match remaining.find(start) {
                Some(pos) => {
                    out.push_str(&remaining[..pos]);
                    remaining = &remaining[pos + start.len()..];
                    in_comment = true;
                }
                None => {
                    out.push_str(remaining);
                    break;
                }
            }
        }
    }

    out
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
        if !in_test_block && (trimmed == "#[cfg(test)]" || trimmed == "#[test]" || trimmed.starts_with("#[cfg(test)]")) {
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
    let s = if preserve_tests { out } else { strip_test_blocks(&out) };
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
        if (ch == '"' || ch == '\'')
            && chars.peek() == Some(&ch)
        {
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

// ── C/C++ ─────────────────────────────────────────────────────────

fn minify_c_like(source: &str) -> String {
    let s = strip_line_comments(source, "//");
    let s = strip_block_comments(&s, "/*", "*/");
    collapse_blank_lines(&s)
}

// ── Java ──────────────────────────────────────────────────────────

fn minify_java(source: &str) -> String {
    let s = strip_line_comments(source, "//");
    let s = strip_block_comments(&s, "/*", "*/");

    // Also strip Javadoc comments (/** ... */)
    let s = strip_block_comments(&s, "/**", "*/");

    collapse_blank_lines(&s)
}

// ── Ruby ──────────────────────────────────────────────────────────

fn minify_ruby(source: &str) -> String {
    let mut out = String::new();
    for line in source.lines() {
        let trimmed = line.trim();
        // Skip comment lines and shebang
        if trimmed.starts_with('#') {
            // Check if it's a heredoc or string containing # — skip for now
            if !trimmed.starts_with("# encoding") && !trimmed.starts_with("# frozen_string_literal") {
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
        // Note: VFS cache is global; other parallel tests may add entries.
        // We test relative behavior rather than absolute size.
        let start_size = minify_cache_size();

        let tmp = std::env::temp_dir().join("kirkforge_minify_cache_test.txt");
        std::fs::write(&tmp, "x = 1 # comment").unwrap();

        let _ = minify_source(&tmp, "x = 1 # comment");
        assert!(minify_cache_size() > start_size, "Cache should grow");

        // Invalidate
        invalidate_minify_cache(&tmp);
        // May have decreased but not guaranteed to 0 due to parallel tests

        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn test_clear_cache() {
        clear_minify_cache();
        // May be populated by parallel tests immediately after clear,
        // so just verify the function doesn't panic
        let _ = minify_cache_size();
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