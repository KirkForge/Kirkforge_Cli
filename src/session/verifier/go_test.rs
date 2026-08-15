//! Go test verifier — runs `go test ./...` on the edited Go file.
//!
//! Mirrors [`super::python_test::verify_python_test`]. Fires on Edit/FileWrite
//! of `.go` files inside a Go project (detected via [`super::detect::find_go_root`]).
//! Non-zero exit → `Verdict::Fixable` with the tail of the output. If `go`
//! isn't on PATH, skips gracefully.

use crate::session::verifier::detect::{detect_project_languages, find_go_root, ProjectLanguage};
use crate::session::verifier::types::{BusEvent, EditEvent, FileWriteEvent};
use crate::session::verifier::{FixSuggestion, Verdict, VerificationError};

/// Probe `go version` to confirm the Go toolchain is on PATH.
async fn pick_go() -> bool {
    tokio::process::Command::new("go")
        .arg("version")
        .output()
        .await
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Run the Go test verifier against an event.
pub async fn verify_go_test(event: &BusEvent) -> Verdict {
    let path = match event {
        BusEvent::Edit(EditEvent { path, .. }) => path.clone(),
        BusEvent::FileWrite(FileWriteEvent { path, .. }) => path.clone(),
        _ => return Verdict::Skipped("not a file modification event".into()),
    };

    if path.extension().and_then(|e| e.to_str()) != Some("go") {
        return Verdict::Skipped(format!("unsupported file type: {}", path.display()));
    }

    let Some(root) = find_go_root(&path) else {
        return Verdict::Skipped(format!("no Go marker for {}", path.display()));
    };

    if !detect_project_languages(&root).contains(&ProjectLanguage::Go) {
        return Verdict::Skipped("Go not detected".into());
    }

    if !pick_go().await {
        return Verdict::Skipped("go toolchain not found on PATH".into());
    }

    let output = tokio::process::Command::new("go")
        .current_dir(&root)
        .args(["test", "./..."])
        .output()
        .await;

    let output = match output {
        Ok(o) => o,
        Err(e) => {
            return Verdict::Unfixable(VerificationError {
                description: "failed to spawn go test".into(),
                file: Some(path),
                details: e.to_string(),
                line: None,
            })
        }
    };

    if output.status.success() {
        return Verdict::Clean;
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    // ponytail: tail-N truncation matches the Python/Rust test verifiers.
    const TAIL_LINES: usize = 20;
    let mut combined: Vec<&str> = stdout.lines().chain(stderr.lines()).collect();
    if combined.len() > TAIL_LINES {
        let start = combined.len() - TAIL_LINES;
        combined = combined.split_off(start);
    }
    let body = combined.join("\n");

    Verdict::Fixable(FixSuggestion {
        description: format!("go test failure near {}\n{body}", path.display()),
        file: path,
        original: String::new(),
        replacement: String::new(),
        severity: "error".to_string(),
        command: None,
        line: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn skips_non_edit_events() {
        let event = BusEvent::BashExec(crate::session::verifier::types::BashExecEvent {
            command: "echo hi".into(),
            exit_code: 0,
            stdout_len: 3,
            stderr_len: 0,
            workdir: None,
        });
        assert!(matches!(verify_go_test(&event).await, Verdict::Skipped(_)));
    }

    #[tokio::test]
    async fn skips_non_go_extensions() {
        let event = BusEvent::Edit(EditEvent {
            path: std::path::PathBuf::from("foo.rs"),
            diff: "".into(),
        });
        assert!(matches!(verify_go_test(&event).await, Verdict::Skipped(_)));
    }

    #[tokio::test]
    async fn skips_go_file_with_no_project_root() {
        let dir = tempfile::tempdir().unwrap();
        let go = dir.path().join("orphan.go");
        std::fs::write(&go, "package main\n").unwrap();
        let event = BusEvent::FileWrite(FileWriteEvent {
            path: go,
            content_length: 12,
            content_hash: 0,
        });
        match verify_go_test(&event).await {
            Verdict::Skipped(msg) => assert!(msg.contains("Go marker")),
            other => panic!("expected Skipped, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn pick_go_does_not_panic() {
        // If go isn't installed (minimal container), this returns false — the
        // verifier itself skips gracefully in that case.
        let _ = pick_go().await;
    }
}
