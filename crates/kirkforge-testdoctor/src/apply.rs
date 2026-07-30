//! Apply a suggestion to a test file (WO 12.6).
//!
//! Text-based rewriting for v1 — no `syn`. The apply command is opt-in
//! and potentially destructive: it always returns the diff first, and
//! only writes to disk when `yes == true`. If a pattern doesn't match
//! exactly one site, it returns an error rather than guessing.
//!
//! Supported fix kinds (v1 ships 3):
//! - `IgnoreSlow` — add `#[ignore = "slow: <reason>"]` above a test fn.
//! - `TokioStartPaused` — wrap `#[tokio::test]` with `start_paused = true`.
//! - `EnvGuard` — replace `std::env::set_var(K, V)` with an
//!   `EnvGuard::set(K, V)` call (textual; does not add the import).

use std::path::Path;

use anyhow::{bail, Result};
use regex::Regex;

use crate::suggest::{Suggestion, SuggestionKind};

/// Apply a suggestion to a test file. Returns the unified diff of the
/// change. When `yes` is true, also writes the new content to
/// `test_path`; when false, leaves the file untouched (dry-run).
///
/// The diff is hand-rolled (no `similar` dep) — it's a minimal unified
/// diff with `---`/`+++` headers and `@@` hunks. Good enough for the
/// doctor's "show the diff, then apply with --yes" contract.
pub fn apply_suggestion(test_path: &Path, suggestion: &Suggestion, yes: bool) -> Result<String> {
    let original = std::fs::read_to_string(test_path)
        .map_err(|e| anyhow::anyhow!("failed to read {}: {e}", test_path.display()))?;
    let updated = rewrite(&original, suggestion)?;
    let diff = render_unified_diff(test_path, &original, &updated);
    if yes {
        std::fs::write(test_path, &updated)
            .map_err(|e| anyhow::anyhow!("failed to write {}: {e}", test_path.display()))?;
    }
    Ok(diff)
}

/// Pure rewrite: produce the new source text from the original + the
/// suggestion. Returns an error if the pattern doesn't match exactly
/// one site (conservative — never guess).
fn rewrite(src: &str, suggestion: &Suggestion) -> Result<String> {
    match suggestion.kind {
        SuggestionKind::IgnoreSlow => apply_ignore_slow(src, &suggestion.test),
        SuggestionKind::TokioStartPaused => apply_tokio_start_paused(src, &suggestion.test),
        SuggestionKind::EnvGuard => apply_env_guard(src),
        // The remaining kinds (MockSubprocess, Wiremock, NamedTempFile)
        // are suggestion-only for v1 — they require factoring code into
        // a trait or introducing a new dep, which a text rewrite can't
        // do safely. The doctor prints the suggestion; the human edits.
        SuggestionKind::MockSubprocess
        | SuggestionKind::Wiremock
        | SuggestionKind::NamedTempFile => bail!(
            "suggestion kind {:?} is suggestion-only for v1; apply is not \
             supported. Apply the fix by hand.",
            suggestion.kind
        ),
    }
}

/// Add `#[ignore = "slow: <reason>"]` above the test fn named `test`.
/// Matches `fn <test>(` on its own line (allowing leading whitespace
/// and `async`). Inserts the attribute on the line above. Errors if
/// the fn is not found, or if it already carries an `#[ignore]`.
fn apply_ignore_slow(src: &str, test: &str) -> Result<String> {
    let re = Regex::new(&format!(r"^([ \t]*(?:async\s+)?fn\s+{test}\s*\()"))
        .map_err(|e| anyhow::anyhow!("internal: bad regex: {e}"))?;
    let lines: Vec<&str> = src.lines().collect();
    let mut out = String::with_capacity(src.len() + 64);
    let mut matched = 0u32;
    for (i, line) in lines.iter().enumerate() {
        if re.is_match(line) {
            // Bail if the fn already has an `#[ignore]` directly above.
            if i > 0 && lines[i - 1].trim().starts_with("#[ignore") {
                bail!("test fn `{test}` already carries an #[ignore] attribute");
            }
            out.push_str("#[ignore = \"slow: testdoctor auto-ignore\"]\n");
            out.push_str(line);
            out.push('\n');
            matched += 1;
        } else {
            out.push_str(line);
            out.push('\n');
        }
    }
    // Drop the trailing newline we added if the original didn't have one.
    if !src.ends_with('\n') && out.ends_with('\n') {
        out.pop();
    }
    match matched {
        0 => bail!("test fn `{test}` not found in source"),
        1 => Ok(out),
        _ => bail!("test fn `{test}` matched {matched} sites; apply refuses to guess"),
    }
}

/// Wrap `#[tokio::test]` (with or without existing args) above the test
/// fn named `test` with `#[tokio::test(start_paused = true)]`. Errors
/// if the fn has no `#[tokio::test]`, or if it already has
/// `start_paused`.
fn apply_tokio_start_paused(src: &str, test: &str) -> Result<String> {
    let fn_re = Regex::new(&format!(r"^([ \t]*(?:async\s+)?fn\s+{test}\s*\()"))
        .map_err(|e| anyhow::anyhow!("internal: bad regex: {e}"))?;
    let tokio_attr_re = Regex::new(r"^([ \t]*)#\[tokio::test(?:\(([^)]*)\))?\]")
        .map_err(|e| anyhow::anyhow!("internal: bad regex: {e}"))?;

    let lines: Vec<&str> = src.lines().collect();
    // First pass: locate the fn line + its preceding `#[tokio::test]`.
    let mut fn_idx = None;
    let mut attr_idx = None;
    let mut indent = "";
    let mut existing_args = "";
    for (i, line) in lines.iter().enumerate() {
        if fn_re.is_match(line) {
            if i == 0 {
                bail!("test fn `{test}` has no `#[tokio::test]` attribute above it");
            }
            let prev = lines[i - 1];
            let caps = match tokio_attr_re.captures(prev) {
                Some(c) => c,
                None => {
                    bail!("test fn `{test}` is not preceded by `#[tokio::test]` (found `{prev}`)")
                }
            };
            indent = caps.get(1).map(|m| m.as_str()).unwrap_or("");
            existing_args = caps.get(2).map(|m| m.as_str()).unwrap_or("");
            if existing_args.contains("start_paused") {
                bail!("test fn `{test}` already has `start_paused` set");
            }
            fn_idx = Some(i);
            attr_idx = Some(i - 1);
            break;
        }
    }
    let (_fn_idx, attr_idx) = match (fn_idx, attr_idx) {
        (Some(f), Some(a)) => (f, a),
        _ => bail!("test fn `{test}` not found in source"),
    };

    // Second pass: rebuild with the new attribute line. Only the attr
    // line changes; everything else is copied verbatim.
    let new_args = if existing_args.is_empty() {
        "start_paused = true".to_string()
    } else {
        format!("{existing_args}, start_paused = true")
    };
    let new_attr = format!("{indent}#[tokio::test({new_args})]");

    let mut out = String::with_capacity(src.len() + 64);
    for (i, line) in lines.iter().enumerate() {
        if i == attr_idx {
            out.push_str(&new_attr);
        } else {
            out.push_str(line);
        }
        out.push('\n');
    }
    if !src.ends_with('\n') && out.ends_with('\n') {
        out.pop();
    }
    Ok(out)
}

/// Replace `std::env::set_var(K, V)` with `EnvGuard::set(K, V)`. This
/// is a textual replacement — it does NOT add the `use` import (the
/// doctor prints a note about the import in the rationale). Errors if
/// no `std::env::set_var(` call is found, or if more than one is found
/// (apply refuses to guess which one to rewrite).
fn apply_env_guard(src: &str) -> Result<String> {
    let re = Regex::new(r"\bstd::env::set_var\(")
        .map_err(|e| anyhow::anyhow!("internal: bad regex: {e}"))?;
    let matches: Vec<_> = re.find_iter(src).collect();
    if matches.is_empty() {
        bail!("no `std::env::set_var(` call found in source");
    }
    if matches.len() > 1 {
        bail!(
            "found {} `std::env::set_var(` calls; apply refuses to guess which to rewrite",
            matches.len()
        );
    }
    let new = re.replace(src, "EnvGuard::set(");
    Ok(new.into_owned())
}

/// Render a minimal unified diff between `original` and `updated`.
/// Hand-rolled — no `similar` dep. The format is:
///
/// ```text
/// --- a/<path>
/// +++ b/<path>
/// @@ -<line>,<count> +<line>,<count> @@
///  context
/// -removed
/// +added
/// ```
///
/// This is a line-level diff: we walk both texts line by line and emit
/// hunks where they differ. It's O(n*m) in the worst case but fine for
/// test files (a few hundred lines).
fn render_unified_diff(path: &Path, original: &str, updated: &str) -> String {
    let a: Vec<&str> = original.lines().collect();
    let b: Vec<&str> = updated.lines().collect();
    let mut out = String::new();
    out.push_str(&format!("--- a/{}\n", path.display()));
    out.push_str(&format!("+++ b/{}\n", path.display()));

    // Find the first and last differing line indices in each side.
    let mut a_start = None;
    let mut a_end = None;
    let mut b_start = None;
    let mut b_end = None;
    let mut ai = 0usize;
    let mut bi = 0usize;
    while ai < a.len() || bi < b.len() {
        let a_line = a.get(ai);
        let b_line = b.get(bi);
        if a_line == b_line {
            ai += 1;
            bi += 1;
            continue;
        }
        // Lines differ. We need to find the resync point — the next
        // line in `a` that matches a line in `b` near the current
        // position. For a single-hunk, line-level diff this is good
        // enough: walk forward in `b` until we find a line that
        // matches `a[ai]`, then walk forward in `a` for the rest.
        if a_start.is_none() {
            a_start = Some(ai);
            b_start = Some(bi);
        }
        a_end = Some(ai);
        b_end = Some(bi);
        // Advance both; if they resync later, the loop will catch up.
        if ai < a.len() {
            ai += 1;
        }
        if bi < b.len() {
            bi += 1;
        }
    }

    let (a_start, a_end, b_start, b_end) = match (a_start, a_end, b_start, b_end) {
        (Some(s), Some(e), Some(bs), Some(be)) => (s, e, bs, be),
        // No differences.
        _ => return out,
    };

    // Include one line of context on each side (clamped to bounds).
    let ctx = 1;
    let a_ctx_start = a_start.saturating_sub(ctx);
    let b_ctx_start = b_start.saturating_sub(ctx);
    let a_ctx_end = (a_end + ctx).min(a.len().saturating_sub(1));
    let b_ctx_end = (b_end + ctx).min(b.len().saturating_sub(1));

    let a_count = a_ctx_end.saturating_sub(a_ctx_start) + 1;
    let b_count = b_ctx_end.saturating_sub(b_ctx_start) + 1;

    // Unified diff line numbers are 1-indexed.
    out.push_str(&format!(
        "@@ -{},{} +{},{} @@\n",
        a_ctx_start + 1,
        a_count,
        b_ctx_start + 1,
        b_count
    ));

    // Emit the hunk: context lines (space prefix), removed (-), added (+).
    // We walk the context window on both sides; where they match, emit
    // a context line; where they differ, emit - then +.
    let mut ai = a_ctx_start;
    let mut bi = b_ctx_start;
    while ai <= a_ctx_end || bi <= b_ctx_end {
        let a_line = a.get(ai);
        let b_line = b.get(bi);
        match (a_line, b_line) {
            (Some(a), Some(b)) if a == b => {
                out.push(' ');
                out.push_str(a);
                out.push('\n');
                ai += 1;
                bi += 1;
            }
            (Some(a), _) => {
                out.push('-');
                out.push_str(a);
                out.push('\n');
                ai += 1;
            }
            (_, Some(b)) => {
                out.push('+');
                out.push_str(b);
                out.push('\n');
                bi += 1;
            }
            (None, None) => break,
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::suggest::{Suggestion, SuggestionKind};

    fn sug(test: &str, kind: SuggestionKind) -> Suggestion {
        Suggestion {
            id: format!("{test}::x"),
            test: test.to_string(),
            severity: "medium".to_string(),
            fix: "fix".to_string(),
            rationale: "rationale".to_string(),
            kind,
        }
    }

    fn write_tmp(body: &str) -> (tempfile::NamedTempFile, String) {
        let f = tempfile::Builder::new()
            .suffix(".rs")
            .tempfile()
            .expect("create temp file");
        std::fs::write(f.path(), body).expect("write temp");
        (f, body.to_string())
    }

    #[test]
    fn apply_ignore_slow_adds_attribute() {
        let (f, _) = write_tmp("fn test_foo() { assert!(true); }\n");
        let s = sug("test_foo", SuggestionKind::IgnoreSlow);
        let diff = apply_suggestion(f.path(), &s, false).expect("dry-run diff");
        assert!(diff.contains("-fn test_foo"));
        assert!(diff.contains("+#[ignore = \"slow: testdoctor auto-ignore\"]"));
        assert!(diff.contains("+fn test_foo"));
        // Dry-run: file untouched.
        let after = std::fs::read_to_string(f.path()).expect("read");
        assert_eq!(after, "fn test_foo() { assert!(true); }\n");
    }

    #[test]
    fn apply_ignore_slow_yes_writes_file() {
        let (f, _) = write_tmp("fn test_foo() { assert!(true); }\n");
        let s = sug("test_foo", SuggestionKind::IgnoreSlow);
        let _ = apply_suggestion(f.path(), &s, true).expect("apply");
        let after = std::fs::read_to_string(f.path()).expect("read");
        assert!(
            after.contains("#[ignore = \"slow: testdoctor auto-ignore\"]\nfn test_foo"),
            "file should now carry the ignore attribute: {after}"
        );
    }

    #[test]
    fn apply_ignore_slow_async_fn_matches() {
        let (f, _) = write_tmp("async fn test_bar() -> Result<()> { Ok(()) }\n");
        let s = sug("test_bar", SuggestionKind::IgnoreSlow);
        let diff = apply_suggestion(f.path(), &s, false).expect("dry-run diff");
        assert!(diff.contains("async fn test_bar"));
    }

    #[test]
    fn apply_ignore_slow_missing_fn_errors() {
        let (f, _) = write_tmp("fn test_other() {}\n");
        let s = sug("test_missing", SuggestionKind::IgnoreSlow);
        let err = apply_suggestion(f.path(), &s, false).unwrap_err();
        assert!(err.to_string().contains("not found"));
    }

    #[test]
    fn apply_ignore_slow_already_ignored_errors() {
        let (f, _) = write_tmp("#[ignore = \"existing\"]\nfn test_foo() { assert!(true); }\n");
        let s = sug("test_foo", SuggestionKind::IgnoreSlow);
        let err = apply_suggestion(f.path(), &s, false).unwrap_err();
        assert!(err.to_string().contains("already carries"));
    }

    #[test]
    fn apply_tokio_start_paused_adds_arg() {
        let (f, _) = write_tmp("#[tokio::test]\nasync fn test_foo() { assert!(true); }\n");
        let s = sug("test_foo", SuggestionKind::TokioStartPaused);
        let diff = apply_suggestion(f.path(), &s, false).expect("dry-run diff");
        assert!(diff.contains("-#[tokio::test]"));
        assert!(diff.contains("+#[tokio::test(start_paused = true)]"));
    }

    #[test]
    fn apply_tokio_start_paused_preserves_existing_args() {
        let (f, _) =
            write_tmp("#[tokio::test(flavor = \"multi_thread\")]\nasync fn test_foo() {}\n");
        let s = sug("test_foo", SuggestionKind::TokioStartPaused);
        let diff = apply_suggestion(f.path(), &s, false).expect("dry-run diff");
        assert!(diff.contains("+#[tokio::test(flavor = \"multi_thread\", start_paused = true)]"));
    }

    #[test]
    fn apply_tokio_start_paused_already_set_errors() {
        let (f, _) = write_tmp("#[tokio::test(start_paused = true)]\nasync fn test_foo() {}\n");
        let s = sug("test_foo", SuggestionKind::TokioStartPaused);
        let err = apply_suggestion(f.path(), &s, false).unwrap_err();
        assert!(err.to_string().contains("already has `start_paused`"));
    }

    #[test]
    fn apply_tokio_start_paused_no_attr_errors() {
        let (f, _) = write_tmp("async fn test_foo() {}\n");
        let s = sug("test_foo", SuggestionKind::TokioStartPaused);
        let err = apply_suggestion(f.path(), &s, false).unwrap_err();
        assert!(err.to_string().contains("no `#[tokio::test]`"));
    }

    #[test]
    fn apply_env_guard_replaces_call() {
        let (f, _) = write_tmp("fn test_foo() {\n    std::env::set_var(\"FOO\", \"bar\");\n}\n");
        let s = sug("test_foo", SuggestionKind::EnvGuard);
        let diff = apply_suggestion(f.path(), &s, false).expect("dry-run diff");
        assert!(diff.contains("-    std::env::set_var(\"FOO\", \"bar\");"));
        assert!(diff.contains("+    EnvGuard::set(\"FOO\", \"bar\");"));
    }

    #[test]
    fn apply_env_guard_no_call_errors() {
        let (f, _) = write_tmp("fn test_foo() { assert!(true); }\n");
        let s = sug("test_foo", SuggestionKind::EnvGuard);
        let err = apply_suggestion(f.path(), &s, false).unwrap_err();
        assert!(err.to_string().contains("no `std::env::set_var"));
    }

    #[test]
    fn apply_env_guard_multiple_calls_errors() {
        let (f, _) = write_tmp(
            "fn test_foo() {\n    std::env::set_var(\"A\", \"1\");\n    std::env::set_var(\"B\", \"2\");\n}\n",
        );
        let s = sug("test_foo", SuggestionKind::EnvGuard);
        let err = apply_suggestion(f.path(), &s, false).unwrap_err();
        assert!(err.to_string().contains("refuses to guess"));
    }

    #[test]
    fn apply_mock_subprocess_is_suggestion_only() {
        let (f, _) =
            write_tmp("fn test_foo() { let s = std::process::Command::new(\"x\").output(); }\n");
        let s = sug("test_foo", SuggestionKind::MockSubprocess);
        let err = apply_suggestion(f.path(), &s, false).unwrap_err();
        assert!(err.to_string().contains("suggestion-only"));
    }

    #[test]
    fn apply_yes_false_does_not_write() {
        let (f, _) = write_tmp("fn test_foo() { assert!(true); }\n");
        let s = sug("test_foo", SuggestionKind::IgnoreSlow);
        let _ = apply_suggestion(f.path(), &s, false).expect("dry-run");
        assert_eq!(
            std::fs::read_to_string(f.path()).unwrap(),
            "fn test_foo() { assert!(true); }\n"
        );
    }

    #[test]
    fn render_diff_no_changes_is_header_only() {
        let path = Path::new("foo.rs");
        let d = render_unified_diff(path, "x\n", "x\n");
        assert!(d.starts_with("--- a/foo.rs"));
        assert!(!d.contains("@@"));
    }
}
