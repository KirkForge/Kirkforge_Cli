use crate::session::error_recovery;
use crate::session::event_bus::{BusEvent, EditEvent, FileWriteEvent};
use crate::session::verifier::helpers::find_cargo_root;
/// Build verifier — runs `cargo build` on Rust files and reports compiler errors.
///
/// This verifier subscribes to `Edit` and `FileWrite` events. When a Rust file
/// inside a Cargo project is modified, it runs
/// `cargo build --message-format=json` in the project root and parses the JSON
/// output. The first compiler `error` that maps back to the modified file is
/// returned as a model-facing `FixSuggestion` with empty `original`/`replacement`
/// and `command` set to `None`, because the compiler does not provide
/// deterministic text replacements.
///
/// The build verifier is registered at priority 3 (after lint, before
/// rustfmt).
use crate::session::verifier::{FixSuggestion, Verdict, VerificationError};
use std::path::Path;

/// Parse a single cargo JSON line and, if it is a compiler error for the
/// target file, return a `FixSuggestion`.
fn parse_build_json(line: &str, target_path: &Path, cargo_root: &Path) -> Option<FixSuggestion> {
    let value: serde_json::Value = serde_json::from_str(line).ok()?;
    if value.get("reason").and_then(|v| v.as_str()) != Some("compiler-message") {
        return None;
    }
    let message = value.get("message")?;
    let level = message.get("level").and_then(|v| v.as_str())?;
    if level != "error" {
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

/// Run the build verifier against an event.
pub async fn verify_build(event: &BusEvent) -> Verdict {
    let path = match event {
        BusEvent::Edit(EditEvent { path, .. }) => path.clone(),
        BusEvent::FileWrite(FileWriteEvent { path, .. }) => path.clone(),
        _ => return Verdict::Skipped("not a file modification event".into()),
    };

    if path.extension().and_then(|e| e.to_str()) != Some("rs") {
        return Verdict::Skipped(format!("unsupported file type: {}", path.display()));
    }

    let Some(cargo_root) = find_cargo_root(&path) else {
        return Verdict::Skipped(format!("no Cargo.toml found for {}", path.display()));
    };

    let output = tokio::process::Command::new("cargo")
        .current_dir(&cargo_root)
        .args(["build", "--message-format=json"])
        .output()
        .await;

    let output = match output {
        Ok(o) => o,
        Err(e) => return Verdict::Skipped(format!("cargo not available: {e}")),
    };

    let stdout = String::from_utf8_lossy(&output.stdout);
    for line in stdout.lines() {
        if let Some(suggestion) = parse_build_json(line, &path, &cargo_root) {
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
        description: "cargo build failed".into(),
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
        let v = verify_build(&event).await;
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
        let v = verify_build(&event).await;
        assert!(matches!(v, Verdict::Skipped(_)));
    }

    #[tokio::test]
    async fn test_parse_build_json_extracts_error() {
        let line = r#"{"reason":"compiler-message","package_id":"foo 0.1.0","target":{"kind":["bin"],"name":"foo","src_path":"/tmp/foo/src/main.rs"},"message":{"rendered":"error: cannot find value `x` in this scope\n  --> src/main.rs:3:9\n   |\n3 |     let y = x;\n   |         ^\n   |\n   = note: ...\n\n","level":"error","message":"cannot find value `x` in this scope","spans":[{"file_name":"src/main.rs","line_start":3,"line_end":3,"column_start":9,"column_end":10}]}}"#;
        let cargo_root = std::path::PathBuf::from("/tmp/foo");
        let target = std::path::PathBuf::from("/tmp/foo/src/main.rs");
        let suggestion = parse_build_json(line, &target, &cargo_root).unwrap();
        assert_eq!(suggestion.file, target);
        assert!(suggestion.description.contains("cannot find value `x`"));
        assert!(suggestion.description.contains("src/main.rs:3"));
        assert_eq!(suggestion.severity, "error");
        assert!(suggestion.original.is_empty());
        assert!(suggestion.replacement.is_empty());
        assert!(suggestion.command.is_none());
    }

    #[tokio::test]
    async fn test_parse_build_json_attaches_hint_for_missing_import() {
        let line = r#"{"reason":"compiler-message","package_id":"foo 0.1.0","target":{"kind":["bin"],"name":"foo","src_path":"/tmp/foo/src/main.rs"},"message":{"rendered":"error: cannot find value `frobnicate` in this scope\n  --> src/main.rs:1:1\n\n","level":"error","message":"cannot find value `frobnicate` in this scope","spans":[{"file_name":"src/main.rs","line_start":1,"line_end":1,"column_start":1,"column_end":10}]}}"#;
        let cargo_root = std::path::PathBuf::from("/tmp/foo");
        let target = std::path::PathBuf::from("/tmp/foo/src/main.rs");
        let suggestion = parse_build_json(line, &target, &cargo_root).unwrap();
        assert!(
            suggestion.description.contains("Hint:"),
            "expected Hint line in description, got: {}",
            suggestion.description,
        );
        assert!(suggestion.description.contains("`frobnicate`"));
    }

    #[tokio::test]
    async fn test_parse_build_json_no_hint_for_unclassified_message() {
        // Generic text that doesn't match any classifier — the description
        // should be the raw error and nothing else.
        let line = r#"{"reason":"compiler-message","package_id":"foo 0.1.0","target":{"kind":["bin"],"name":"foo","src_path":"/tmp/foo/src/main.rs"},"message":{"rendered":"error: something custom\n","level":"error","message":"something custom happened","spans":[{"file_name":"src/main.rs","line_start":1,"line_end":1,"column_start":1,"column_end":2}]}}"#;
        let cargo_root = std::path::PathBuf::from("/tmp/foo");
        let target = std::path::PathBuf::from("/tmp/foo/src/main.rs");
        let suggestion = parse_build_json(line, &target, &cargo_root).unwrap();
        assert!(!suggestion.description.contains("Hint:"));
    }

    // This test spawns `cargo build` in a temporary project. It cannot run
    // concurrently with another `cargo` invocation because the Cargo package
    // cache lock serializes all cargo processes, so it is ignored by default.
    // Run it separately when needed: `cargo test --workspace -- --ignored`.
    #[tokio::test]
    #[ignore = "spawns cargo; run separately with cargo test --workspace -- --ignored"]
    async fn test_build_error_on_temp_project() {
        let dir = std::env::temp_dir().join("kirkforge_build_test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(
            dir.join("Cargo.toml"),
            r#"[package]
name = "kirkforge-build-test"
version = "0.1.0"
edition = "2021"
"#,
        )
        .unwrap();
        std::fs::write(dir.join("src/main.rs"), "fn main() {\n    let y = x;\n}\n").unwrap();

        let path = dir.join("src/main.rs");
        let event = BusEvent::FileWrite(FileWriteEvent {
            path: path.clone(),
            content_length: 26,
            content_hash: 0,
        });
        let v = verify_build(&event).await;
        match v {
            Verdict::Fixable(suggestion) => {
                assert_eq!(suggestion.file, path);
                assert!(suggestion.description.contains("cannot find value `x`"));
                assert_eq!(suggestion.severity, "error");
                assert!(suggestion.original.is_empty());
                assert!(suggestion.replacement.is_empty());
                assert!(suggestion.command.is_none());
            }
            other => panic!("expected Fixable build error, got {other:?}"),
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn parse_build_json_returns_none_for_non_compiler_message() {
        let line = r#"{"reason":"compiler-artifact","package_id":"foo"}"#;
        let result = parse_build_json(
            line,
            std::path::Path::new("x.rs"),
            std::path::Path::new("."),
        );
        assert!(result.is_none());
    }

    #[test]
    fn parse_build_json_returns_none_for_non_error_level() {
        let line = r#"{"reason":"compiler-message","message":{"level":"warning","message":"w","spans":[{"file_name":"src/main.rs","line_start":1}]}}"#;
        let result = parse_build_json(
            line,
            std::path::Path::new("src/main.rs"),
            std::path::Path::new("."),
        );
        assert!(result.is_none(), "build verifier only fires on errors");
    }

    #[test]
    fn parse_build_json_returns_none_when_spans_do_not_match_target() {
        let line = r#"{"reason":"compiler-message","message":{"level":"error","message":"e","spans":[{"file_name":"src/other.rs","line_start":1}]}}"#;
        let cargo_root = std::path::PathBuf::from("/tmp/foo");
        let target = std::path::PathBuf::from("/tmp/foo/src/main.rs");
        let result = parse_build_json(line, &target, &cargo_root);
        assert!(result.is_none(), "mismatching span should yield None");
    }

    #[test]
    fn parse_build_json_returns_none_for_invalid_json() {
        let result = parse_build_json(
            "not json",
            std::path::Path::new("x.rs"),
            std::path::Path::new("."),
        );
        assert!(result.is_none());
    }
}
