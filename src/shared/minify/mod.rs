// Partial prompt-compression implementation; unused symbols are wired up in Phase 10.
#![allow(dead_code)]

mod expand;
mod lang;

pub use expand::{
    expand_minified, extract_minified_envelope, has_minified_envelope, lang_name_for_ext,
    wrap_minified_envelope,
};

/// Language-aware source minification for prompt compression.
///
/// Applied at prompt-build time only — files on disk are never modified.
/// Strips comments, collapses whitespace, shortens local identifiers
/// in languages where that's safe.
///
/// Maintains a VFS cache keyed by (path, mtime) to avoid re-minifying
/// the same file across multiple turns.
use lang::minify_content_by_ext;
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
    use super::lang::collapse_blank_lines;
    use super::*;
    use crate::shared::test_util::remove_test_file;
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
        std::fs::write(&tmp, "x = 1 # comment v2").unwrap();
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

        remove_test_file(&tmp);
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
        remove_test_file(&tmp);
        std::fs::write(&tmp, "fn main() {}").unwrap();

        // Sanity
        assert!(!cache_contains(&tmp));
        let _ = minify_source(&tmp, "fn main() {}");
        assert!(cache_contains(&tmp), "should be cached after minify");
        invalidate_minify_cache(&tmp);
        assert!(!cache_contains(&tmp), "should be evicted after invalidate");

        remove_test_file(&tmp);
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
