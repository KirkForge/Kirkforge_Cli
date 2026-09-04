//! Python lint verifier — runs `ruff` (or `flake8`) on the edited Python file.
//!
//! Mirrors [`super::lint::verify_lint`]: subscribes to `Edit` and `FileWrite`
//! events. When a `.py` file inside a Python project is modified, runs
//! `ruff check {file}` (preferred) or `flake8 {file}` (fallback) in the
//! project root. Lint findings are returned as `Verdict::Fixable` with the
//! tool output. If neither tool is installed, the verifier skips gracefully.

use crate::session::verifier::bus::{
    BusVerifier, Severity, VerdictEntry, VerifierSource, VerifyContext,
};
use crate::session::verifier::detect::{find_python_root, ProjectLanguage};
use crate::session::verifier::helpers::{
    command_finding, head_body, language_gate, modified_path, tool_on_path, tool_on_path_sync, Gate,
};
use crate::session::verifier::types::BusEvent;
use crate::session::verifier::{FixSuggestion, Verdict};

/// Pick the first available Python linter binary by probing `--version`.
/// Returns the binary name (`"ruff"` or `"flake8"`) or `None`.
async fn pick_linter() -> Option<&'static str> {
    for tool in ["ruff", "flake8"] {
        if tool_on_path(tool, &["--version"]).await {
            return Some(tool);
        }
    }
    None
}

/// Run the Python lint verifier against an event.
pub async fn verify_python_lint(event: &BusEvent) -> Verdict {
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

    let body = head_body(
        &String::from_utf8_lossy(&output.stdout),
        &String::from_utf8_lossy(&output.stderr),
        20,
    );

    command_finding(
        format!("{tool} findings near {}\n{body}", path.display()),
        path,
        "warning",
    )
}

// ── BusVerifier impl (WO 47.14) ─────────────────────────────────────────

/// Python lint verifier registered on the `VerifierBus`. WO 47.14.
pub struct PythonLintVerifier;

impl BusVerifier for PythonLintVerifier {
    fn name(&self) -> &str {
        "python_lint"
    }

    fn verify(&self, ctx: &VerifyContext) -> Vec<VerdictEntry> {
        let Some(path) = ctx.changed_files.first() else {
            return vec![];
        };
        let root = match language_gate(
            path,
            &["py"],
            "Python",
            find_python_root,
            ProjectLanguage::Python,
        ) {
            Gate::Root(root) => root,
            Gate::Skip(_) => return vec![],
        };
        let tool = ["ruff", "flake8"]
            .into_iter()
            .find(|bin| tool_on_path_sync(bin, &["--version"]));
        let Some(tool) = tool else {
            return vec![];
        };
        let output = match tool {
            "ruff" => std::process::Command::new(tool)
                .current_dir(&root)
                .args(["check", &path.to_string_lossy()])
                .output(),
            _ => std::process::Command::new(tool)
                .current_dir(&root)
                .arg(path)
                .output(),
        };
        let output = match output {
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
            description: format!("{tool} findings near {}\n{body}", path.display()),
            file: path.clone(),
            original: String::new(),
            replacement: String::new(),
            severity: "warning".to_string(),
            command: None,
            line: None,
        };
        vec![VerdictEntry {
            source: VerifierSource::Lint,
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
