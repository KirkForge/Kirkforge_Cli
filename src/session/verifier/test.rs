use crate::session::event_bus::{BusEvent, EditEvent, FileWriteEvent};
/// Test verifier — runs targeted `cargo test` for the edited Rust file.
///
/// This verifier subscribes to `Edit` and `FileWrite` events. When a Rust file
/// inside a Cargo project is modified, it derives a module path prefix from the
/// file path and runs `cargo test <prefix>` in the project root. Only tests
/// whose fully qualified name matches the prefix are executed, keeping the
/// correction loop fast. Failures are returned as a `Verdict::Fixable` with the
/// test output as the `description`, so the model sees the failure verbatim as
/// a correction prompt.
///
/// The test verifier is registered at priority 5 (after rustfmt).
use crate::session::verifier::{FixSuggestion, Verdict, VerificationError};
use std::path::{Path, PathBuf};

/// Walk up from `path` looking for a `Cargo.toml`.
fn find_cargo_root(path: &Path) -> Option<PathBuf> {
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

/// Convert a file path inside a crate into a likely Rust module path prefix.
///
/// For example, `src/foo/bar.rs` -> `foo::bar`, `src/foo/bar/baz.rs` ->
/// `foo::bar::baz`, and `src/main.rs` / `src/lib.rs` -> `` (run all crate
/// tests).
fn module_path_prefix(file_path: &Path, cargo_root: &Path) -> Option<String> {
    let relative = file_path.strip_prefix(cargo_root).ok()?;
    let mut components: Vec<&str> = relative
        .components()
        .map(|c| c.as_os_str().to_str().unwrap_or(""))
        .collect();
    if components.is_empty() {
        return None;
    }
    // Drop the leading `src/` or `tests/` directory.
    let first = components.remove(0);
    if !matches!(first, "src" | "tests") {
        return None;
    }
    let last = components.pop()?;
    let (stem, _ext) = last.split_once('.')?;
    // Full-suite fallback only for `main.rs` / `lib.rs` sitting DIRECTLY at
    // the crate root (`src/main.rs`, `src/lib.rs`). A nested file like
    // `src/foo/main.rs` keeps a targeted filter (`foo::main`) so editing a
    // bin/example doesn't trigger the whole crate's test suite.
    // (bucketlist 3.37)
    let at_crate_root = components.is_empty();
    let mut path_parts = components;
    path_parts.push(stem);
    if (stem == "main" || stem == "lib") && at_crate_root {
        // Running `main` or `lib` as a test filter would not match anything.
        // Fall back to the whole crate by returning an empty prefix, which the
        // caller can translate into `cargo test` without a filter.
        return Some(String::new());
    }
    Some(path_parts.join("::"))
}

/// Run the test verifier against an event.
pub async fn verify_test(event: &BusEvent) -> Verdict {
    let path = match event {
        BusEvent::Edit(EditEvent { path, .. }) => path.clone(),
        BusEvent::FileWrite(FileWriteEvent { path, .. }) => path.clone(),
        _ => return Verdict::Skipped("not a file modification event".into()),
    };

    if path.extension().and_then(|e| e.to_str()) != Some("rs") {
        return Verdict::Skipped(format!("unsupported file type: {}", path.display()));
    }

    let Some(cargo_root) = find_cargo_root(&path) else {
        return Verdict::Skipped(format!("no Cargo.toml found for {}", path.display()));
    };

    let prefix = match module_path_prefix(&path, &cargo_root) {
        Some(p) => p,
        None => {
            return Verdict::Skipped(format!(
                "could not infer module path for {}",
                path.display()
            ))
        }
    };

    let mut args: Vec<String> = vec!["test".into(), "--".into(), "--nocapture".into()];
    if !prefix.is_empty() {
        // Insert the filter before the `--` separator so cargo passes it to the
        // test harness as a substring filter.
        args.insert(1, prefix.clone());
    }

    let test_output = tokio::process::Command::new("cargo")
        .current_dir(&cargo_root)
        .args(&args)
        .output()
        .await;

    let test_output = match test_output {
        Ok(o) => o,
        Err(e) => {
            return Verdict::Unfixable(VerificationError {
                description: "failed to spawn cargo test".into(),
                file: Some(path),
                details: e.to_string(),
            })
        }
    };

    if test_output.status.success() {
        return Verdict::Clean;
    }

    let stdout_text = String::from_utf8_lossy(&test_output.stdout);
    let stderr_text = String::from_utf8_lossy(&test_output.stderr);
    let mut combined: Vec<&str> = stdout_text.lines().chain(stderr_text.lines()).collect();
    // Keep the last few meaningful lines so the model gets a compact failure.
    const TAIL_LINES: usize = 20;
    if combined.len() > TAIL_LINES {
        let start = combined.len() - TAIL_LINES;
        combined = combined.split_off(start);
    }
    let description = combined.join("\n");

    Verdict::Fixable(FixSuggestion {
        description: format!("test failure near {}\n{description}", path.display()),
        file: path,
        original: String::new(),
        replacement: String::new(),
        severity: "error".to_string(),
        command: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_skips_unknown_file_types() {
        let event = BusEvent::Edit(EditEvent {
            path: std::path::PathBuf::from("readme.md"),
            diff: "".into(),
        });
        let v = verify_test(&event).await;
        assert!(matches!(v, Verdict::Skipped(_)));
    }

    #[tokio::test]
    async fn test_skips_non_edit_events() {
        let event = BusEvent::BashExec(crate::session::event_bus::BashExecEvent {
            command: "echo hi".into(),
            exit_code: 0,
            stdout_len: 3,
            stderr_len: 0,
            workdir: None,
        });
        let v = verify_test(&event).await;
        assert!(matches!(v, Verdict::Skipped(_)));
    }

    #[tokio::test]
    async fn test_skips_rust_file_with_no_cargo_root() {
        // A `.rs` file in a temp dir with no Cargo.toml ancestor: the
        // verifier must skip (no project to run `cargo test` against)
        // rather than spawning cargo in an unrelated directory.
        let dir = tempfile::tempdir().unwrap();
        let rs = dir.path().join("lonely.rs");
        std::fs::write(&rs, "fn main() {}").unwrap();
        let event = BusEvent::Edit(EditEvent {
            path: rs.clone(),
            diff: "".into(),
        });
        let v = verify_test(&event).await;
        match v {
            Verdict::Skipped(msg) => assert!(msg.contains("Cargo.toml")),
            other => panic!("expected Skipped for rs file with no cargo root, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_skips_filewrite_rust_with_no_cargo_root() {
        // Same skip path, but via the FileWrite event variant.
        let dir = tempfile::tempdir().unwrap();
        let rs = dir.path().join("orphan.rs");
        std::fs::write(&rs, "fn main() {}").unwrap();
        let event = BusEvent::FileWrite(FileWriteEvent {
            path: rs,
            content_length: 12,
            content_hash: 0,
        });
        let v = verify_test(&event).await;
        assert!(matches!(v, Verdict::Skipped(_)));
    }

    #[test]
    fn test_module_path_prefix_src() {
        let root = std::path::PathBuf::from("/tmp/foo");
        assert_eq!(
            module_path_prefix(&root.join("src/bar.rs"), &root),
            Some("bar".into())
        );
        assert_eq!(
            module_path_prefix(&root.join("src/foo/bar.rs"), &root),
            Some("foo::bar".into())
        );
        assert_eq!(
            module_path_prefix(&root.join("src/main.rs"), &root),
            Some("".into())
        );
        assert_eq!(
            module_path_prefix(&root.join("src/lib.rs"), &root),
            Some("".into())
        );
    }

    #[test]
    fn module_path_prefix_tests_dir_yields_prefix() {
        let root = std::path::PathBuf::from("/tmp/foo");
        assert_eq!(
            module_path_prefix(&root.join("tests/integration.rs"), &root),
            Some("integration".into())
        );
        assert_eq!(
            module_path_prefix(&root.join("tests/sub/integration.rs"), &root),
            Some("sub::integration".into())
        );
    }

    #[test]
    fn module_path_prefix_nested_main_lib_keeps_targeted_filter() {
        // bucketlist 3.37: only crate-root `src/main.rs` / `src/lib.rs`
        // fall back to the full suite (empty prefix). A nested main/lib
        // keeps a targeted filter so it does not run the whole crate.
        let root = std::path::PathBuf::from("/tmp/foo");
        assert_eq!(
            module_path_prefix(&root.join("src/foo/main.rs"), &root),
            Some("foo::main".into()),
            "nested src/foo/main.rs must NOT trigger the full suite"
        );
        assert_eq!(
            module_path_prefix(&root.join("src/bin/lib.rs"), &root),
            Some("bin::lib".into()),
            "nested src/bin/lib.rs must NOT trigger the full suite"
        );
        // Sanity: crate-root main.rs / lib.rs still fall back to empty.
        assert_eq!(
            module_path_prefix(&root.join("src/main.rs"), &root),
            Some(String::new())
        );
        assert_eq!(
            module_path_prefix(&root.join("src/lib.rs"), &root),
            Some(String::new())
        );
    }

    #[test]
    fn module_path_prefix_returns_none_when_not_under_src_or_tests() {
        let root = std::path::PathBuf::from("/tmp/foo");
        assert_eq!(
            module_path_prefix(&root.join("docs/guide.md"), &root),
            None,
            "paths outside src/ or tests/ return None"
        );
        assert_eq!(
            module_path_prefix(&root.join("benches/bench1.rs"), &root),
            None,
            "benches/ is not recognised as a test root"
        );
    }

    #[test]
    fn module_path_prefix_returns_none_when_path_not_under_cargo_root() {
        let root = std::path::PathBuf::from("/tmp/foo");
        assert_eq!(
            module_path_prefix(std::path::Path::new("/elsewhere/x.rs"), &root),
            None,
            "files not under cargo_root return None"
        );
    }

    #[test]
    fn module_path_prefix_returns_none_for_path_without_extension() {
        let root = std::path::PathBuf::from("/tmp/foo");
        assert_eq!(
            module_path_prefix(&root.join("src/no_ext"), &root),
            None,
            "files without an extension cannot produce a stem"
        );
    }

    #[test]
    fn module_path_prefix_returns_none_for_root_relative_to_cargo_root() {
        let root = std::path::PathBuf::from("/tmp/foo");
        assert_eq!(module_path_prefix(&root, &root), None);
    }

    #[test]
    fn find_cargo_root_finds_immediate_parent() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("Cargo.toml"), "").unwrap();
        let src = tmp.path().join("lib.rs");
        let found = find_cargo_root(&src).unwrap();
        assert_eq!(found, tmp.path());
    }

    #[test]
    fn find_cargo_root_walks_up_multiple_levels() {
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

    // This test spawns `cargo test` in a temporary project. It cannot run
    // concurrently with another `cargo` invocation because the Cargo package
    // cache lock serializes all cargo processes, so it is ignored by default.
    // Run it separately when needed: `cargo test --workspace -- --ignored`.
    #[tokio::test]
    #[ignore = "spawns cargo; run separately with cargo test --workspace -- --ignored"]
    async fn test_test_failure_on_temp_project() {
        let dir = std::env::temp_dir().join("kirkforge_test_test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(
            dir.join("Cargo.toml"),
            r#"[package]
name = "kirkforge-test-test"
version = "0.1.0"
edition = "2021"
"#,
        )
        .unwrap();
        std::fs::write(
            dir.join("src/lib.rs"),
            "pub fn add(a: i32, b: i32) -> i32 { a + b }\n\n#[cfg(test)]\nmod tests {\n    use super::*;\n\n    #[test]\n    fn it_fails() {\n        assert_eq!(add(1, 1), 3);\n    }\n}\n",
        )
        .unwrap();

        let path = dir.join("src/lib.rs");
        let event = BusEvent::FileWrite(FileWriteEvent {
            path: path.clone(),
            content_length: 120,
            content_hash: 0,
        });
        let v = verify_test(&event).await;
        match v {
            Verdict::Fixable(suggestion) => {
                assert_eq!(suggestion.file, path);
                assert!(suggestion.description.contains("test failure"));
                assert!(
                    suggestion.description.contains("assertion failed")
                        || suggestion.description.contains("it_fails")
                );
                assert_eq!(suggestion.severity, "error");
                assert!(suggestion.original.is_empty());
                assert!(suggestion.replacement.is_empty());
                assert!(suggestion.command.is_none());
            }
            other => panic!("expected Fixable test failure, got {other:?}"),
        }

        let _ = std::fs::remove_dir_all(&dir);
    }
}
