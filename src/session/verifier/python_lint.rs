//! Python lint verifier — runs `ruff` (or `flake8`) on the edited Python file.
//!
//! Mirrors [`super::lint::verify_lint`]: subscribes to `Edit` and `FileWrite`
//! events. When a `.py` file inside a Python project is modified, runs
//! `ruff check {file}` (preferred) or `flake8 {file}` (fallback) in the
//! project root. Lint findings are returned as `Verdict::Fixable` with the
//! tool output. If neither tool is installed, the verifier skips gracefully.

use crate::session::verifier::detect::{
    detect_project_languages, find_python_root, ProjectLanguage,
};
use crate::session::verifier::types::{BusEvent, EditEvent, FileWriteEvent};
use crate::session::verifier::{FixSuggestion, Verdict};

/// Pick the first available Python linter binary by probing `--version`.
/// Returns the binary name (`"ruff"` or `"flake8"`) or `None`.
async fn pick_linter() -> Option<&'static str> {
    for tool in ["ruff", "flake8"] {
        let ok = tokio::process::Command::new(tool)
            .arg("--version")
            .output()
            .await
            .map(|o| o.status.success())
            .unwrap_or(false);
        if ok {
            return Some(tool);
        }
    }
    None
}

/// Run the Python lint verifier against an event.
pub async fn verify_python_lint(event: &BusEvent) -> Verdict {
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

    let Some(tool) = pick_linter().await else {
        return Verdict::Skipped("neither ruff nor flake8 installed".into());
    };

    // ruff uses `check <path>`; flake8 takes the path directly.
    let output = match tool {
        "ruff" => {
            tokio::process::Command::new(tool)
                .current_dir(&root)
                .args(["check", &path.to_string_lossy()])
                .output()
                .await
        }
        _ => {
            tokio::process::Command::new(tool)
                .current_dir(&root)
                .arg(&path)
                .output()
                .await
        }
    };

    let output = match output {
        Ok(o) => o,
        Err(e) => return Verdict::Skipped(format!("{tool} not available: {e}")),
    };

    if output.status.success() {
        return Verdict::Clean;
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let body: String = if !stdout.trim().is_empty() {
        stdout.lines().take(20).collect::<Vec<_>>().join("\n")
    } else {
        stderr.lines().take(20).collect::<Vec<_>>().join("\n")
    };

    Verdict::Fixable(FixSuggestion {
        description: format!("{tool} findings near {}\n{body}", path.display()),
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
        assert!(matches!(
            verify_python_lint(&event).await,
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
            verify_python_lint(&event).await,
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
        match verify_python_lint(&event).await {
            Verdict::Skipped(msg) => assert!(msg.contains("Python marker")),
            other => panic!("expected Skipped, got {other:?}"),
        }
    }
}
