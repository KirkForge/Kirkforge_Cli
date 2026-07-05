use crate::session::event_bus::{BusEvent, EditEvent, FileWriteEvent};
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
use std::path::{Path, PathBuf};

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

/// Walk up from `path` looking for a `Cargo.toml`.
fn find_cargo_root(path: &Path) -> Option<PathBuf> {
    let mut dir = path.parent()?;
    loop {
        if dir.join("Cargo.toml").exists() {
            return Some(dir.to_path_buf());
        }
        match dir.parent() {
            Some(parent) => dir = parent,
            None => return None,
        }
    }
}

/// Parse a single cargo JSON line and, if it is a warning/error for the
/// target file, return a `FixSuggestion`.
fn parse_clippy_json(
    line: &str,
    target_path: &Path,
    cargo_root: &Path,
) -> Option<FixSuggestion> {
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
        let line_start = span
            .get("line_start")
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as usize;
        let resolved = cargo_root.join(file_name);
        if resolved == target_path {
            return Some(FixSuggestion {
                description: format!("{text} at {file_name}:{line_start}"),
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
    Verdict::Unfixable(VerificationError {
        description: "clippy check failed".into(),
        file: Some(path),
        details: stderr.lines().take(5).collect::<Vec<_>>().join("\n"),
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
        std::fs::write(
            dir.join("src/main.rs"),
            "fn main() {\n    let x = 1;\n}\n",
        )
        .unwrap();

        let path = dir.join("src/main.rs");
        let event = BusEvent::FileWrite(FileWriteEvent {
            path: path.clone(),
            content_length: 25,
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
}
