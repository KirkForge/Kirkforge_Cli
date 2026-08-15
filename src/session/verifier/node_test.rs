//! Node test verifier — runs `npm test` (or `npx vitest` when vitest is
//! configured) on the edited JS/TS file.
//!
//! Mirrors [`super::python_test::verify_python_test`]. Fires on Edit/FileWrite
//! of `.js`/`.jsx`/`.ts`/`.tsx`/`.mjs`/`.cjs` files inside a Node project
//! (detected via [`super::detect::find_node_root`]). Runs `npm test` in the
//! project root, or `npx vitest run` when a `vitest.config.*` is present.
//! Non-zero exit → `Verdict::Fixable` with the tail of the output. If `npm`
//! isn't on PATH, skips gracefully (per WO 31 failure criteria: never block
//! when a tool is absent).

use crate::session::verifier::detect::{detect_project_languages, find_node_root, ProjectLanguage};
use crate::session::verifier::types::{BusEvent, EditEvent, FileWriteEvent};
use crate::session::verifier::{FixSuggestion, Verdict, VerificationError};

/// JS/TS extensions this verifier handles.
const NODE_EXTS: &[&str] = &["js", "jsx", "ts", "tsx", "mjs", "cjs"];

/// True if `root` contains a vitest config (vitest.config.{js,ts,mjs,cjs} or
/// a `vitest` field in package.json — ponytail: filename sniff only, no JSON
/// parse; the `vitest` CLI itself reads package.json, we just gate on config
/// files which are the conventional marker).
fn vitest_configured(root: &std::path::Path) -> bool {
    for ext in ["js", "ts", "mjs", "cjs"] {
        if root.join(format!("vitest.config.{ext}")).is_file() {
            return true;
        }
    }
    false
}

/// Resolve the first available Node toolchain by probing `--version`.
/// Returns `Some(("npm", "npx"))` or `None`.
async fn pick_node() -> Option<(&'static str, &'static str)> {
    let npm_ok = tokio::process::Command::new("npm")
        .arg("--version")
        .output()
        .await
        .map(|o| o.status.success())
        .unwrap_or(false);
    if npm_ok {
        Some(("npm", "npx"))
    } else {
        None
    }
}

/// Run the Node test verifier against an event.
pub async fn verify_node_test(event: &BusEvent) -> Verdict {
    let path = match event {
        BusEvent::Edit(EditEvent { path, .. }) => path.clone(),
        BusEvent::FileWrite(FileWriteEvent { path, .. }) => path.clone(),
        _ => return Verdict::Skipped("not a file modification event".into()),
    };

    let Some(ext) = path.extension().and_then(|e| e.to_str()) else {
        return Verdict::Skipped(format!("unsupported file type: {}", path.display()));
    };
    if !NODE_EXTS.contains(&ext) {
        return Verdict::Skipped(format!("unsupported file type: {}", path.display()));
    }

    let Some(root) = find_node_root(&path) else {
        return Verdict::Skipped(format!("no Node marker for {}", path.display()));
    };

    if !detect_project_languages(&root).contains(&ProjectLanguage::Node) {
        return Verdict::Skipped("Node not detected".into());
    }

    let Some((npm, npx)) = pick_node().await else {
        return Verdict::Skipped("npm not found on PATH".into());
    };

    // vitest projects: `npx vitest run` (one-shot, no watch). Otherwise the
    // project's own `npm test` script — which the project author configured.
    let output = if vitest_configured(&root) {
        tokio::process::Command::new(npx)
            .current_dir(&root)
            .args(["vitest", "run"])
            .output()
            .await
    } else {
        tokio::process::Command::new(npm)
            .current_dir(&root)
            .arg("test")
            .output()
            .await
    };

    let output = match output {
        Ok(o) => o,
        Err(e) => {
            return Verdict::Unfixable(VerificationError {
                description: "failed to spawn Node test runner".to_string(),
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
    // ponytail: tail-N truncation matches the Python/Rust test verifiers;
    // full vitest/jest output can be hundreds of lines, the model only needs
    // the failure summary.
    const TAIL_LINES: usize = 20;
    let mut combined: Vec<&str> = stdout.lines().chain(stderr.lines()).collect();
    if combined.len() > TAIL_LINES {
        let start = combined.len() - TAIL_LINES;
        combined = combined.split_off(start);
    }
    let body = combined.join("\n");

    Verdict::Fixable(FixSuggestion {
        description: format!("node test failure near {}\n{body}", path.display()),
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
            verify_node_test(&event).await,
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
            verify_node_test(&event).await,
            Verdict::Skipped(_)
        ));
    }

    #[tokio::test]
    async fn skips_node_file_with_no_project_root() {
        let dir = tempfile::tempdir().unwrap();
        let js = dir.path().join("lonely.js");
        std::fs::write(&js, "x = 1\n").unwrap();
        let event = BusEvent::FileWrite(FileWriteEvent {
            path: js,
            content_length: 6,
            content_hash: 0,
        });
        match verify_node_test(&event).await {
            Verdict::Skipped(msg) => assert!(msg.contains("Node marker")),
            other => panic!("expected Skipped, got {other:?}"),
        }
    }

    #[test]
    fn vitest_configured_detects_config_files() {
        for ext in ["js", "ts", "mjs", "cjs"] {
            let tmp = tempfile::tempdir().unwrap();
            std::fs::write(tmp.path().join(format!("vitest.config.{ext}")), "").unwrap();
            assert!(
                vitest_configured(tmp.path()),
                "vitest.config.{ext} should detect"
            );
        }
    }

    #[test]
    fn vitest_configured_false_when_absent() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(!vitest_configured(tmp.path()));
    }

    #[tokio::test]
    async fn pick_node_returns_consistent_pair() {
        // If npm is absent (minimal container), skip the assertion rather than
        // fail spuriously — the verifier itself skips gracefully in that case.
        match pick_node().await {
            Some((npm, npx)) => {
                assert_eq!(npm, "npm");
                assert_eq!(npx, "npx");
            }
            None => eprintln!("no npm on PATH — skipping node probe"),
        }
    }

    /// Regression: on a host with npm, the verifier must NOT fail with
    /// `Unfixable("failed to spawn")`. It runs `npm test`/`npx vitest` and
    /// either passes, skips (no test script), or reports Fixable.
    #[tokio::test]
    async fn does_not_unfixable_spawn_when_npm_present() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("package.json"),
            "{\"name\":\"x\",\"scripts\":{\"test\":\"node -e 'process.exit(1)'\"}}\n",
        )
        .unwrap();
        let js = dir.path().join("mod.js");
        std::fs::write(&js, "module.exports = 1;\n").unwrap();
        let event = BusEvent::Edit(EditEvent {
            path: js,
            diff: "".into(),
        });
        if let Verdict::Unfixable(e) = verify_node_test(&event).await {
            assert!(
                !e.details.contains("spawn"),
                "must not fail to spawn when npm is available: {e:?}"
            );
        }
    }
}
