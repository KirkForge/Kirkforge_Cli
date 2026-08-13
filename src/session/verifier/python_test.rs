//! Python test verifier — runs `python -m pytest` on the edited Python file.
//!
//! Mirrors [`super::test::verify_test`]: subscribes to `Edit` and `FileWrite`
//! events. When a `.py` file inside a Python project (detected via
//! [`super::detect::find_python_root`]) is modified, runs
//! `python -m pytest {workspace} -x --tb=short -q` in the project root and
//! parses stdout/stderr. Failures are returned as `Verdict::Fixable` with the
//! failure text; success returns `Verdict::Clean`. If pytest isn't installed,
//! the verifier skips gracefully (per WO 31 failure criteria: never block when
//! a tool is absent).

use crate::session::verifier::detect::{
    detect_project_languages, find_python_root, ProjectLanguage,
};
use crate::session::verifier::types::{BusEvent, EditEvent, FileWriteEvent};
use crate::session::verifier::{FixSuggestion, Verdict, VerificationError};

/// Run the Python test verifier against an event.
pub async fn verify_python_test(event: &BusEvent) -> Verdict {
    let path = match event {
        BusEvent::Edit(EditEvent { path, .. }) => path.clone(),
        BusEvent::FileWrite(FileWriteEvent { path, .. }) => path.clone(),
        _ => return Verdict::Skipped("not a file modification event".into()),
    };

    if path.extension().and_then(|e| e.to_str()) != Some("py") {
        return Verdict::Skipped(format!("unsupported file type: {}", path.display()));
    }

    let Some(root) = find_python_root(&path) else {
        return Verdict::Skipped(format!("no Python marker for {}", path.display()));
    };

    if !detect_project_languages(&root).contains(&ProjectLanguage::Python) {
        return Verdict::Skipped("Python not detected".into());
    }

    let output = tokio::process::Command::new("python")
        .current_dir(&root)
        .args(["-m", "pytest", "-x", "--tb=short", "-q"])
        .output()
        .await;

    let output = match output {
        Ok(o) => o,
        Err(e) => {
            return Verdict::Unfixable(VerificationError {
                description: "failed to spawn python".into(),
                file: Some(path),
                details: e.to_string(),
                line: None,
            })
        }
    };

    // pytest exits non-zero when invoked but the module is absent. Distinguish
    // "pytest not installed" (skip) from "tests failed" (fixable): the python
    // interpreter emits "No module named pytest" to stderr when missing.
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    if output.status.success() {
        return Verdict::Clean;
    }
    if stderr.contains("No module named pytest") {
        return Verdict::Skipped("pytest not installed".into());
    }

    let mut combined: Vec<&str> = stdout.lines().chain(stderr.lines()).collect();
    // ponytail: tail-N truncation matches the Rust test verifier; full pytest
    // output can be hundreds of lines, the model only needs the failure.
    const TAIL_LINES: usize = 20;
    if combined.len() > TAIL_LINES {
        let start = combined.len() - TAIL_LINES;
        combined = combined.split_off(start);
    }
    let body = combined.join("\n");

    Verdict::Fixable(FixSuggestion {
        description: format!("pytest failure near {}\n{body}", path.display()),
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
        assert!(matches!(
            verify_python_test(&event).await,
            Verdict::Skipped(_)
        ));
    }

    #[tokio::test]
    async fn skips_non_python_extensions() {
        let event = BusEvent::Edit(EditEvent {
            path: std::path::PathBuf::from("foo.rs"),
            diff: "".into(),
        });
        assert!(matches!(
            verify_python_test(&event).await,
            Verdict::Skipped(_)
        ));
    }

    #[tokio::test]
    async fn skips_python_file_with_no_project_root() {
        let dir = tempfile::tempdir().unwrap();
        let py = dir.path().join("lonely.py");
        std::fs::write(&py, "x = 1\n").unwrap();
        let event = BusEvent::FileWrite(FileWriteEvent {
            path: py,
            content_length: 6,
            content_hash: 0,
        });
        match verify_python_test(&event).await {
            Verdict::Skipped(msg) => assert!(msg.contains("Python marker")),
            other => panic!("expected Skipped, got {other:?}"),
        }
    }
}
