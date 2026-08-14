//! Python test verifier — runs `python3 -m pytest` on the edited Python file.
//!
//! Mirrors [`super::test::verify_test`]: subscribes to `Edit` and `FileWrite`
//! events. When a `.py` file inside a Python project (detected via
//! [`super::detect::find_python_root`]) is modified, runs
//! `python3 -m pytest {workspace} -x --tb=short -q` in the project root and
//! parses stdout/stderr. Failures are returned as `Verdict::Fixable` with the
//! failure text; success returns `Verdict::Clean`. If pytest isn't installed,
//! the verifier skips gracefully (per WO 31 failure criteria: never block when
//! a tool is absent).
//!
//! The interpreter is resolved by [`pick_python`], which prefers `python3`
//! (the canonical name on most Linux distros — many ship NO `python` symlink)
//! and falls back to `python`.

use crate::session::verifier::detect::{
    detect_project_languages, find_python_root, ProjectLanguage,
};
use crate::session::verifier::types::{BusEvent, EditEvent, FileWriteEvent};
use crate::session::verifier::{FixSuggestion, Verdict, VerificationError};

/// Resolve the first available Python interpreter by probing `--version`.
/// Prefers `python3` (the canonical name on most Linux distros — many ship
/// NO `python` symlink, so `Command::new("python")` fails with `NotFound`
/// and the verifier can never run). Falls back to `python` (macOS Homebrew,
/// some CI images). Returns the binary name or `None`.
///
/// Mirrors [`super::python_lint::pick_linter`]'s probe-then-fallback shape.
async fn pick_python() -> Option<&'static str> {
    for bin in ["python3", "python"] {
        let ok = tokio::process::Command::new(bin)
            .arg("--version")
            .output()
            .await
            .map(|o| o.status.success())
            .unwrap_or(false);
        if ok {
            return Some(bin);
        }
    }
    None
}

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

    // python3-only hosts (most Linux distros) have no `python` symlink; without
    // resolution the spawn fails with NotFound and the verifier can never run.
    let Some(python) = pick_python().await else {
        return Verdict::Skipped("no python interpreter found (tried python3, python)".into());
    };

    let output = tokio::process::Command::new(python)
        .current_dir(&root)
        .args(["-m", "pytest", "-x", "--tb=short", "-q"])
        .output()
        .await;

    let output = match output {
        Ok(o) => o,
        Err(e) => {
            return Verdict::Unfixable(VerificationError {
                description: format!("failed to spawn {python}"),
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

    /// `pick_python` resolves a real interpreter on any host with python3 or
    /// python (the CI Ubuntu runners + dev machines all have at least one).
    #[tokio::test]
    async fn pick_python_finds_an_interpreter() {
        // If neither is present the host can't run Python tests at all; skip
        // the assertion rather than fail spuriously on a minimal container.
        match pick_python().await {
            Some(bin) => assert!(bin == "python3" || bin == "python"),
            None => eprintln!("no python3/python on PATH — skipping interpreter probe"),
        }
    }

    /// Regression: on a python3-only host (no `python` symlink), the verifier
    /// must NOT fail with `Unfixable("failed to spawn python")`. It resolves
    /// `python3` and either runs pytest or skips cleanly.
    #[tokio::test]
    async fn resolves_interpreter_when_python_symlink_absent() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("pyproject.toml"),
            "[project]\nname = \"x\"\n",
        )
        .unwrap();
        let py = dir.path().join("mod.py");
        std::fs::write(&py, "x = 1\n").unwrap();
        let event = BusEvent::Edit(EditEvent {
            path: py,
            diff: "".into(),
        });
        // Clean (pytest installed, tests pass) / Skipped (pytest or
        // interpreter absent) / Fixable (a real test failure) are all
        // acceptable. The bug being guarded: Unfixable spawn failure.
        if let Verdict::Unfixable(e) = verify_python_test(&event).await {
            assert!(
                !e.details.contains("spawn") && !e.description.contains("spawn"),
                "must not fail to spawn when an interpreter is available: {e:?}"
            );
        }
    }
}
