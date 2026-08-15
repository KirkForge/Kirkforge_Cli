//! Go vet verifier — runs `go vet ./...` on the edited Go file.
//!
//! Mirrors [`super::python_lint::verify_python_lint`]. Fires on Edit/FileWrite
//! of `.go` files inside a Go project. `go vet` reports suspicious constructs
//! (printf format mismatches, unreachable code, struct copy in lock, etc).
//! Non-zero exit → `Verdict::Fixable` with the tool output. If `go` isn't on
//! PATH, skips gracefully.

use crate::session::verifier::detect::{detect_project_languages, find_go_root, ProjectLanguage};
use crate::session::verifier::types::{BusEvent, EditEvent, FileWriteEvent};
use crate::session::verifier::{FixSuggestion, Verdict};

/// Probe `go version` to confirm the Go toolchain is on PATH.
async fn pick_go() -> bool {
    tokio::process::Command::new("go")
        .arg("version")
        .output()
        .await
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Run the Go vet verifier against an event.
pub async fn verify_go_vet(event: &BusEvent) -> Verdict {
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
        .args(["vet", "./..."])
        .output()
        .await;

    let output = match output {
        Ok(o) => o,
        Err(e) => return Verdict::Skipped(format!("go vet not available: {e}")),
    };

    if output.status.success() {
        return Verdict::Clean;
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let body: String = if !stdout.trim().is_empty() {
        stdout
    } else {
        stderr
    }
    .lines()
    .take(20)
    .collect::<Vec<_>>()
    .join("\n");

    Verdict::Fixable(FixSuggestion {
        description: format!("go vet findings near {}\n{body}", path.display()),
        file: path,
        original: String::new(),
        replacement: String::new(),
        severity: "warning".to_string(),
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
        assert!(matches!(verify_go_vet(&event).await, Verdict::Skipped(_)));
    }

    #[tokio::test]
    async fn skips_non_go_extensions() {
        let event = BusEvent::Edit(EditEvent {
            path: std::path::PathBuf::from("foo.rs"),
            diff: "".into(),
        });
        assert!(matches!(verify_go_vet(&event).await, Verdict::Skipped(_)));
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
        match verify_go_vet(&event).await {
            Verdict::Skipped(msg) => assert!(msg.contains("Go marker")),
            other => panic!("expected Skipped, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn pick_go_does_not_panic() {
        let _ = pick_go().await;
    }
}
