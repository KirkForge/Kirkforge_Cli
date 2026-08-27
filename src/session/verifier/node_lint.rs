//! Node lint verifier — runs `npx eslint .` and/or `npx tsc --noEmit` on the
//! edited JS/TS file.
//!
//! Mirrors [`super::python_lint::verify_python_lint`]. Fires on Edit/FileWrite
//! of JS/TS files inside a Node project. Runs whichever of eslint / tsc is
//! configured (eslint config present, or tsconfig.json present). Lint findings
//! → `Verdict::Fixable` with the tool output. If neither tool/config is
//! available, skips gracefully.

use crate::session::verifier::detect::{find_node_root, ProjectLanguage};
use crate::session::verifier::helpers::{
    command_finding, head_body, language_gate, modified_path, tool_on_path, Gate,
};
use crate::session::verifier::types::BusEvent;
use crate::session::verifier::Verdict;

const NODE_EXTS: &[&str] = &["js", "jsx", "ts", "tsx", "mjs", "cjs"];

/// True if an eslint config file is present at `root`. Covers the modern
/// `eslint.config.{js,ts,mjs,cjs}` flat config plus the legacy `.eslintrc`
/// /`.eslintrc.json`/`.eslintrc.js`.
fn eslint_configured(root: &std::path::Path) -> bool {
    for ext in ["js", "ts", "mjs", "cjs"] {
        if root.join(format!("eslint.config.{ext}")).is_file() {
            return true;
        }
    }
    for name in [
        ".eslintrc",
        ".eslintrc.json",
        ".eslintrc.js",
        ".eslintrc.cjs",
    ] {
        if root.join(name).is_file() {
            return true;
        }
    }
    false
}

/// True if `tsconfig.json` is present at `root`.
fn tsc_configured(root: &std::path::Path) -> bool {
    root.join("tsconfig.json").is_file()
}

/// Probe `npx <tool> --version` to confirm the tool is invocable. Returns true
/// if the probe succeeded (the tool exists and npx can resolve it).
async fn npx_tool_available(tool: &str) -> bool {
    tool_on_path("npx", &[tool, "--version"]).await
}

/// Run the Node lint verifier against an event.
pub async fn verify_node_lint(event: &BusEvent) -> Verdict {
    let Some(path) = modified_path(event) else {
        return Verdict::Skipped("not a file modification event".into());
    };

    let root = match language_gate(
        &path,
        NODE_EXTS,
        "Node",
        find_node_root,
        ProjectLanguage::Node,
    ) {
        Gate::Root(root) => root,
        Gate::Skip(verdict) => return verdict,
    };

    let want_eslint = eslint_configured(&root);
    let want_tsc = tsc_configured(&root)
        && path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| matches!(e, "ts" | "tsx"));
    if !want_eslint && !want_tsc {
        return Verdict::Skipped("neither eslint nor tsc configured".into());
    }

    let mut bodies: Vec<String> = Vec::new();
    let mut had_failure = false;

    if want_eslint && npx_tool_available("eslint").await {
        let out = tokio::process::Command::new("npx")
            .current_dir(&root)
            .args(["eslint", "."])
            .output()
            .await;
        if let Ok(o) = out {
            if !o.status.success() {
                had_failure = true;
            }
            let body = head_body(
                &String::from_utf8_lossy(&o.stdout),
                &String::from_utf8_lossy(&o.stderr),
                20,
            );
            if !body.trim().is_empty() {
                bodies.push(format!("eslint:\n{body}"));
            }
        }
    }

    if want_tsc && npx_tool_available("typescript").await {
        let out = tokio::process::Command::new("npx")
            .current_dir(&root)
            .args(["tsc", "--noEmit"])
            .output()
            .await;
        if let Ok(o) = out {
            if !o.status.success() {
                had_failure = true;
            }
            let body = head_body(
                &String::from_utf8_lossy(&o.stdout),
                &String::from_utf8_lossy(&o.stderr),
                20,
            );
            if !body.trim().is_empty() {
                bodies.push(format!("tsc:\n{body}"));
            }
        }
    }

    if !had_failure {
        // Either everything passed, or no configured tool was actually
        // invocable (npx couldn't resolve it) — treat the latter as a skip so
        // we don't claim clean on a tool we couldn't run.
        if bodies.is_empty() {
            return Verdict::Skipped("eslint/tsc configured but not invocable via npx".into());
        }
        return Verdict::Clean;
    }

    command_finding(
        format!(
            "node lint findings near {}\n{}",
            path.display(),
            bodies.join("\n")
        ),
        path,
        "warning",
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
            verify_node_lint(&event).await,
            Verdict::Skipped(_)
        ));
    }

    #[tokio::test]
    async fn skips_non_node_extensions() {
        let event = BusEvent::Edit(EditEvent {
            path: std::path::PathBuf::from("foo.rs"),
            diff: "".into(),
        });
        assert!(matches!(
            verify_node_lint(&event).await,
            Verdict::Skipped(_)
        ));
    }

    #[tokio::test]
    async fn skips_node_file_with_no_project_root() {
        let dir = tempfile::tempdir().unwrap();
        let js = dir.path().join("orphan.js");
        std::fs::write(&js, "x = 1\n").unwrap();
        let event = BusEvent::FileWrite(FileWriteEvent {
            path: js,
            content_length: 6,
            content_hash: 0,
        });
        match verify_node_lint(&event).await {
            Verdict::Skipped(msg) => assert!(msg.contains("Node marker")),
            other => panic!("expected Skipped, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn skips_when_neither_tool_configured() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("package.json"), "{\"name\":\"x\"}\n").unwrap();
        let js = dir.path().join("m.js");
        std::fs::write(&js, "x = 1\n").unwrap();
        let event = BusEvent::Edit(EditEvent {
            path: js,
            diff: "".into(),
        });
        match verify_node_lint(&event).await {
            Verdict::Skipped(msg) => assert!(msg.contains("neither eslint nor tsc")),
            other => panic!("expected Skipped, got {other:?}"),
        }
    }

    #[test]
    fn eslint_configured_detects_flat_config() {
        for ext in ["js", "ts", "mjs", "cjs"] {
            let tmp = tempfile::tempdir().unwrap();
            std::fs::write(tmp.path().join(format!("eslint.config.{ext}")), "").unwrap();
            assert!(
                eslint_configured(tmp.path()),
                "eslint.config.{ext} should detect"
            );
        }
    }

    #[test]
    fn eslint_configured_detects_legacy_eslintrc() {
        for name in [
            ".eslintrc",
            ".eslintrc.json",
            ".eslintrc.js",
            ".eslintrc.cjs",
        ] {
            let tmp = tempfile::tempdir().unwrap();
            std::fs::write(tmp.path().join(name), "").unwrap();
            assert!(eslint_configured(tmp.path()), "{name} should detect");
        }
    }

    #[test]
    fn eslint_configured_false_when_absent() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(!eslint_configured(tmp.path()));
    }

    #[test]
    fn tsc_configured_detects_tsconfig() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("tsconfig.json"), "{}").unwrap();
        assert!(tsc_configured(tmp.path()));
    }

    #[test]
    fn tsc_configured_false_when_absent() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(!tsc_configured(tmp.path()));
    }
}
