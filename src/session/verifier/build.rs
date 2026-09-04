use crate::session::error_recovery;
use crate::session::verifier::bus::{
    BusVerifier, Severity, VerdictEntry, VerifierSource, VerifyContext,
};
use crate::session::verifier::helpers::find_cargo_root;
use crate::session::verifier::types::{BusEvent, CommandRunner, EditEvent, FileWriteEvent};
/// Build verifier — runs `cargo build` on Rust files and reports compiler errors.
///
/// This verifier subscribes to `Edit` and `FileWrite` events. When a Rust file
/// inside a Cargo project is modified, it runs
/// `cargo build --message-format=json` in the project root and parses the JSON
/// output. The first compiler `error` that maps back to the modified file is
/// returned as a model-facing `FixSuggestion` with empty `original`/`replacement`
/// and `command` set to `None`, because the compiler does not provide
/// deterministic text replacements.
///
/// The build verifier is registered at priority 3 (after lint, before
/// rustfmt).
use crate::session::verifier::{FixSuggestion, Verdict, VerificationError};
use std::path::Path;

/// Parse a single cargo JSON line and, if it is a compiler error for the
/// target file, return a `FixSuggestion`.
fn parse_build_json(line: &str, target_path: &Path, cargo_root: &Path) -> Option<FixSuggestion> {
    let value: serde_json::Value = serde_json::from_str(line).ok()?;
    if value.get("reason").and_then(|v| v.as_str()) != Some("compiler-message") {
        return None;
    }
    let message = value.get("message")?;
    let level = message.get("level").and_then(|v| v.as_str())?;
    if level != "error" {
        return None;
    }
    let text = message.get("message").and_then(|v| v.as_str())?.to_string();
    let spans = message.get("spans")?.as_array()?;
    for span in spans {
        let file_name = span.get("file_name").and_then(|v| v.as_str())?;
        let line_start = span.get("line_start").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
        let resolved = cargo_root.join(file_name);
        if resolved == target_path {
            let mut description = format!("{text} at {file_name}:{line_start}");
            if let Some(hint) = error_recovery::classify_error(&text) {
                description.push('\n');
                description.push_str(&error_recovery::render_hint(&hint));
            }
            return Some(FixSuggestion {
                description,
                file: target_path.to_path_buf(),
                original: String::new(),
                replacement: String::new(),
                severity: level.to_string(),
                command: None,
                line: Some(line_start as u32),
            });
        }
    }
    None
}

/// Run the build verifier against an event.
///
/// `runner` abstracts the `cargo build` subprocess so unit tests can feed
/// canned compiler JSON through the full event→Verdict path without spawning
/// real Cargo. Production passes [`SystemCommandRunner`].
pub async fn verify_build(event: &BusEvent, runner: &dyn CommandRunner) -> Verdict {
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

    // Run the build via the injected runner. `SystemCommandRunner` uses the
    // blocking `std::process::Command::output`; the prior code blocked the
    // async worker on `tokio::process::Command::output().await` too, so this
    // is no worse. A `spawn_blocking` wrap would require `'static` on the
    // `&dyn CommandRunner` (it escapes the closure) — not worth the Arc
    // indirection for a post-edit verifier that runs outside the hot path.
    // ponytail: ceiling — a long cargo build blocks one worker; acceptable
    // because the verifier is not per-token. Upgrade path: box the runner
    // in an `Arc<dyn CommandRunner + Send + Sync>` and `spawn_blocking` it.
    let outcome = runner.run("cargo", &["build", "--message-format=json"], &cargo_root);

    use crate::session::verifier::types::ExitState;
    match outcome.status {
        ExitState::SpawnFailed(msg) => {
            return Verdict::Skipped(format!("cargo not available: {msg}"))
        }
        ExitState::Success | ExitState::Code(_) => {}
    }

    let stdout = String::from_utf8_lossy(&outcome.stdout);
    for line in stdout.lines() {
        if let Some(suggestion) = parse_build_json(line, &path, &cargo_root) {
            return Verdict::Fixable(suggestion);
        }
    }

    match outcome.status {
        ExitState::Success => Verdict::Clean,
        ExitState::Code(_) => {
            // Could not extract a concrete finding — report the first few
            // stderr lines.
            let stderr = String::from_utf8_lossy(&outcome.stderr);
            let stderr_summary = stderr.lines().take(5).collect::<Vec<_>>().join("\n");
            let mut details = stderr_summary;
            if let Some(hint) = error_recovery::classify_error(&stderr) {
                details.push('\n');
                details.push_str(&error_recovery::render_hint(&hint));
            }
            Verdict::Unfixable(VerificationError {
                description: "cargo build failed".into(),
                file: Some(path),
                details,
                line: None,
            })
        }
        ExitState::SpawnFailed(_) => Verdict::Skipped("cargo not available".into()),
    }
}

// ── BusVerifier impl (WO 47.14) ─────────────────────────────────────────
//
// The sync `BusVerifier` path replaces the async `Verifier` trait. The logic
// is identical to `verify_build` above but reads from `VerifyContext` instead
// of `BusEvent` and returns `Vec<VerdictEntry>` instead of `Verdict`. The
// translation:
//   Verdict::Clean           → vec![]
//   Verdict::Fixable(fix)    → vec![VerdictEntry { severity: Warning, fix: Some(fix), .. }]
//   Verdict::Unfixable(err)  → vec![VerdictEntry { severity: Error, message: err.details, .. }]
//   Verdict::Skipped(reason) → vec![VerdictEntry { severity: Info, message: reason, .. }]

/// Build verifier registered on the `VerifierBus`. WO 47.14.
pub struct BuildVerifier {
    runner: std::sync::Arc<dyn CommandRunner>,
}

impl BuildVerifier {
    pub fn new(runner: std::sync::Arc<dyn CommandRunner>) -> Self {
        Self { runner }
    }
}

impl BusVerifier for BuildVerifier {
    fn name(&self) -> &str {
        "build"
    }

    fn verify(&self, ctx: &VerifyContext) -> Vec<VerdictEntry> {
        let Some(path) = ctx.changed_files.first() else {
            return vec![];
        };
        if path.extension().and_then(|e| e.to_str()) != Some("rs") {
            return vec![];
        }
        let Some(cargo_root) = find_cargo_root(path) else {
            return vec![];
        };
        let outcome = self
            .runner
            .run("cargo", &["build", "--message-format=json"], &cargo_root);
        use crate::session::verifier::types::ExitState;
        match outcome.status {
            ExitState::SpawnFailed(_) => return vec![],
            ExitState::Success | ExitState::Code(_) => {}
        }
        let stdout = String::from_utf8_lossy(&outcome.stdout);
        for line in stdout.lines() {
            if let Some(suggestion) = parse_build_json(line, path, &cargo_root) {
                return vec![VerdictEntry {
                    source: VerifierSource::Build,
                    severity: Severity::Warning,
                    message: suggestion.description.clone(),
                    file: Some(suggestion.file.clone()),
                    line: suggestion.line,
                    fix: Some(suggestion),
                }];
            }
        }
        match outcome.status {
            ExitState::Success => vec![],
            ExitState::Code(_) => {
                let stderr = String::from_utf8_lossy(&outcome.stderr);
                let stderr_summary = stderr.lines().take(5).collect::<Vec<_>>().join("\n");
                let mut details = stderr_summary;
                if let Some(hint) = error_recovery::classify_error(&stderr) {
                    details.push('\n');
                    details.push_str(&error_recovery::render_hint(&hint));
                }
                vec![VerdictEntry {
                    source: VerifierSource::Build,
                    severity: Severity::Error,
                    message: details,
                    file: Some(path.clone()),
                    line: None,
                    fix: None,
                }]
            }
            ExitState::SpawnFailed(_) => vec![],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::verifier::types::{CommandOutcome, ExitState, SystemCommandRunner};
    use std::path::PathBuf;
    use std::sync::Mutex;

    // Hand-rolled fake runner: returns a canned `CommandOutcome` so the full
    // event → cargo_root → spawn → parse → Verdict path runs in-process
    // without a real `cargo build`. WO 33.14 phase 3: no mock framework.
    struct FakeRunner {
        stdout: Vec<u8>,
        stderr: Vec<u8>,
        code: i32,
        // Tracks the (cmd, args, cwd) the verifier passed so the test can
        // assert the right command was built. Mutex because `CommandRunner`
        // is `Send + Sync` and the test reads it after `verify_build` returns.
        seen: Mutex<Vec<(String, Vec<String>, PathBuf)>>,
    }

    impl FakeRunner {
        fn success(stdout: Vec<u8>) -> Self {
            Self {
                stdout,
                stderr: Vec::new(),
                code: 0,
                seen: Mutex::new(Vec::new()),
            }
        }
        fn failure(stdout: Vec<u8>, stderr: Vec<u8>) -> Self {
            Self {
                stdout,
                stderr,
                code: 101,
                seen: Mutex::new(Vec::new()),
            }
        }
    }

    impl CommandRunner for FakeRunner {
        fn run(&self, cmd: &str, args: &[&str], cwd: &Path) -> CommandOutcome {
            self.seen.lock().unwrap().push((
                cmd.to_string(),
                args.iter().map(|s| s.to_string()).collect(),
                cwd.to_path_buf(),
            ));
            let status = if self.code == 0 {
                ExitState::Success
            } else {
                ExitState::Code(self.code)
            };
            CommandOutcome {
                status,
                stdout: self.stdout.clone(),
                stderr: self.stderr.clone(),
            }
        }
    }

    #[tokio::test]
    async fn test_skips_unknown_file_types() {
        let event = BusEvent::Edit(EditEvent {
            path: std::path::PathBuf::from("readme.md"),
            diff: "".into(),
        });
        let v = verify_build(&event, &SystemCommandRunner).await;
        assert!(matches!(v, Verdict::Skipped(_)));
    }

    #[tokio::test]
    async fn test_skips_non_edit_events() {
        let event = BusEvent::BashExec(crate::session::verifier::types::BashExecEvent {
            command: "echo hi".into(),
            exit_code: 0,
            stdout_len: 3,
            stderr_len: 0,
            workdir: None,
        });
        let v = verify_build(&event, &SystemCommandRunner).await;
        assert!(matches!(v, Verdict::Skipped(_)));
    }

    #[tokio::test]
    async fn test_parse_build_json_extracts_error() {
        let line = r#"{"reason":"compiler-message","package_id":"foo 0.1.0","target":{"kind":["bin"],"name":"foo","src_path":"/tmp/foo/src/main.rs"},"message":{"rendered":"error: cannot find value `x` in this scope\n  --> src/main.rs:3:9\n   |\n3 |     let y = x;\n   |         ^\n   |\n   = note: ...\n\n","level":"error","message":"cannot find value `x` in this scope","spans":[{"file_name":"src/main.rs","line_start":3,"line_end":3,"column_start":9,"column_end":10}]}}"#;
        let cargo_root = std::path::PathBuf::from("/tmp/foo");
        let target = std::path::PathBuf::from("/tmp/foo/src/main.rs");
        let suggestion = parse_build_json(line, &target, &cargo_root).unwrap();
        assert_eq!(suggestion.file, target);
        assert!(suggestion.description.contains("cannot find value `x`"));
        assert!(suggestion.description.contains("src/main.rs:3"));
        assert_eq!(suggestion.severity, "error");
        assert!(suggestion.original.is_empty());
        assert!(suggestion.replacement.is_empty());
        assert!(suggestion.command.is_none());
    }

    #[tokio::test]
    async fn test_parse_build_json_attaches_hint_for_missing_import() {
        let line = r#"{"reason":"compiler-message","package_id":"foo 0.1.0","target":{"kind":["bin"],"name":"foo","src_path":"/tmp/foo/src/main.rs"},"message":{"rendered":"error: cannot find value `frobnicate` in this scope\n  --> src/main.rs:1:1\n\n","level":"error","message":"cannot find value `frobnicate` in this scope","spans":[{"file_name":"src/main.rs","line_start":1,"line_end":1,"column_start":1,"column_end":10}]}}"#;
        let cargo_root = std::path::PathBuf::from("/tmp/foo");
        let target = std::path::PathBuf::from("/tmp/foo/src/main.rs");
        let suggestion = parse_build_json(line, &target, &cargo_root).unwrap();
        assert!(
            suggestion.description.contains("Hint:"),
            "expected Hint line in description, got: {}",
            suggestion.description,
        );
        assert!(suggestion.description.contains("`frobnicate`"));
    }

    #[tokio::test]
    async fn test_parse_build_json_no_hint_for_unclassified_message() {
        // Generic text that doesn't match any classifier — the description
        // should be the raw error and nothing else.
        let line = r#"{"reason":"compiler-message","package_id":"foo 0.1.0","target":{"kind":["bin"],"name":"foo","src_path":"/tmp/foo/src/main.rs"},"message":{"rendered":"error: something custom\n","level":"error","message":"something custom happened","spans":[{"file_name":"src/main.rs","line_start":1,"line_end":1,"column_start":1,"column_end":2}]}}"#;
        let cargo_root = std::path::PathBuf::from("/tmp/foo");
        let target = std::path::PathBuf::from("/tmp/foo/src/main.rs");
        let suggestion = parse_build_json(line, &target, &cargo_root).unwrap();
        assert!(!suggestion.description.contains("Hint:"));
    }

    // WO 33.14 phase 3: was `#[ignore = "spawns cargo"]` — now uses a
    // `FakeRunner` that returns canned cargo JSON. Exercises the full
    // event → cargo_root → spawn → parse → Verdict path in-process.
    #[tokio::test]
    async fn test_build_error_via_fake_runner() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname = \"fake-build\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .unwrap();
        let path = dir.path().join("src/main.rs");
        std::fs::write(&path, "fn main() {\n    let y = x;\n}\n").unwrap();

        // Canned cargo JSON: one compiler-message error for src/main.rs:3.
        let json = r#"{"reason":"compiler-message","package_id":"fake-build 0.1.0","target":{"kind":["bin"],"name":"fake-build","src_path":"src/main.rs"},"message":{"rendered":"error: cannot find value `x` in this scope\n  --> src/main.rs:3:9\n","level":"error","message":"cannot find value `x` in this scope","spans":[{"file_name":"src/main.rs","line_start":3,"line_end":3,"column_start":9,"column_end":10}]}}"#;
        let runner = FakeRunner::failure(json.as_bytes().to_vec(), b"error".to_vec());

        let event = BusEvent::FileWrite(FileWriteEvent {
            path: path.clone(),
            content_length: 26,
            content_hash: 0,
        });
        let v = verify_build(&event, &runner).await;
        match v {
            Verdict::Fixable(suggestion) => {
                assert_eq!(suggestion.file, path);
                assert!(suggestion.description.contains("cannot find value `x`"));
                assert_eq!(suggestion.severity, "error");
                assert!(suggestion.original.is_empty());
                assert!(suggestion.replacement.is_empty());
                assert!(suggestion.command.is_none());
            }
            other => panic!("expected Fixable build error, got {other:?}"),
        }

        // The runner was called with `cargo build --message-format=json` in
        // the temp project root.
        let seen = runner.seen.lock().unwrap();
        assert_eq!(seen.len(), 1, "runner should be called exactly once");
        assert_eq!(seen[0].0, "cargo");
        assert_eq!(seen[0].1, vec!["build", "--message-format=json"]);
    }

    // WO 33.14 phase 3: clean build via fake runner returns Verdict::Clean.
    #[tokio::test]
    async fn test_build_clean_via_fake_runner() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname = \"fake-clean\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .unwrap();
        let path = dir.path().join("src/main.rs");
        std::fs::write(&path, "fn main() {}\n").unwrap();

        // No compiler messages, exit 0 → Clean.
        let runner = FakeRunner::success(Vec::new());
        let event = BusEvent::FileWrite(FileWriteEvent {
            path: path.clone(),
            content_length: 14,
            content_hash: 0,
        });
        let v = verify_build(&event, &runner).await;
        assert!(matches!(v, Verdict::Clean), "expected Clean, got {v:?}");
    }

    // WO 33.14 phase 3: a build that fails with no parseable JSON message
    // falls through to Unfixable with the stderr summary.
    #[tokio::test]
    async fn test_build_unfixable_via_fake_runner_when_no_json_message() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname = \"fake-unfixable\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .unwrap();
        let path = dir.path().join("src/main.rs");
        std::fs::write(&path, "fn main() {}\n").unwrap();

        let runner = FakeRunner::failure(Vec::new(), b"cargo: some raw stderr\n".to_vec());
        let event = BusEvent::FileWrite(FileWriteEvent {
            path: path.clone(),
            content_length: 14,
            content_hash: 0,
        });
        let v = verify_build(&event, &runner).await;
        match v {
            Verdict::Unfixable(err) => {
                assert_eq!(err.description, "cargo build failed");
                assert!(err.details.contains("raw stderr"));
            }
            other => panic!("expected Unfixable, got {other:?}"),
        }
    }

    // WO 33.14 phase 3: when the runner reports a spawn failure (cargo
    // missing), the verifier skips rather than panicking.
    #[tokio::test]
    async fn test_build_skips_when_cargo_unavailable() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname = \"fake-nocargo\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .unwrap();
        let path = dir.path().join("src/main.rs");
        std::fs::write(&path, "fn main() {}\n").unwrap();

        struct NoCargoRunner;
        impl CommandRunner for NoCargoRunner {
            fn run(&self, _cmd: &str, _args: &[&str], _cwd: &Path) -> CommandOutcome {
                CommandOutcome {
                    status: ExitState::SpawnFailed("no such file: cargo".into()),
                    stdout: Vec::new(),
                    stderr: Vec::new(),
                }
            }
        }
        let event = BusEvent::FileWrite(FileWriteEvent {
            path: path.clone(),
            content_length: 14,
            content_hash: 0,
        });
        let v = verify_build(&event, &NoCargoRunner).await;
        match v {
            Verdict::Skipped(msg) => assert!(msg.contains("cargo not available")),
            other => panic!("expected Skipped, got {other:?}"),
        }
    }

    // Integration test: actually invokes `cargo build` in a temp project.
    // Gated behind `#[ignore]` because the Cargo package-cache lock
    // serializes all cargo processes — run with `cargo test -- --ignored`
    // or the integration nextest profile. This is the 1 real-Cargo test
    // for the build verifier (WO 33.14 phase 3).
    #[tokio::test]
    #[ignore = "integration: spawns real cargo build; run with cargo test -- --ignored"]
    async fn test_build_error_on_temp_project_real_cargo() {
        let dir = std::env::temp_dir().join("kf_code_build_test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(
            dir.join("Cargo.toml"),
            r#"[package]
name = "kf-code-build-test"
version = "0.1.0"
edition = "2021"
"#,
        )
        .unwrap();
        std::fs::write(dir.join("src/main.rs"), "fn main() {\n    let y = x;\n}\n").unwrap();

        let path = dir.join("src/main.rs");
        let event = BusEvent::FileWrite(FileWriteEvent {
            path: path.clone(),
            content_length: 26,
            content_hash: 0,
        });
        let v = verify_build(&event, &SystemCommandRunner).await;
        match v {
            Verdict::Fixable(suggestion) => {
                assert_eq!(suggestion.file, path);
                assert!(suggestion.description.contains("cannot find value `x`"));
                assert_eq!(suggestion.severity, "error");
                assert!(suggestion.original.is_empty());
                assert!(suggestion.replacement.is_empty());
                assert!(suggestion.command.is_none());
            }
            other => panic!("expected Fixable build error, got {other:?}"),
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn parse_build_json_returns_none_for_non_compiler_message() {
        let line = r#"{"reason":"compiler-artifact","package_id":"foo"}"#;
        let result = parse_build_json(
            line,
            std::path::Path::new("x.rs"),
            std::path::Path::new("."),
        );
        assert!(result.is_none());
    }

    #[test]
    fn parse_build_json_returns_none_for_non_error_level() {
        let line = r#"{"reason":"compiler-message","message":{"level":"warning","message":"w","spans":[{"file_name":"src/main.rs","line_start":1}]}}"#;
        let result = parse_build_json(
            line,
            std::path::Path::new("src/main.rs"),
            std::path::Path::new("."),
        );
        assert!(result.is_none(), "build verifier only fires on errors");
    }

    #[test]
    fn parse_build_json_returns_none_when_spans_do_not_match_target() {
        let line = r#"{"reason":"compiler-message","message":{"level":"error","message":"e","spans":[{"file_name":"src/other.rs","line_start":1}]}}"#;
        let cargo_root = std::path::PathBuf::from("/tmp/foo");
        let target = std::path::PathBuf::from("/tmp/foo/src/main.rs");
        let result = parse_build_json(line, &target, &cargo_root);
        assert!(result.is_none(), "mismatching span should yield None");
    }

    #[test]
    fn parse_build_json_returns_none_for_invalid_json() {
        let result = parse_build_json(
            "not json",
            std::path::Path::new("x.rs"),
            std::path::Path::new("."),
        );
        assert!(result.is_none());
    }
}
