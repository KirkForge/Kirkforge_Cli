/// Lint verifier — runs cargo clippy or rustfmt on edited files.
///
/// This verifier subscribes to `Edit` and `FileWrite` events.
/// When a Rust file is modified, it runs `cargo clippy` (or `rustfmt --check`)
/// and reports any issues found.
///
/// The lint verifier is registered at priority 2 (after security).
use crate::session::verifier::{FixSuggestion, Verdict, VerificationError};
use crate::session::event_bus::{BusEvent, EditEvent, FileWriteEvent};
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
        return Verdict::Skipped(format!("lint verifier not yet implemented for {:?}", target));
    }

    // Run clippy on the project
    let output = tokio::process::Command::new("cargo")
        .args(["clippy", "--", "-D", "warnings"])
        .output()
        .await;

    let output = match output {
        Ok(o) => o,
        Err(e) => return Verdict::Skipped(format!("cargo not available: {e}")),
    };

    if output.status.success() {
        return Verdict::Clean;
    }

    let stderr = String::from_utf8_lossy(&output.stderr);
    let first_warning = extract_first_clippy_warning(&stderr);

    match first_warning {
        Some((msg, fname, _ln)) => {
            Verdict::Fixable(FixSuggestion {
                description: msg.clone(),
                file: PathBuf::from(&fname),
                original: String::new(), // We don't know the exact original text
                replacement: String::new(),
                severity: if msg.contains("error") { "error".into() } else { "warning".into() },
            })
        }
        None => {
            Verdict::Unfixable(VerificationError {
                description: "clippy check failed".into(),
                file: Some(path),
                details: stderr.lines().take(5).collect::<Vec<_>>().join("\n"),
            })
        }
    }
}

/// Extract the first clippy warning message and location.
fn extract_first_clippy_warning(stderr: &str) -> Option<(String, String, usize)> {
    for line in stderr.lines() {
        // Pattern: "error[E0308]: mismatched types" or "warning: unused variable"
        if line.contains("error[") || line.starts_with("error") || line.starts_with("warning") {
            // Try to extract file:line from the next line
            let msg = line.to_string();
            return Some((msg, String::new(), 0));
        }
    }
    None
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
        });
        let v = verify_lint(&event).await;
        assert!(matches!(v, Verdict::Skipped(_)));
    }
}