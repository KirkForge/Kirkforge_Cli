//! Go vet verifier — runs `go vet ./...` on the edited Go file.
//!
//! Mirrors [`super::python_lint::verify_python_lint`]. Fires on Edit/FileWrite
//! of `.go` files inside a Go project. `go vet` reports suspicious constructs
//! (printf format mismatches, unreachable code, struct copy in lock, etc).
//! Non-zero exit → `Verdict::Fixable` with the tool output. If `go` isn't on
//! PATH, skips gracefully.

use crate::session::verifier::bus::{
    BusVerifier, Severity, VerdictEntry, VerifierSource, VerifyContext,
};
use crate::session::verifier::detect::{find_go_root, ProjectLanguage};
use crate::session::verifier::helpers::{
    command_finding, head_body, language_gate, modified_path, tool_on_path, tool_on_path_sync, Gate,
};
use crate::session::verifier::types::BusEvent;
use crate::session::verifier::{FixSuggestion, Verdict};

/// Probe `go version` to confirm the Go toolchain is on PATH.
async fn pick_go() -> bool {
    tool_on_path("go", &["version"]).await
}

/// Run the Go vet verifier against an event.
pub async fn verify_go_vet(event: &BusEvent) -> Verdict {
    let Some(path) = modified_path(event) else {
        return Verdict::Skipped("not a file modification event".into());
    };

    let root = match language_gate(&path, &["go"], "Go", find_go_root, ProjectLanguage::Go) {
        Gate::Root(root) => root,
        Gate::Skip(verdict) => return verdict,
    };

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

    let body = head_body(
        &String::from_utf8_lossy(&output.stdout),
        &String::from_utf8_lossy(&output.stderr),
        20,
    );

    command_finding(
        format!("go vet findings near {}\n{body}", path.display()),
        path,
        "warning",
    )
}

// ── BusVerifier impl (WO 47.14) ─────────────────────────────────────────

/// Go vet verifier registered on the `VerifierBus`. WO 47.14.
pub struct GoVetVerifier;

impl BusVerifier for GoVetVerifier {
    fn name(&self) -> &str {
        "go_vet"
    }

    fn verify(&self, ctx: &VerifyContext) -> Vec<VerdictEntry> {
        let Some(path) = ctx.changed_files.first() else {
            return vec![];
        };
        let root = match language_gate(path, &["go"], "Go", find_go_root, ProjectLanguage::Go) {
            Gate::Root(root) => root,
            Gate::Skip(_) => return vec![],
        };
        if !tool_on_path_sync("go", &["version"]) {
            return vec![];
        }
        let output = match std::process::Command::new("go")
            .current_dir(&root)
            .args(["vet", "./..."])
            .output()
        {
            Ok(o) => o,
            Err(_) => return vec![],
        };
        if output.status.success() {
            return vec![];
        }
        let body = head_body(
            &String::from_utf8_lossy(&output.stdout),
            &String::from_utf8_lossy(&output.stderr),
            20,
        );
        let fix = FixSuggestion {
            description: format!("go vet findings near {}\n{body}", path.display()),
            file: path.clone(),
            original: String::new(),
            replacement: String::new(),
            severity: "warning".to_string(),
            command: None,
            line: None,
        };
        vec![VerdictEntry {
            source: VerifierSource::Custom("go_vet".into()),
            severity: Severity::Warning,
            message: fix.description.clone(),
            file: Some(path.clone()),
            line: None,
            fix: Some(fix),
        }]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::verifier::{EditEvent, FileWriteEvent};

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
