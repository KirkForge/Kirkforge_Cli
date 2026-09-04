//! Go test verifier — runs `go test ./...` on the edited Go file.
//!
//! Mirrors [`super::python_test::verify_python_test`]. Fires on Edit/FileWrite
//! of `.go` files inside a Go project (detected via [`super::detect::find_go_root`]).
//! Non-zero exit → `Verdict::Fixable` with the tail of the output. If `go`
//! isn't on PATH, skips gracefully.

use crate::session::verifier::bus::{
    BusVerifier, Severity, VerdictEntry, VerifierSource, VerifyContext,
};
use crate::session::verifier::detect::{find_go_root, ProjectLanguage};
use crate::session::verifier::helpers::{
    command_finding, language_gate, modified_path, tail_body, tool_on_path, tool_on_path_sync,
    Gate,
};
use crate::session::verifier::types::BusEvent;
use crate::session::verifier::{Verdict, VerificationError, FixSuggestion};

/// Probe `go version` to confirm the Go toolchain is on PATH.
async fn pick_go() -> bool {
    tool_on_path("go", &["version"]).await
}

/// Run the Go test verifier against an event.
pub async fn verify_go_test(event: &BusEvent) -> Verdict {
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

    // ponytail: tail-N truncation matches the Python/Rust test verifiers.
    let body = tail_body(
        &String::from_utf8_lossy(&output.stdout),
        &String::from_utf8_lossy(&output.stderr),
        20,
    );

    command_finding(
        format!("go test failure near {}\n{body}", path.display()),
        path,
        "error",
    )
}

// ── BusVerifier impl (WO 47.14) ─────────────────────────────────────────

/// Go test verifier registered on the `VerifierBus`. WO 47.14.
pub struct GoTestVerifier;

impl BusVerifier for GoTestVerifier {
    fn name(&self) -> &str {
        "go_test"
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
            .args(["test", "./..."])
            .output()
        {
            Ok(o) => o,
            Err(_) => return vec![],
        };
        if output.status.success() {
            return vec![];
        }
        let body = tail_body(
            &String::from_utf8_lossy(&output.stdout),
            &String::from_utf8_lossy(&output.stderr),
            20,
        );
        let fix = FixSuggestion {
            description: format!("go test failure near {}\n{body}", path.display()),
            file: path.clone(),
            original: String::new(),
            replacement: String::new(),
            severity: "error".to_string(),
            command: None,
            line: None,
        };
        vec![VerdictEntry {
            source: VerifierSource::Test,
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
