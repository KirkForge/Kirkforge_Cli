use std::path::{Path, PathBuf};

use crate::session::verifier::detect::{detect_project_languages, ProjectLanguage};
use crate::session::verifier::types::{BusEvent, EditEvent, FileWriteEvent};
use crate::session::verifier::{FixSuggestion, Verdict};

pub(super) fn find_cargo_root(path: &Path) -> Option<PathBuf> {
    let mut dir = path.parent()?;
    loop {
        if dir.join("Cargo.toml").exists() {
            return Some(dir.to_path_buf());
        }
        match dir.parent() {
            Some(parent) => dir = parent,
            None => return None,
        }
    }
}

/// Path of the file an Edit/FileWrite event modified; None for other events.
pub(super) fn modified_path(event: &BusEvent) -> Option<PathBuf> {
    match event {
        BusEvent::Edit(EditEvent { path, .. }) => Some(path.clone()),
        BusEvent::FileWrite(FileWriteEvent { path, .. }) => Some(path.clone()),
        _ => None,
    }
}

/// Outcome of the shared verifier prelude: the confirmed project root, or
/// the `Skipped` verdict explaining why the verifier does not apply. Not a
/// `Result` — a skip is a normal outcome, not an error (and `Verdict` is
/// too large for `result_large_err`'s budget).
#[derive(Debug)]
pub(super) enum Gate {
    Root(PathBuf),
    Skip(Verdict),
}

/// Common prelude of the marker-gated command verifiers: extension allowlist,
/// project-root walk, language confirmation.
pub(super) fn language_gate(
    path: &Path,
    exts: &[&str],
    lang_label: &str,
    find_root: fn(&Path) -> Option<PathBuf>,
    lang: ProjectLanguage,
) -> Gate {
    if !path
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| exts.contains(&e))
    {
        return Gate::Skip(Verdict::Skipped(format!(
            "unsupported file type: {}",
            path.display()
        )));
    }
    let Some(root) = find_root(path) else {
        return Gate::Skip(Verdict::Skipped(format!(
            "no {lang_label} marker for {}",
            path.display()
        )));
    };
    if !detect_project_languages(&root).contains(&lang) {
        return Gate::Skip(Verdict::Skipped(format!("{lang_label} not detected")));
    }
    Gate::Root(root)
}

/// First `lines` lines of stdout, falling back to stderr when stdout is
/// empty — the body shape the linter-style verifiers report.
pub(super) fn head_body(stdout: &str, stderr: &str, lines: usize) -> String {
    let body = if !stdout.trim().is_empty() {
        stdout
    } else {
        stderr
    };
    body.lines().take(lines).collect::<Vec<_>>().join("\n")
}

/// Last `lines` lines of stdout+stderr chained — the body shape the
/// test-runner verifiers report.
pub(super) fn tail_body(stdout: &str, stderr: &str, lines: usize) -> String {
    let mut combined: Vec<&str> = stdout.lines().chain(stderr.lines()).collect();
    if combined.len() > lines {
        let start = combined.len() - lines;
        combined = combined.split_off(start);
    }
    combined.join("\n")
}

/// The `FixSuggestion` shape every command-runner verifier emits: the tool
/// offers no deterministic replacement, so `original`/`replacement` are empty
/// and `command` is None.
pub(super) fn command_finding(description: String, file: PathBuf, severity: &str) -> Verdict {
    Verdict::Fixable(FixSuggestion {
        description,
        file,
        original: String::new(),
        replacement: String::new(),
        severity: severity.to_string(),
        command: None,
        line: None,
    })
}

/// True if `bin` runs and exits 0 when probed with `probe_args` — the
/// toolchain probe every command verifier uses before spawning the real
/// command.
pub(super) async fn tool_on_path(bin: &str, probe_args: &[&str]) -> bool {
    tokio::process::Command::new(bin)
        .args(probe_args)
        .output()
        .await
        .map(|o| o.status.success())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn find_cargo_root_finds_immediate_parent_cargo_toml() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("Cargo.toml"), "").unwrap();
        let src = tmp.path().join("src/lib.rs");
        std::fs::create_dir_all(src.parent().unwrap()).unwrap();
        let found = find_cargo_root(&src).unwrap();
        assert_eq!(found, tmp.path());
    }

    #[test]
    fn find_cargo_root_walks_up_multiple_dirs() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("Cargo.toml"), "").unwrap();
        let deep = tmp.path().join("a/b/c/deep.rs");
        std::fs::create_dir_all(deep.parent().unwrap()).unwrap();
        let found = find_cargo_root(&deep).unwrap();
        assert_eq!(found, tmp.path());
    }

    #[test]
    fn find_cargo_root_returns_none_when_no_cargo_toml_in_ancestors() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("lonely.rs");
        assert!(find_cargo_root(&path).is_none());
    }

    #[test]
    fn modified_path_extracts_edit_and_file_write() {
        let p = PathBuf::from("a.go");
        assert_eq!(
            modified_path(&BusEvent::Edit(EditEvent {
                path: p.clone(),
                diff: "".into()
            })),
            Some(p.clone())
        );
        assert_eq!(
            modified_path(&BusEvent::FileWrite(FileWriteEvent {
                path: p.clone(),
                content_length: 1,
                content_hash: 0
            })),
            Some(p)
        );
        assert_eq!(
            modified_path(&BusEvent::BashExec(
                crate::session::verifier::types::BashExecEvent {
                    command: "ls".into(),
                    exit_code: 0,
                    stdout_len: 0,
                    stderr_len: 0,
                    workdir: None,
                }
            )),
            None
        );
    }

    #[test]
    fn language_gate_messages_match_the_verifier_contract() {
        let tmp = tempfile::tempdir().unwrap();
        // Wrong extension.
        match language_gate(
            &tmp.path().join("x.rs"),
            &["go"],
            "Go",
            crate::session::verifier::detect::find_go_root,
            ProjectLanguage::Go,
        ) {
            Gate::Skip(Verdict::Skipped(msg)) => {
                assert!(msg.starts_with("unsupported file type"))
            }
            other => panic!("expected Skipped, got {other:?}"),
        }
        // Right extension, no marker anywhere up the tree.
        match language_gate(
            &tmp.path().join("x.go"),
            &["go"],
            "Go",
            crate::session::verifier::detect::find_go_root,
            ProjectLanguage::Go,
        ) {
            Gate::Skip(Verdict::Skipped(msg)) => assert!(msg.contains("no Go marker")),
            other => panic!("expected Skipped, got {other:?}"),
        }
        // Marker present: gate passes and returns the marker directory.
        std::fs::write(tmp.path().join("go.mod"), "module x\n").unwrap();
        match language_gate(
            &tmp.path().join("x.go"),
            &["go"],
            "Go",
            crate::session::verifier::detect::find_go_root,
            ProjectLanguage::Go,
        ) {
            Gate::Root(root) => assert_eq!(root, tmp.path()),
            other => panic!("expected Root, got {other:?}"),
        }
    }

    #[test]
    fn head_body_prefers_stdout_and_falls_back_to_stderr() {
        assert_eq!(head_body("a\nb\n", "c\nd\n", 1), "a");
        assert_eq!(head_body("  \n", "c\nd\n", 5), "c\nd");
    }

    #[test]
    fn tail_body_keeps_the_last_lines_of_both_streams() {
        let stdout: String = (0..15).map(|i| format!("s{i}\n")).collect();
        let stderr: String = (0..15).map(|i| format!("e{i}\n")).collect();
        let body = tail_body(&stdout, &stderr, 20);
        let lines: Vec<&str> = body.lines().collect();
        assert_eq!(lines.len(), 20);
        assert_eq!(*lines.first().unwrap(), "s10");
        assert_eq!(*lines.last().unwrap(), "e14");
    }
}
