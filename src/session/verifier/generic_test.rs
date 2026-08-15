//! Generic fallback test verifier — tries `make test`, then `ctest`, then
//! `./test.sh` (first that exists wins).
//!
//! Mirrors [`super::python_test::verify_python_test`] in shape but fires as the
//! fallback when no language-specific verifier (Rust/Python/Node/Go) applies.
//! The trigger: an edited file whose project root has none of the language
//! markers but DOES have a generic test runner. "Project root" for the generic
//! case is the edited file's parent directory (no marker to walk up to), so we
//! use the immediate parent — ponytail: this avoids scanning the whole
//! filesystem upward when there's no marker to anchor on; the generic runner
//! is conventionally at the repo root which is the file's parent in the
//! common case. Upgrade path: walk up to a `.git` boundary if the
//! immediate-parent heuristic misfires.
//!
//! Non-zero exit → `Verdict::Fixable` with the tail of the output. If no
//! generic runner is present, skips gracefully.

use std::path::Path;

use crate::session::verifier::detect::detect_project_languages;
use crate::session::verifier::types::{BusEvent, EditEvent, FileWriteEvent};
use crate::session::verifier::{FixSuggestion, Verdict, VerificationError};

/// True if `make` is on PATH.
async fn make_available() -> bool {
    tokio::process::Command::new("make")
        .arg("--version")
        .output()
        .await
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// True if `ctest` is on PATH.
async fn ctest_available() -> bool {
    tokio::process::Command::new("ctest")
        .arg("--version")
        .output()
        .await
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// True if `./test.sh` exists in `root` and is executable (file present).
fn test_script_present(root: &Path) -> bool {
    root.join("test.sh").is_file()
}

/// Pick the first available generic runner. Returns `(runner, argv)` where
/// argv is the full command to invoke. Returns `None` if none exists.
async fn pick_runner(root: &Path) -> Option<(&'static str, Vec<String>)> {
    // `make test` — needs make on PATH AND a Makefile with a test target.
    // ponytail: we don't parse the Makefile to confirm the target exists;
    // `make test` failing with "No rule to make target 'test'" surfaces as a
    // non-zero exit which the verifier reports as Fixable — slightly noisy but
    // honest. Upgrade path: grep Makefile for `^test:` before invoking.
    if make_available().await && root.join("Makefile").is_file() {
        return Some(("make", vec!["test".into()]));
    }
    // `ctest` — needs ctest on PATH AND a CMakeTestCache or CTestTestfile.cmake
    // (the conventional ctest markers).
    if ctest_available().await
        && (root.join("CTestTestfile.cmake").is_file() || root.join("CMakeCache.txt").is_file())
    {
        return Some(("ctest", vec![]));
    }
    // `./test.sh` — needs the script to exist (executability is enforced by
    // the OS at spawn; we only check presence to gate).
    if test_script_present(root) {
        return Some(("./test.sh", vec![]));
    }
    None
}

/// Run the generic fallback test verifier against an event.
pub async fn verify_generic_test(event: &BusEvent) -> Verdict {
    let path = match event {
        BusEvent::Edit(EditEvent { path, .. }) => path.clone(),
        BusEvent::FileWrite(FileWriteEvent { path, .. }) => path.clone(),
        _ => return Verdict::Skipped("not a file modification event".into()),
    };

    // The generic verifier is the FALLBACK — it must NOT fire when a
    // language-specific verifier applies. Use the edited file's parent as the
    // candidate root and confirm no language marker is present there.
    let Some(root) = path.parent().map(std::path::Path::to_path_buf) else {
        return Verdict::Skipped("edited file has no parent directory".into());
    };

    if !detect_project_languages(&root).is_empty() {
        return Verdict::Skipped("language-specific verifier applies".into());
    }

    let Some((runner, args)) = pick_runner(&root).await else {
        return Verdict::Skipped("no generic test runner found (make/ctest/test.sh)".into());
    };

    let output = tokio::process::Command::new(runner)
        .current_dir(&root)
        .args(&args)
        .output()
        .await;

    let output = match output {
        Ok(o) => o,
        Err(e) => {
            return Verdict::Unfixable(VerificationError {
                description: format!("failed to spawn {runner}"),
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
    const TAIL_LINES: usize = 20;
    let mut combined: Vec<&str> = stdout.lines().chain(stderr.lines()).collect();
    if combined.len() > TAIL_LINES {
        let start = combined.len() - TAIL_LINES;
        combined = combined.split_off(start);
    }
    let body = combined.join("\n");

    Verdict::Fixable(FixSuggestion {
        description: format!("{runner} failure near {}\n{body}", path.display()),
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
            verify_generic_test(&event).await,
            Verdict::Skipped(_)
        ));
    }

    #[tokio::test]
    async fn skips_when_language_marker_present() {
        // A Rust project root has a language-specific verifier; the generic
        // fallback must defer.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("Cargo.toml"), "").unwrap();
        let src = dir.path().join("lib.rs");
        std::fs::write(&src, "fn main() {}\n").unwrap();
        let event = BusEvent::Edit(EditEvent {
            path: src,
            diff: "".into(),
        });
        match verify_generic_test(&event).await {
            Verdict::Skipped(msg) => assert!(msg.contains("language-specific")),
            other => panic!("expected Skipped, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn skips_when_no_runner_present() {
        let dir = tempfile::tempdir().unwrap();
        // No marker, no Makefile, no test.sh, no ctest markers.
        let src = dir.path().join("weird.xyz");
        std::fs::write(&src, "data\n").unwrap();
        let event = BusEvent::Edit(EditEvent {
            path: src,
            diff: "".into(),
        });
        match verify_generic_test(&event).await {
            Verdict::Skipped(msg) => assert!(msg.contains("no generic test runner")),
            other => panic!("expected Skipped, got {other:?}"),
        }
    }

    #[test]
    fn test_script_present_detects_file() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("test.sh"), "#!/bin/sh\n").unwrap();
        assert!(test_script_present(tmp.path()));
    }

    #[test]
    fn test_script_present_false_when_absent() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(!test_script_present(tmp.path()));
    }

    #[tokio::test]
    async fn pick_runner_prefers_make_when_makefile_present() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("Makefile"), "test:\n\techo hi\n").unwrap();
        std::fs::write(tmp.path().join("test.sh"), "#!/bin/sh\n").unwrap();
        // If make is on PATH, it wins. If not (minimal container), the
        // ./test.sh fallback should be picked.
        match pick_runner(tmp.path()).await {
            Some((runner, args)) => {
                assert!(runner == "make" || runner == "./test.sh");
                if runner == "make" {
                    assert_eq!(args, vec!["test".to_string()]);
                } else {
                    assert!(args.is_empty());
                }
            }
            None => eprintln!("no make/test.sh on PATH — skipping runner pick"),
        }
    }
}
