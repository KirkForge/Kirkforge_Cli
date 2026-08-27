//! Python type-check verifier — runs `mypy` on the edited Python file.
//!
//! Mirrors the Rust build/lint verifiers. Fires only when the project is
//! Python AND mypy is configured (presence of `mypy.ini`, or a
//! `[tool.mypy]` section in `pyproject.toml`). If mypy isn't installed, skips
//! gracefully. Type errors are returned as `Verdict::Fixable` with the tool
//! output.

use crate::session::verifier::detect::{find_python_root, ProjectLanguage};
use crate::session::verifier::helpers::{
    command_finding, head_body, language_gate, modified_path, Gate,
};
use crate::session::verifier::types::BusEvent;
use crate::session::verifier::Verdict;

/// True if mypy is configured for `root` (mypy.ini file, or a
/// `[tool.mypy]` section in pyproject.toml).
fn mypy_configured(root: &std::path::Path) -> bool {
    if root.join("mypy.ini").is_file() {
        return true;
    }
    // ponytail: substring scan over pyproject.toml — avoids pulling a TOML
    // parser into the verifier hot path; false-positive risk is negligible
    // because `[tool.mypy]` is a fixed literal that only appears under that
    // section. Upgrade path: parse with `toml` if section detection ever
    // becomes ambiguous.
    if let Ok(text) = std::fs::read_to_string(root.join("pyproject.toml")) {
        return text.contains("[tool.mypy]");
    }
    false
}

/// Run the Python type-check verifier against an event.
pub async fn verify_python_typecheck(event: &BusEvent) -> Verdict {
    let Some(path) = modified_path(event) else {
        return Verdict::Skipped("not a file modification event".into());
    };

    let root = match language_gate(
        &path,
        &["py"],
        "Python",
        find_python_root,
        ProjectLanguage::Python,
    ) {
        Gate::Root(root) => root,
        Gate::Skip(verdict) => return verdict,
    };

    if !mypy_configured(&root) {
        return Verdict::Skipped("mypy not configured (no mypy.ini or [tool.mypy])".into());
    }

    let output = tokio::process::Command::new("mypy")
        .current_dir(&root)
        .arg(&path)
        .output()
        .await;

    let output = match output {
        Ok(o) => o,
        Err(e) => return Verdict::Skipped(format!("mypy not available: {e}")),
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
        format!("mypy errors near {}\n{body}", path.display()),
        path,
        "error",
    )
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
        assert!(matches!(
            verify_python_typecheck(&event).await,
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
            verify_python_typecheck(&event).await,
            Verdict::Skipped(_)
        ));
    }

    #[tokio::test]
    async fn skips_python_file_with_no_project_root() {
        let dir = tempfile::tempdir().unwrap();
        let py = dir.path().join("orphan.py");
        std::fs::write(&py, "x = 1\n").unwrap();
        let event = BusEvent::FileWrite(FileWriteEvent {
            path: py,
            content_length: 6,
            content_hash: 0,
        });
        match verify_python_typecheck(&event).await {
            Verdict::Skipped(msg) => assert!(msg.contains("Python marker")),
            other => panic!("expected Skipped, got {other:?}"),
        }
    }

    #[test]
    fn mypy_configured_detects_mypy_ini() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("mypy.ini"), "[mypy]\n").unwrap();
        assert!(mypy_configured(tmp.path()));
    }

    #[test]
    fn mypy_configured_detects_tool_mypy_in_pyproject() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("pyproject.toml"),
            "[tool.mypy]\nstrict = true\n",
        )
        .unwrap();
        assert!(mypy_configured(tmp.path()));
    }

    #[test]
    fn mypy_configured_false_when_neither_present() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(!mypy_configured(tmp.path()));
    }

    #[tokio::test]
    async fn skips_when_mypy_not_configured() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("pyproject.toml"), "[project]\nname='x'\n").unwrap();
        let py = dir.path().join("m.py");
        std::fs::write(&py, "x = 1\n").unwrap();
        let event = BusEvent::Edit(EditEvent {
            path: py,
            diff: "".into(),
        });
        match verify_python_typecheck(&event).await {
            Verdict::Skipped(msg) => assert!(msg.contains("mypy not configured")),
            other => panic!("expected Skipped, got {other:?}"),
        }
    }
}
