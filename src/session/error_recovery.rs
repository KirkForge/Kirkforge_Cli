//! Error recovery — smart retry hints after tool failures.
//!
//! When a tool call fails (file not found, build error, permission denied),
//! this module analyzes the error and provides the model with a correction
//! hint as a follow-up user message. This lets the model self-correct instead
//! of spinning on the same failed approach.
//!
//! # Retry limits
//!
//! - Max 2 retries per turn (the third failure stops the loop)
//! - Each retry includes the specific error message and a targeted hint
//! - The retry count is tracked per-turn, not per-tool

use crate::shared::backoff::retry_backoff;
use crate::shared::{Message, Role};
use regex::Regex;

/// A recovery hint: what went wrong and how to fix it.
#[derive(Debug, Clone)]
pub struct RecoveryHint {
    /// The error message from the tool (truncated).
    pub error_summary: String,
    /// The suggested corrective action.
    pub suggestion: String,
    /// Whether this error is considered recoverable.
    pub recoverable: bool,
}

/// Structured classification of a common compile-time error.
///
/// `ErrorHint` is produced by the classifier functions below. It captures
/// *which* well-known failure happened so the recovery message can name it
/// precisely and the verifier verdict can carry it forward. The variants
/// are deliberately coarse — a full rustc/clippy parse is out of scope.
///
/// The enum is `Serialize`/`Deserialize` so verifiers can attach a hint
/// to a `Verdict` and tests can compare exact values.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ErrorHint {
    /// Two borrows overlap; both are still live at the same point.
    BorrowConflict {
        original_ref: String,
        conflicting_ref: String,
    },
    /// A name is used but not in scope; the classifier suggests a module.
    MissingImport {
        symbol: String,
        suggested_module: String,
    },
    /// An expression of one type was used where another was expected.
    TypeMismatch {
        expected: String,
        found: String,
    },
    /// A method was called on a type that does not implement it.
    MissingMethod {
        type_name: String,
        method_name: String,
        suggested_traits: Vec<String>,
    },
}

/// Render a `ErrorHint` as a single short note suitable for the model.
///
/// The output is stable and human-readable. The note is prefixed with
/// "Hint:" so the model can tell it apart from the raw tool error.
pub fn render_hint(hint: &ErrorHint) -> String {
    match hint {
        ErrorHint::BorrowConflict {
            original_ref,
            conflicting_ref,
        } => format!(
            "Hint: this looks like a borrow conflict. The reference `{original_ref}` is \
             still alive when `{conflicting_ref}` tries to borrow it. Consider cloning \
             `{original_ref}` or restructuring the borrows so their lifetimes do not overlap."
        ),
        ErrorHint::MissingImport {
            symbol,
            suggested_module,
        } => format!(
            "Hint: `{symbol}` is not in scope. Try adding `use {suggested_module}::{symbol};`."
        ),
        ErrorHint::TypeMismatch { expected, found } => format!(
            "Hint: expected `{expected}` but found `{found}`. Consider using a conversion \
             (e.g. `.into()` or `.try_into()`) or fixing the type annotation."
        ),
        ErrorHint::MissingMethod {
            type_name,
            method_name,
            suggested_traits,
        } => {
            let traits = if suggested_traits.is_empty() {
                "the trait that provides it".to_string()
            } else {
                format!("one of: {}", suggested_traits.join(", "))
            };
            format!(
                "Hint: `{type_name}` does not have a method named `{method_name}`. \
                 Consider importing {traits}, or implementing the method on `{type_name}`."
            )
        }
    }
}

/// Try to classify a tool error as a borrow conflict.
///
/// Matches canonical rustc output, for example:
/// `cannot borrow `x` as mutable ... also borrowed as immutable`
fn classify_borrow_conflict(error: &str) -> Option<ErrorHint> {
    // Pull the two backtick-quoted identifiers out of the line. The
    // first quoted token after "also borrowed as" (or "also mutably
    // borrowed") is the name of the borrow that is *still alive*; the
    // first quoted token after "cannot borrow" is the name of the
    // borrow that *conflicts*. This order is consistent across rustc
    // editions and clippy rewrites of the same diagnostic.
    let re_borrower = Regex::new(r"cannot borrow `([^`]+)`").ok()?;
    let re_original = Regex::new(r"also (?:mutably )?borrowed as `([^`]+)`").ok()?;
    let lower = error.to_lowercase();
    if !lower.contains("cannot borrow") {
        return None;
    }
    let conflicting_ref = re_borrower
        .captures(error)
        .and_then(|c| c.get(1))
        .map(|m| m.as_str().to_string())?;
    let original_ref = re_original
        .captures(error)
        .and_then(|c| c.get(1))
        .map(|m| m.as_str().to_string())
        .unwrap_or_else(|| conflicting_ref.clone());
    Some(ErrorHint::BorrowConflict {
        original_ref,
        conflicting_ref,
    })
}

/// Try to classify a tool error as a missing-import / missing-name error.
///
/// Matches rustc's "cannot find value `X` in this scope" and
/// "cannot find type `X` in this scope" diagnostics.
fn classify_missing_import(error: &str) -> Option<ErrorHint> {
    let lower = error.to_lowercase();
    let is_missing_value =
        lower.contains("cannot find value") && lower.contains("in this scope");
    let is_missing_type = lower.contains("cannot find type") && lower.contains("in this scope");
    if !(is_missing_value || is_missing_type) {
        return None;
    }
    let re = Regex::new(r"cannot find (?:value|type) `([^`]+)`").ok()?;
    let symbol = re
        .captures(error)
        .and_then(|c| c.get(1))
        .map(|m| m.as_str().to_string())?;
    // No deterministic way to know the right module path from a free-form
    // error message; we suggest the crate root and let the model resolve
    // it via context.
    Some(ErrorHint::MissingImport {
        symbol,
        suggested_module: "crate".to_string(),
    })
}

/// Try to classify a tool error as a type-mismatch error.
///
/// Matches rustc's "expected `T`, found `U`" pattern.
fn classify_type_mismatch(error: &str) -> Option<ErrorHint> {
    let lower = error.to_lowercase();
    if !lower.contains("expected") || !lower.contains("found") {
        return None;
    }
    // `expected \`T\`, found \`U\`` may appear with extra prose on either
    // side (e.g. on a separate line of the rendered diagnostic). Match
    // on the canonical short form.
    let re = Regex::new(r"expected `([^`]+)`, found `([^`]+)`").ok()?;
    let caps = re.captures(error)?;
    let expected = caps.get(1)?.as_str().to_string();
    let found = caps.get(2)?.as_str().to_string();
    if expected.is_empty() || found.is_empty() || expected == found {
        return None;
    }
    Some(ErrorHint::TypeMismatch { expected, found })
}

/// Try to classify a tool error as a missing-method error.
///
/// Matches rustc's "no method named `m` found for type `T`" pattern.
fn classify_missing_method(error: &str) -> Option<ErrorHint> {
    let lower = error.to_lowercase();
    if !lower.contains("no method named") {
        return None;
    }
    let re = Regex::new(r"no method named `([^`]+)` found for (?:type|struct|enum) `([^`]+)`")
        .ok()?;
    let caps = re.captures(error)?;
    let method_name = caps.get(1)?.as_str().to_string();
    let type_name = caps.get(2)?.as_str().to_string();
    if method_name.is_empty() || type_name.is_empty() {
        return None;
    }
    Some(ErrorHint::MissingMethod {
        type_name,
        method_name,
        suggested_traits: Vec::new(),
    })
}

/// Run every classifier against `error` and return the first match.
///
/// Order is intentional: borrow-conflict and missing-import diagnostics
/// frequently appear in the same compiler invocation, and borrow-conflict
/// messages include "expected" / "found" tokens, so borrow-conflict goes
/// first.
pub fn classify_error(error: &str) -> Option<ErrorHint> {
    if let Some(h) = classify_borrow_conflict(error) {
        return Some(h);
    }
    if let Some(h) = classify_missing_import(error) {
        return Some(h);
    }
    if let Some(h) = classify_missing_method(error) {
        return Some(h);
    }
    if let Some(h) = classify_type_mismatch(error) {
        return Some(h);
    }
    None
}

/// Analyze a tool error and produce a recovery hint.
///
/// Returns `None` if the error is not something we can give a useful hint for.
pub fn analyze_error(
    tool_name: &str,
    error_message: &str,
    args: &serde_json::Value,
) -> Option<RecoveryHint> {
    let err_lower = error_message.to_lowercase();

    // File not found patterns (after command-not-found check so "not found"
    // in tool output doesn't capture "command not found" errors)
    if err_lower.contains("no such file")
        || (err_lower.contains("not found") && !err_lower.contains("command"))
    {
        let path_hint = args
            .get("path")
            .and_then(|v| v.as_str())
            .or_else(|| args.get("file_path").and_then(|v| v.as_str()))
            .unwrap_or("the file");

        return Some(RecoveryHint {
            error_summary: format!("File not found: {path_hint}"),
            suggestion: format!(
                "The file '{path_hint}' was not found. Try:\n\
                 1. Use `glob` to search for the correct file path\n\
                 2. Use `grep` to search for code that references this file\n\
                 3. If this is a new file you're creating, use `write_file` instead"
            ),
            recoverable: true,
        });
    }

    // Permission denied
    if err_lower.contains("permission denied") || err_lower.contains("access denied") {
        let path_hint = args
            .get("path")
            .and_then(|v| v.as_str())
            .unwrap_or("the target");

        return Some(RecoveryHint {
            error_summary: format!("Permission denied: {path_hint}"),
            suggestion: format!(
                "Access was denied for '{path_hint}'. Try:\n\
                 1. Check if you have write permissions in this directory\n\
                 2. Consider using a different output path (e.g., /tmp/)\n\
                 3. If this is a system file, the change may need to be applied differently"
            ),
            recoverable: true,
        });
    }

    // Build/compile errors
    if tool_name == "bash"
        && (err_lower.contains("error:")
            || err_lower.contains("failed")
            || err_lower.contains("cannot find"))
    {
        if err_lower.contains("cargo") || err_lower.contains("rustc") {
            return Some(RecoveryHint {
                error_summary: "Build/compile error".to_string(),
                suggestion: "The build failed. Read the error output carefully — \
                    the compiler tells you exactly what line is wrong and often \
                    suggests the fix. Use `read_file` to view the offending file, \
                    then `edit_file` to fix the specific issue."
                    .to_string(),
                recoverable: true,
            });
        }

        if err_lower.contains("npm") || err_lower.contains("node") || err_lower.contains("tsc") {
            return Some(RecoveryHint {
                error_summary: "JavaScript/TypeScript build error".to_string(),
                suggestion: "The build failed. Check the error output for the specific \
                    file and line number. Use `read_file` to inspect the file and \
                    `edit_file` to fix the issue."
                    .to_string(),
                recoverable: true,
            });
        }
    }

    // Command not found (check before generic "not found" to avoid false match)
    if err_lower.contains("command not found") || err_lower.contains("not recognized") {
        return Some(RecoveryHint {
            error_summary: "Command not found".to_string(),
            suggestion: "The command you tried to run doesn't exist. Check:\n\
                     1. Is the tool installed? Try `which <command>`\n\
                     2. Do you need to install it first? Use the package manager\n\
                     3. Is there an alternative tool you can use?"
                .to_string(),
            recoverable: true,
        });
    }

    // Connection/network errors
    if err_lower.contains("connection")
        || err_lower.contains("timeout")
        || err_lower.contains("network")
    {
        return Some(RecoveryHint {
            error_summary: "Network error".to_string(),
            suggestion: "A network operation failed. Retry may succeed — \
                network issues are often transient. If this persists, check \
                connectivity with a simple command like `curl`."
                .to_string(),
            recoverable: true,
        });
    }

    None
}

/// Build a recovery message to append to the conversation.
///
/// This is sent as a user message so the model can read it and adjust.
pub fn build_recovery_message(hint: &RecoveryHint) -> Message {
    Message {
        role: Role::User,
        content: format!(
            "The previous action failed: {}\n\n{}\n\nPlease correct the issue and try again. \
             Do NOT repeat the same failing command — use the suggestions above.",
            hint.error_summary, hint.suggestion
        ),
        content_parts: None,
        thinking: None,
        tool_calls: None,
        tool_call_id: None,
        tool_name: None,
        token_count: None,
    }
}

/// Track retry state within a turn.
#[derive(Debug, Clone, Default)]
pub struct RetryTracker {
    /// Number of error-recovery retries attempted this turn.
    pub retry_count: usize,
    /// Maximum retries allowed per turn.
    pub max_retries: usize,
}

impl RetryTracker {
    pub fn new() -> Self {
        Self {
            retry_count: 0,
            max_retries: 2,
        }
    }

    /// Returns true if we should still attempt recovery.
    pub fn can_retry(&self) -> bool {
        self.retry_count < self.max_retries
    }

    /// Sleep before the next retry using exponential backoff.
    ///
    /// The first retry waits ~1 s, the second ~2 s, the third ~4 s, etc.
    /// `record_retry()` should be called *after* this returns.
    pub async fn wait_before_retry(&self) {
        let attempt = self.retry_count as u32 + 1;
        tokio::time::sleep(retry_backoff(attempt)).await;
    }

    /// Record a retry attempt.
    pub fn record_retry(&mut self) {
        self.retry_count += 1;
    }

    /// Reset for a new turn.
    pub fn reset(&mut self) {
        self.retry_count = 0;
    }
}

#[cfg(test)]
mod retry_tests {
    use super::*;

    #[tokio::test]
    async fn test_two_retries_wait_at_least_one_and_a_half_seconds() {
        let tracker = RetryTracker {
            retry_count: 0,
            max_retries: 2,
        };
        let start = std::time::Instant::now();

        tracker.wait_before_retry().await;
        assert!(
            start.elapsed() >= std::time::Duration::from_secs(1),
            "first retry should wait at least 1 s"
        );

        tracker.wait_before_retry().await;
        assert!(
            start.elapsed() >= std::time::Duration::from_millis(1500),
            "two retries should wait at least ~1.5 s total after the first sleep"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_analyze_file_not_found() {
        let args = serde_json::json!({"path": "src/lib.rs"});
        let hint =
            analyze_error("read_file", "No such file or directory: src/lib.rs", &args).unwrap();
        assert!(hint.suggestion.contains("glob"));
        assert!(hint.recoverable);
    }

    #[test]
    fn test_analyze_permission_denied() {
        let args = serde_json::json!({"path": "/etc/shadow"});
        let hint = analyze_error("read_file", "Permission denied", &args).unwrap();
        assert!(hint.suggestion.contains("permissions"));
        assert!(hint.recoverable);
    }

    #[test]
    fn test_analyze_build_error() {
        let args = serde_json::json!({"command": "cargo build"});
        let hint = analyze_error(
            "bash",
            "error: failed to compile `foo`\ncargo failed with exit code 101",
            &args,
        )
        .unwrap();
        assert!(hint.suggestion.contains("read_file"));
        assert!(hint.suggestion.contains("edit_file"));
        assert!(hint.recoverable);
    }

    #[test]
    fn test_analyze_command_not_found() {
        let args = serde_json::json!({"command": "nonexistent-tool"});
        let hint =
            analyze_error("bash", "bash: nonexistent-tool: command not found", &args).unwrap();
        assert!(hint.suggestion.contains("installed"));
        assert!(hint.recoverable);
    }

    #[test]
    fn test_analyze_unknown_error_returns_none() {
        let args = serde_json::json!({"path": "ok.txt"});
        let hint = analyze_error("read_file", "some unparseable error", &args);
        assert!(hint.is_none());
    }

    #[test]
    fn test_retry_tracker() {
        let mut tracker = RetryTracker::new();
        assert!(tracker.can_retry());
        tracker.record_retry();
        assert!(tracker.can_retry());
        tracker.record_retry();
        assert!(!tracker.can_retry(), "should max out at 2 retries");
        tracker.reset();
        assert!(tracker.can_retry(), "reset should clear counter");
    }
}

#[cfg(test)]
mod hint_tests {
    use super::*;

    #[test]
    fn render_borrow_conflict_hint_mentions_both_refs() {
        let hint = ErrorHint::BorrowConflict {
            original_ref: "x".to_string(),
            conflicting_ref: "y".to_string(),
        };
        let s = render_hint(&hint);
        assert!(s.starts_with("Hint:"));
        assert!(s.contains("`x`"));
        assert!(s.contains("`y`"));
        assert!(s.contains("borrow"));
    }

    #[test]
    fn render_missing_import_hint_names_symbol_and_module() {
        let hint = ErrorHint::MissingImport {
            symbol: "Foo".to_string(),
            suggested_module: "crate::bar".to_string(),
        };
        let s = render_hint(&hint);
        assert!(s.contains("`Foo`"));
        assert!(s.contains("crate::bar"));
    }

    #[test]
    fn render_type_mismatch_hint_names_both_types() {
        let hint = ErrorHint::TypeMismatch {
            expected: "String".to_string(),
            found: "&str".to_string(),
        };
        let s = render_hint(&hint);
        assert!(s.contains("`String`"));
        assert!(s.contains("`&str`"));
    }

    #[test]
    fn render_missing_method_hint_with_suggested_traits() {
        let hint = ErrorHint::MissingMethod {
            type_name: "MyType".to_string(),
            method_name: "clone".to_string(),
            suggested_traits: vec!["Clone".to_string()],
        };
        let s = render_hint(&hint);
        assert!(s.contains("`MyType`"));
        assert!(s.contains("`clone`"));
        assert!(s.contains("Clone"));
    }

    #[test]
    fn render_missing_method_hint_without_traits_still_renders() {
        let hint = ErrorHint::MissingMethod {
            type_name: "MyType".to_string(),
            method_name: "frobnicate".to_string(),
            suggested_traits: vec![],
        };
        let s = render_hint(&hint);
        assert!(s.contains("`MyType`"));
        assert!(s.contains("`frobnicate`"));
    }

    #[test]
    fn classify_borrow_conflict_pulls_two_names() {
        // rustc phrasing: "cannot borrow `foo`" (the new borrow that fails)
        // because the value "is also borrowed as `bar`" (the borrow that
        // is still alive and blocks us). So the original_ref is the
        // still-alive one (`bar`) and the conflicting_ref is the new one
        // (`foo`).
        let err = "error[E0502]: cannot borrow `foo` as immutable because it is also borrowed as `bar`";
        let h = classify_borrow_conflict(err).unwrap();
        assert_eq!(
            h,
            ErrorHint::BorrowConflict {
                original_ref: "bar".to_string(),
                conflicting_ref: "foo".to_string(),
            }
        );
    }

    #[test]
    fn classify_missing_import_value() {
        let err = "error[E0425]: cannot find value `frobnicate` in this scope";
        let h = classify_missing_import(err).unwrap();
        assert_eq!(
            h,
            ErrorHint::MissingImport {
                symbol: "frobnicate".to_string(),
                suggested_module: "crate".to_string(),
            }
        );
    }

    #[test]
    fn classify_missing_import_type() {
        let err = "error[E0412]: cannot find type `Widget` in this scope";
        let h = classify_missing_import(err).unwrap();
        assert_eq!(h, ErrorHint::MissingImport { symbol: "Widget".to_string(), suggested_module: "crate".to_string() });
    }

    #[test]
    fn classify_type_mismatch_picks_types() {
        let err = "error[E0308]: mismatched types\n   expected `String`, found `&str`";
        let h = classify_type_mismatch(err).unwrap();
        assert_eq!(
            h,
            ErrorHint::TypeMismatch {
                expected: "String".to_string(),
                found: "&str".to_string(),
            }
        );
    }

    #[test]
    fn classify_missing_method_picks_type_and_method() {
        let err = "error[E0599]: no method named `frobnicate` found for type `MyType` in current scope";
        let h = classify_missing_method(err).unwrap();
        assert_eq!(
            h,
            ErrorHint::MissingMethod {
                type_name: "MyType".to_string(),
                method_name: "frobnicate".to_string(),
                suggested_traits: vec![],
            }
        );
    }

    #[test]
    fn classify_returns_none_for_unrelated_text() {
        assert!(classify_borrow_conflict("permission denied").is_none());
        assert!(classify_missing_import("connection refused").is_none());
        assert!(classify_type_mismatch("all good").is_none());
        assert!(classify_missing_method("file not found").is_none());
        assert!(classify_error("just a generic error").is_none());
    }

    #[test]
    fn classify_error_prefers_borrow_conflict_over_type_mismatch() {
        // A borrow-conflict error may also mention "expected" and "found"
        // in the rendered diagnostic. The classifier must pick the
        // borrow-conflict variant, not the type-mismatch variant.
        let err = "error[E0502]: cannot borrow `x` as mutable because it is also borrowed as `y`\n\
                   expected `String`, found `&str`";
        let h = classify_error(err).unwrap();
        assert!(matches!(h, ErrorHint::BorrowConflict { .. }));
    }

    #[test]
    fn error_hint_is_serializable() {
        let hint = ErrorHint::MissingImport {
            symbol: "Foo".to_string(),
            suggested_module: "crate::bar".to_string(),
        };
        let json = serde_json::to_string(&hint).unwrap();
        let back: ErrorHint = serde_json::from_str(&json).unwrap();
        assert_eq!(hint, back);
    }

    // Real rustc output: spawn `cargo build` on a broken file in a tempdir
    // and confirm `classify_error` produces a MissingImport. This is the
    // high-fidelity end-to-end check; we keep it `#[ignore]` because cargo
    // locks the global package cache and is non-hermetic.
    #[test]
    #[ignore = "spawns cargo; run with --ignored"]
    fn classify_real_rustc_missing_import_output() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let project = tmp.path();
        std::fs::create_dir_all(project.join("src")).unwrap();
        std::fs::write(
            project.join("Cargo.toml"),
            "[package]\nname = \"kirkforge-hint-test\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .unwrap();
        std::fs::write(
            project.join("src/main.rs"),
            "fn main() {\n    let y = frobnicate;\n}\n",
        )
        .unwrap();

        let output = std::process::Command::new("cargo")
            .current_dir(project)
            .args(["build", "--message-format=short"])
            .output()
            .expect("cargo build");
        let stderr = String::from_utf8_lossy(&output.stderr);
        let h = classify_error(&stderr);
        assert!(
            matches!(h, Some(ErrorHint::MissingImport { ref symbol, .. }) if symbol == "frobnicate"),
            "expected MissingImport for `frobnicate`, got {h:?}\nstderr: {stderr}",
        );
    }

    // Real rustc output: borrow conflict in a tempdir project.
    #[test]
    #[ignore = "spawns cargo; run with --ignored"]
    fn classify_real_rustc_borrow_conflict_output() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let project = tmp.path();
        std::fs::create_dir_all(project.join("src")).unwrap();
        std::fs::write(
            project.join("Cargo.toml"),
            "[package]\nname = \"kirkforge-borrow-hint-test\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .unwrap();
        std::fs::write(
            project.join("src/main.rs"),
            r#"fn main() {
    let mut v = vec![1, 2, 3];
    let first = &v[0];
    v.push(4);
    println!("{first}");
}
"#,
        )
        .unwrap();

        let output = std::process::Command::new("cargo")
            .current_dir(project)
            .args(["build", "--message-format=short"])
            .output()
            .expect("cargo build");
        let stderr = String::from_utf8_lossy(&output.stderr);
        let h = classify_error(&stderr);
        assert!(
            matches!(h, Some(ErrorHint::BorrowConflict { .. })),
            "expected BorrowConflict, got {h:?}\nstderr: {stderr}",
        );
    }
}
