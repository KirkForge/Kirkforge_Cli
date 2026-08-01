use crate::session::error_recovery;
use crate::session::event_bus::{BusEvent, EditEvent, FileWriteEvent};
use crate::session::verifier::helpers::find_cargo_root;
/// Lint verifier — runs `cargo clippy` on Rust files and reports findings.
///
/// This verifier subscribes to `Edit` and `FileWrite` events. When a Rust
/// file inside a Cargo project is modified, it runs
/// `cargo clippy --message-format=json` in the project root and parses the
/// JSON output. The first clippy `warning` or `error` that maps back to the
/// modified file is returned as a model-facing `FixSuggestion` with empty
/// `original`/`replacement` and `command` set to `None`, because clippy does
/// not provide deterministic text replacements.
///
/// The lint verifier is registered at priority 2 (after security).
use crate::session::verifier::{FixSuggestion, Verdict, VerificationError};
use std::path::Path;

/// Lint targets supported by the verifier.
#[derive(Debug)]
enum LintTarget {
    Rust,
    Python,
    JavaScript,
    Unknown,
}

impl LintTarget {
    fn from_path(path: &Path) -> Self {
        match path.extension().and_then(|e| e.to_str()) {
            Some("rs") => LintTarget::Rust,
            Some("py") => LintTarget::Python,
            Some("js" | "ts" | "jsx" | "tsx") => LintTarget::JavaScript,
            _ => LintTarget::Unknown,
        }
    }
}

/// Parse a single cargo JSON line and, if it is a warning/error for the
/// target file, return a `FixSuggestion`.
fn parse_clippy_json(line: &str, target_path: &Path, cargo_root: &Path) -> Option<FixSuggestion> {
    let value: serde_json::Value = serde_json::from_str(line).ok()?;
    if value.get("reason").and_then(|v| v.as_str()) != Some("compiler-message") {
        return None;
    }
    let message = value.get("message")?;
    let level = message.get("level").and_then(|v| v.as_str())?;
    if !matches!(level, "warning" | "error") {
        return None;
    }
    let text = message.get("message").and_then(|v| v.as_str())?.to_string();
    let spans = message.get("spans")?.as_array()?;
    for span in spans {
        let file_name = span.get("file_name").and_then(|v| v.as_str())?;
        let line_start = span.get("line_start").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
        let resolved = cargo_root.join(file_name);
        if resolved == target_path {
            let mut description = format!("{text} at {file_name}:{line_start}");
            if let Some(hint) = error_recovery::classify_error(&text) {
                description.push('\n');
                description.push_str(&error_recovery::render_hint(&hint));
            }
            return Some(FixSuggestion {
                description,
                file: target_path.to_path_buf(),
                original: String::new(),
                replacement: String::new(),
                severity: level.to_string(),
                command: None,
            });
        }
    }
    None
}

/// Run the lint verifier against an event.
pub async fn verify_lint(event: &BusEvent) -> Verdict {
    let path = match event {
        BusEvent::Edit(EditEvent { path, .. }) => path.clone(),
        BusEvent::FileWrite(FileWriteEvent { path, .. }) => path.clone(),
        _ => return Verdict::Skipped("not a file modification event".into()),
    };

    let target = LintTarget::from_path(&path);
    if matches!(target, LintTarget::Unknown) {
        return Verdict::Skipped(format!("unsupported file type: {}", path.display()));
    }

    // For now only Rust is fully supported
    if !matches!(target, LintTarget::Rust) {
        return Verdict::Skipped(format!("lint verifier not yet implemented for {target:?}"));
    }

    let Some(cargo_root) = find_cargo_root(&path) else {
        return Verdict::Skipped(format!("no Cargo.toml found for {}", path.display()));
    };

    let output = tokio::process::Command::new("cargo")
        .current_dir(&cargo_root)
        .args(["clippy", "--message-format=json"])
        .output()
        .await;

    let output = match output {
        Ok(o) => o,
        Err(e) => return Verdict::Skipped(format!("cargo not available: {e}")),
    };

    let stdout = String::from_utf8_lossy(&output.stdout);
    for line in stdout.lines() {
        if let Some(suggestion) = parse_clippy_json(line, &path, &cargo_root) {
            return Verdict::Fixable(suggestion);
        }
    }

    if output.status.success() {
        return Verdict::Clean;
    }

    // Could not extract a concrete finding — report the first few stderr lines.
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stderr_summary = stderr.lines().take(5).collect::<Vec<_>>().join("\n");
    let mut details = stderr_summary;
    if let Some(hint) = error_recovery::classify_error(&stderr) {
        details.push('\n');
        details.push_str(&error_recovery::render_hint(&hint));
    }
    Verdict::Unfixable(VerificationError {
        description: "clippy check failed".into(),
        file: Some(path),
        details,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_skips_unknown_file_types() {
        let event = BusEvent::Edit(EditEvent {
            path: std::path::PathBuf::from("readme.md"),
            diff: "".into(),
        });
        let v = verify_lint(&event).await;
        assert!(matches!(v, Verdict::Skipped(_)));
    }

    #[tokio::test]
    async fn test_skips_non_edit_events() {
        let event = BusEvent::BashExec(crate::session::event_bus::BashExecEvent {
            command: "echo hi".into(),
            exit_code: 0,
            stdout_len: 3,
            stderr_len: 0,
            workdir: None,
        });
        let v = verify_lint(&event).await;
        assert!(matches!(v, Verdict::Skipped(_)));
    }

    #[tokio::test]
    async fn test_parse_clippy_json_extracts_warning() {
        let line = r#"{"reason":"compiler-message","package_id":"foo 0.1.0","target":{"kind":["bin"],"name":"foo","src_path":"/tmp/foo/src/main.rs"},"message":{"rendered":"warning: unused variable: `x`\n  --> src/main.rs:3:9\n   |\n3 |     let x = 1;\n   |         ^\n   |\n   = note: `#[warn(unused_variables)]` on by default\n\n","level":"warning","message":"unused variable: `x`","spans":[{"file_name":"src/main.rs","line_start":3,"line_end":3,"column_start":9,"column_end":10}]}}"#;
        let cargo_root = std::path::PathBuf::from("/tmp/foo");
        let target = std::path::PathBuf::from("/tmp/foo/src/main.rs");
        let suggestion = parse_clippy_json(line, &target, &cargo_root).unwrap();
        assert_eq!(suggestion.file, target);
        assert!(suggestion.description.contains("unused variable: `x`"));
        assert!(suggestion.description.contains("src/main.rs:3"));
        assert_eq!(suggestion.severity, "warning");
        assert!(suggestion.original.is_empty());
        assert!(suggestion.replacement.is_empty());
        assert!(suggestion.command.is_none());
    }

    #[tokio::test]
    async fn test_parse_clippy_json_attaches_hint_for_borrow_conflict() {
        let line = r#"{"reason":"compiler-message","package_id":"foo 0.1.0","target":{"kind":["bin"],"name":"foo","src_path":"/tmp/foo/src/main.rs"},"message":{"rendered":"error: cannot borrow `foo` as immutable because it is also borrowed as `bar`\n","level":"error","message":"cannot borrow `foo` as immutable because it is also borrowed as `bar`","spans":[{"file_name":"src/main.rs","line_start":2,"line_end":2,"column_start":5,"column_end":8}]}}"#;
        let cargo_root = std::path::PathBuf::from("/tmp/foo");
        let target = std::path::PathBuf::from("/tmp/foo/src/main.rs");
        let suggestion = parse_clippy_json(line, &target, &cargo_root).unwrap();
        assert!(
            suggestion.description.contains("Hint:"),
            "expected Hint line in description, got: {}",
            suggestion.description,
        );
        assert!(suggestion.description.contains("`foo`"));
        assert!(suggestion.description.contains("`bar`"));
    }

    #[tokio::test]
    async fn test_parse_clippy_json_no_hint_for_unclassified_message() {
        let line = r#"{"reason":"compiler-message","package_id":"foo 0.1.0","target":{"kind":["bin"],"name":"foo","src_path":"/tmp/foo/src/main.rs"},"message":{"rendered":"warning: something custom\n","level":"warning","message":"something custom happened","spans":[{"file_name":"src/main.rs","line_start":1,"line_end":1,"column_start":1,"column_end":2}]}}"#;
        let cargo_root = std::path::PathBuf::from("/tmp/foo");
        let target = std::path::PathBuf::from("/tmp/foo/src/main.rs");
        let suggestion = parse_clippy_json(line, &target, &cargo_root).unwrap();
        assert!(!suggestion.description.contains("Hint:"));
    }

    // This test spawns `cargo clippy` in a temporary project. It cannot run
    // concurrently with another `cargo` invocation because the Cargo package
    // cache lock serializes all cargo processes, so it is ignored by default.
    // Run it separately when needed: `cargo test --workspace -- --ignored`.
    #[tokio::test]
    #[ignore = "spawns cargo clippy; run separately with cargo test --workspace -- --ignored"]
    async fn test_clippy_warning_on_temp_project() {
        let dir = std::env::temp_dir().join("kirkforge_lint_test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(
            dir.join("Cargo.toml"),
            r#"[package]
name = "kirkforge-lint-test"
version = "0.1.0"
edition = "2021"
"#,
        )
        .unwrap();
        std::fs::write(dir.join("src/main.rs"), "fn main() {\n    let x = 1;\n}\n").unwrap();

        let path = dir.join("src/main.rs");
        let event = BusEvent::FileWrite(FileWriteEvent {
            path: path.clone(),
            content_length: 25,
            content_hash: 0,
        });
        let v = verify_lint(&event).await;
        match v {
            Verdict::Fixable(suggestion) => {
                assert_eq!(suggestion.file, path);
                assert!(
                    suggestion.description.contains("unused variable")
                        || suggestion.description.contains("unused_variables")
                );
                assert!(suggestion.severity == "warning" || suggestion.severity == "error");
            }
            other => panic!("expected Fixable clippy warning, got {other:?}"),
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn lint_target_from_path_classifies_rust_extensions() {
        assert!(matches!(
            LintTarget::from_path(std::path::Path::new("x.rs")),
            LintTarget::Rust
        ));
    }

    #[test]
    fn lint_target_from_path_classifies_python_extension() {
        assert!(matches!(
            LintTarget::from_path(std::path::Path::new("x.py")),
            LintTarget::Python
        ));
    }

    #[test]
    fn lint_target_from_path_classifies_javascript_extensions() {
        for ext in ["js", "ts", "jsx", "tsx"] {
            let name = format!("file.{ext}");
            let path = std::path::Path::new(&name);
            assert!(
                matches!(LintTarget::from_path(path), LintTarget::JavaScript),
                "{ext} should classify as JavaScript"
            );
        }
    }

    #[test]
    fn lint_target_from_path_unknown_for_unrecognized_extension() {
        assert!(matches!(
            LintTarget::from_path(std::path::Path::new("README.md")),
            LintTarget::Unknown
        ));
        assert!(matches!(
            LintTarget::from_path(std::path::Path::new("config.toml")),
            LintTarget::Unknown
        ));
        assert!(matches!(
            LintTarget::from_path(std::path::Path::new("no_ext")),
            LintTarget::Unknown
        ));
    }

    #[test]
    fn lint_target_from_path_unknown_for_no_extension() {
        assert!(matches!(
            LintTarget::from_path(std::path::Path::new("just_a_name")),
            LintTarget::Unknown
        ));
    }

    #[test]
    fn parse_clippy_json_returns_none_for_non_compiler_message_reason() {
        let line = r#"{"reason":"compiler-artifact"}"#;
        let result = parse_clippy_json(
            line,
            std::path::Path::new("x.rs"),
            std::path::Path::new("."),
        );
        assert!(result.is_none());
    }

    #[test]
    fn parse_clippy_json_returns_none_for_info_level() {
        let line = r#"{"reason":"compiler-message","message":{"level":"info","message":"m","spans":[{"file_name":"x.rs","line_start":1}]}}"#;
        let result = parse_clippy_json(
            line,
            std::path::Path::new("x.rs"),
            std::path::Path::new("."),
        );
        assert!(result.is_none(), "info level should not yield a suggestion");
    }

    #[test]
    fn parse_clippy_json_returns_none_for_invalid_json() {
        let result = parse_clippy_json(
            "not json",
            std::path::Path::new("x.rs"),
            std::path::Path::new("."),
        );
        assert!(result.is_none());
    }

    #[test]
    fn parse_clippy_json_returns_none_when_span_file_does_not_match_target() {
        let line = r#"{"reason":"compiler-message","message":{"level":"warning","message":"m","spans":[{"file_name":"src/other.rs","line_start":1}]}}"#;
        let cargo_root = std::path::PathBuf::from("/tmp/foo");
        let target = std::path::PathBuf::from("/tmp/foo/src/main.rs");
        let result = parse_clippy_json(line, &target, &cargo_root);
        assert!(result.is_none(), "mismatching span should yield None");
    }

    #[tokio::test]
    async fn verify_lint_skips_python_file_with_unsupported_message() {
        let event = BusEvent::Edit(EditEvent {
            path: std::path::PathBuf::from("script.py"),
            diff: "".into(),
        });
        let v = verify_lint(&event).await;
        match v {
            Verdict::Skipped(reason) => assert!(reason.contains("not yet implemented")),
            other => panic!("expected Skipped, got {other:?}"),
        }
    }
}
