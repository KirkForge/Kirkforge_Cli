use crate::shared::access::{GuardVerdict, PathGuard};
use crate::shared::{ToolDef, ToolError, ToolOutcome};
use crate::tools::{Tool, ToolContext};
use globset::{Glob as GlobPattern, GlobSet, GlobSetBuilder};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

pub struct Glob {
    path_guard: PathGuard,
}

impl Glob {
    pub fn new(path_guard: PathGuard) -> Self {
        Self { path_guard }
    }
}

// WO 48.34: the walk body, extracted so the cancel contract is testable
// without racing a real token. `cancel` is checked per directory entry —
// when the tool's select arm loses to `ctx.token.cancelled()` it flips
// the flag, and the blocking-pool thread stops at the next entry instead
// of walking the whole tree to completion.
fn walk_glob_matches(
    walk_base: &Path,
    glob_set: &GlobSet,
    max_matches: usize,
    path_guard: &PathGuard,
    cancel: &AtomicBool,
) -> (Vec<String>, bool) {
    let mut out = Vec::new();
    let mut capped = false;
    let walker = ignore::WalkBuilder::new(walk_base)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .build();

    for entry in walker.flatten() {
        if cancel.load(std::sync::atomic::Ordering::Relaxed) {
            break;
        }
        let entry_path = entry.path();
        if entry_path.is_dir() {
            continue;
        }

        // Per-file traversal guard: the walker may have followed a
        // symlink from inside the base_dir to outside it, or the file
        // may sit on a denied path. `check_traversal` is the
        // lightweight deny-list + symlink + sandbox check (no
        // size/binary gate, because we are only listing paths).
        if let GuardVerdict::Denied(_) = path_guard.check_traversal(entry_path) {
            continue;
        }

        // Relative path matching
        let rel = entry_path.strip_prefix(walk_base).unwrap_or(entry_path);
        if glob_set.is_match(rel) {
            out.push(rel.to_string_lossy().to_string());
            // WO 47.24: stop the walk once the cap is collected —
            // buffering every match and truncating afterward walks
            // (and buffers) the whole tree for {"max_matches":1}.
            // Dropping the walker here ends the traversal.
            if out.len() >= max_matches {
                capped = true;
                break;
            }
        }
    }
    (out, capped)
}

#[async_trait::async_trait]
impl Tool for Glob {
    fn def(&self) -> ToolDef {
        ToolDef {
            name: "glob",
            description: "List files matching a glob pattern. Uses gitignore-aware matching.",
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "pattern": {
                        "type": "string",
                        "description": "Glob pattern (e.g., 'src/**/*.rs', '*.toml')"
                    },
                    "base_dir": {
                        "type": "string",
                        "description": "Base directory (default: current directory)",
                        "default": "."
                    },
                    "max_matches": {
                        "type": "integer",
                        "description": "Maximum number of files to return (default: 1000)",
                        "default": 1000
                    }
                },
                "required": ["pattern"]
            }),
        }
    }

    async fn run(&self, ctx: &ToolContext, args: serde_json::Value) -> ToolOutcome {
        let pattern = match args.get("pattern").and_then(|p| p.as_str()) {
            Some(p) => p.to_string(),
            None => {
                return ToolOutcome::Failure(ToolError::invalid_args("Missing 'pattern' argument"));
            }
        };

        let base_dir = args.get("base_dir").and_then(|b| b.as_str()).unwrap_or(".");
        let max_matches = args
            .get("max_matches")
            .and_then(|m| m.as_u64())
            .unwrap_or(1000) as usize;

        let base_path = PathBuf::from(shellexpand::tilde(base_dir).as_ref());

        if !base_path.is_dir() {
            return ToolOutcome::Failure(ToolError::Internal {
                message: format!("Base directory not found: {}", base_path.display()),
            });
        }

        // WO 47.24: guard the base dir BEFORE spawning the walker — otherwise
        // base_dir="/" walks the filesystem broadly and filters per-entry
        // afterward, leaving the traversal cost unguarded. The per-entry
        // check below stays for symlink races during the walk.
        if let GuardVerdict::Denied(msg) = self.path_guard.check_traversal(&base_path) {
            return ToolOutcome::Failure(ToolError::AccessDenied { message: msg });
        }

        // Build glob set
        let mut builder = GlobSetBuilder::new();
        match GlobPattern::new(&pattern) {
            Ok(g) => {
                builder.add(g);
            }
            Err(e) => {
                return ToolOutcome::Failure(ToolError::invalid_args(format!(
                    "Invalid glob pattern '{pattern}': {e}"
                )));
            }
        }
        let glob_set = builder.build().unwrap_or_else(|_| GlobSet::empty());

        // Walk the filesystem on a blocking thread — `ignore::WalkBuilder` is
        // synchronous `std::fs` traversal and would stall the tokio worker
        // for a large `base_dir` (e.g. model-supplied `base_dir="/"`).
        // `path_guard` is `Clone`; clone it into the `'static` closure.
        // `base_path` is cloned so the original remains for the result header.
        //
        // WO 46.8: race the blocking walk against `ctx.token.cancelled()` so
        // a user/turn cancel returns promptly with `Cancelled`. WO 48.34: the
        // cancel arm now also flips `cancel_flag`, which the walker checks
        // per entry — the blocking-pool thread stops at the next entry
        // instead of walking to completion (previously the residual run was
        // bounded only by the cap or the tree size).
        // Pattern: `plugin_tools/wrapper.rs:324` (`Finish::Cancelled`).
        let path_guard = self.path_guard.clone();
        let walk_base = base_path.clone();
        let cancel_flag = Arc::new(AtomicBool::new(false));
        let walk_cancel = cancel_flag.clone();
        let walk = tokio::task::spawn_blocking(move || {
            walk_glob_matches(
                &walk_base,
                &glob_set,
                max_matches,
                &path_guard,
                &walk_cancel,
            )
        });
        let (mut matches, truncated) = tokio::select! {
            biased;
            _ = ctx.token.cancelled() => {
                cancel_flag.store(true, std::sync::atomic::Ordering::Relaxed);
                return ToolOutcome::Failure(ToolError::Cancelled);
            }
            res = walk => res.unwrap_or_default(),
        };

        matches.sort();
        let total = matches.len();
        matches.truncate(max_matches);

        if matches.is_empty() {
            return ToolOutcome::Success {
                content: format!("No files matching '{}' in {}", pattern, base_path.display()),
            };
        }

        let output = matches.join("\n");
        let header = if truncated {
            // Early stop: the exact total is unknown past the cap.
            format!("Found at least {max_matches} files matching '{pattern}'; showing first {max_matches}:")
        } else {
            format!("Found {total} files matching '{pattern}':")
        };
        ToolOutcome::Success {
            content: format!("{header}\n{output}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::ToolContext;

    // WO 48.34: a set cancel flag must stop the walk before the first
    // match is pushed. The control run (flag unset) over the same dir
    // returns all files, so 0 matches proves the loop checked the flag —
    // a walker that ignored it would return all 5.
    #[test]
    fn walk_glob_matches_cancel_flag_stops_walk() {
        let dir = std::env::temp_dir().join("kf_code_glob_cancel_test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for i in 0..5 {
            std::fs::write(dir.join(format!("file{i}.txt")), "x").unwrap();
        }
        let glob_set = GlobSetBuilder::new()
            .add(GlobPattern::new("*.txt").unwrap())
            .build()
            .unwrap();
        let guard = PathGuard::default();

        let cancel = AtomicBool::new(true);
        let (out, capped) = walk_glob_matches(&dir, &glob_set, 1000, &guard, &cancel);
        assert!(
            out.is_empty() && !capped,
            "pre-set cancel flag must stop the walk with no matches, got {out:?}"
        );

        let cancel = AtomicBool::new(false);
        let (out, _) = walk_glob_matches(&dir, &glob_set, 1000, &guard, &cancel);
        assert_eq!(out.len(), 5, "control run without the flag matches all");

        let _ = std::fs::remove_dir_all(&dir);
    }

    // WO 48.34: run() with an already-cancelled token returns Cancelled
    // and flips the flag the walker observes (biased select → the cancel
    // arm wins before the walk starts).
    #[tokio::test]
    async fn glob_cancelled_token_returns_cancelled() {
        let dir = std::env::temp_dir().join("kf_code_glob_cancel_run_test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.txt"), "x").unwrap();

        let glob = Glob::new(PathGuard::default());
        let ctx = ToolContext::default();
        ctx.token.cancel();
        let args = serde_json::json!({
            "pattern": "*.txt",
            "base_dir": dir.to_string_lossy(),
        });
        let outcome = glob.run(&ctx, args).await;
        assert!(
            matches!(outcome, ToolOutcome::Failure(ToolError::Cancelled)),
            "expected Cancelled, got {outcome:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn glob_stops_at_max_matches_and_reports_at_least() {
        let dir = std::env::temp_dir().join("kf_code_glob_cap_test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for i in 0..5 {
            std::fs::write(dir.join(format!("file{i}.txt")), "x").unwrap();
        }

        let glob = Glob::new(PathGuard::default());
        let args = serde_json::json!({
            "pattern": "*.txt",
            "base_dir": dir.to_string_lossy(),
            "max_matches": 2
        });
        let outcome = glob.run(&ToolContext::default(), args).await;
        match outcome {
            ToolOutcome::Success { content } => {
                // WO 47.24: the walk stops at the cap, so the total is
                // reported as a lower bound, not an exact count.
                assert!(
                    content.contains("Found at least 2 files matching '*.txt'; showing first 2:"),
                    "expected early-stop truncation header, got: {content}"
                );
                // Two filenames should appear, not all five.
                let lines: Vec<_> = content.lines().skip(1).collect();
                assert_eq!(
                    lines.len(),
                    2,
                    "output should contain exactly max_matches files"
                );
            }
            other => panic!("expected Success, got {other:?}"),
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn glob_base_dir_denied_by_guard_is_access_denied() {
        let sandbox = std::env::temp_dir().join("kf_code_glob_sandbox_test");
        let outside = std::env::temp_dir().join("kf_code_glob_outside_test");
        let _ = std::fs::remove_dir_all(&sandbox);
        let _ = std::fs::remove_dir_all(&outside);
        std::fs::create_dir_all(&sandbox).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("a.txt"), "x").unwrap();

        // WO 47.24: the base dir is guarded BEFORE the walker spawns, so a
        // base_dir outside the sandbox fails without any traversal.
        let guard = PathGuard {
            sandbox_dir: Some(sandbox.clone()),
            ..PathGuard::default()
        };
        let glob = Glob::new(guard);
        let args = serde_json::json!({
            "pattern": "*.txt",
            "base_dir": outside.to_string_lossy(),
        });
        let outcome = glob.run(&ToolContext::default(), args).await;
        assert!(
            matches!(outcome, ToolOutcome::Failure(ToolError::AccessDenied { ref message }) if message.contains("outside sandbox")),
            "expected AccessDenied for base_dir outside sandbox, got {outcome:?}"
        );

        let _ = std::fs::remove_dir_all(&sandbox);
        let _ = std::fs::remove_dir_all(&outside);
    }

    #[tokio::test]
    async fn glob_no_truncation_when_under_cap() {
        let dir = std::env::temp_dir().join("kf_code_glob_under_cap_test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.txt"), "x").unwrap();

        let glob = Glob::new(PathGuard::default());
        let args = serde_json::json!({
            "pattern": "*.txt",
            "base_dir": dir.to_string_lossy(),
            "max_matches": 10
        });
        let outcome = glob.run(&ToolContext::default(), args).await;
        match outcome {
            ToolOutcome::Success { content } => {
                assert!(
                    content.contains("Found 1 files matching '*.txt':"),
                    "expected non-truncation header, got: {content}"
                );
                assert!(!content.contains("showing first"));
            }
            other => panic!("expected Success, got {other:?}"),
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn glob_missing_pattern_arg_is_invalid_args() {
        let glob = Glob::new(PathGuard::default());
        let outcome = glob
            .run(&ToolContext::default(), serde_json::json!({}))
            .await;
        assert!(
            matches!(outcome, ToolOutcome::Failure(ToolError::InvalidArgs { .. })),
            "expected InvalidArgs, got {outcome:?}"
        );
    }

    #[tokio::test]
    async fn glob_invalid_pattern_is_invalid_args() {
        let dir = std::env::temp_dir().join("kf_code_glob_invalid_pattern_test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let glob = Glob::new(PathGuard::default());
        let args = serde_json::json!({
            "pattern": "[unclosed",
            "base_dir": dir.to_string_lossy(),
        });
        let outcome = glob.run(&ToolContext::default(), args).await;
        assert!(
            matches!(outcome, ToolOutcome::Failure(ToolError::InvalidArgs { ref message }) if message.contains("Invalid glob pattern")),
            "expected InvalidArgs for bad pattern, got {outcome:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn glob_nonexistent_base_dir_is_internal_error() {
        let glob = Glob::new(PathGuard::default());
        let args = serde_json::json!({
            "pattern": "*.txt",
            "base_dir": "/nonexistent/kf-code/does/not/exist",
        });
        let outcome = glob.run(&ToolContext::default(), args).await;
        assert!(
            matches!(outcome, ToolOutcome::Failure(ToolError::Internal { ref message }) if message.contains("Base directory not found")),
            "expected Internal error for missing base dir, got {outcome:?}"
        );
    }

    #[tokio::test]
    async fn glob_no_matches_returns_empty_message() {
        let dir = std::env::temp_dir().join("kf_code_glob_empty_test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.txt"), "x").unwrap();

        let glob = Glob::new(PathGuard::default());
        let args = serde_json::json!({
            "pattern": "*.nonexistent",
            "base_dir": dir.to_string_lossy(),
        });
        let outcome = glob.run(&ToolContext::default(), args).await;
        match outcome {
            ToolOutcome::Success { content } => {
                assert!(
                    content.contains("No files matching") && content.contains("*.nonexistent"),
                    "expected no-match message, got: {content}"
                );
            }
            other => panic!("expected Success with empty msg, got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn glob_default_max_matches_is_1000() {
        let dir = std::env::temp_dir().join("kf_code_glob_default_cap_test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.txt"), "x").unwrap();

        let glob = Glob::new(PathGuard::default());
        let args = serde_json::json!({
            "pattern": "*.txt",
            "base_dir": dir.to_string_lossy(),
        });
        let outcome = glob.run(&ToolContext::default(), args).await;
        assert!(
            matches!(outcome, ToolOutcome::Success { .. }),
            "got {outcome:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn glob_matches_recursive_pattern() {
        let dir = std::env::temp_dir().join("kf_code_glob_recursive_test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("a.txt"), "x").unwrap();
        std::fs::write(dir.join("sub").join("b.txt"), "x").unwrap();

        let glob = Glob::new(PathGuard::default());
        let args = serde_json::json!({
            "pattern": "**/*.txt",
            "base_dir": dir.to_string_lossy(),
        });
        let outcome = glob.run(&ToolContext::default(), args).await;
        match outcome {
            ToolOutcome::Success { content } => {
                assert!(content.contains("a.txt"), "expected a.txt in: {content}");
                assert!(content.contains("b.txt"), "expected b.txt in: {content}");
                assert!(content.contains("sub"), "expected sub path in: {content}");
            }
            other => panic!("expected Success, got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn glob_results_are_sorted() {
        let dir = std::env::temp_dir().join("kf_code_glob_sort_test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("zebra.txt"), "x").unwrap();
        std::fs::write(dir.join("apple.txt"), "x").unwrap();
        std::fs::write(dir.join("mango.txt"), "x").unwrap();

        let glob = Glob::new(PathGuard::default());
        let args = serde_json::json!({
            "pattern": "*.txt",
            "base_dir": dir.to_string_lossy(),
        });
        let outcome = glob.run(&ToolContext::default(), args).await;
        match outcome {
            ToolOutcome::Success { content } => {
                let lines: Vec<&str> = content.lines().skip(1).collect();
                assert_eq!(lines.len(), 3, "expected 3 lines, got {lines:?}");
                assert_eq!(lines[0], "apple.txt");
                assert_eq!(lines[1], "mango.txt");
                assert_eq!(lines[2], "zebra.txt");
            }
            other => panic!("expected Success, got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn glob_skips_directories_in_results() {
        let dir = std::env::temp_dir().join("kf_code_glob_skip_dirs_test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("a.txt"), "x").unwrap();

        let glob = Glob::new(PathGuard::default());
        let args = serde_json::json!({
            "pattern": "*",
            "base_dir": dir.to_string_lossy(),
        });
        let outcome = glob.run(&ToolContext::default(), args).await;
        match outcome {
            ToolOutcome::Success { content } => {
                let lines: Vec<&str> = content.lines().skip(1).collect();
                assert!(
                    !lines
                        .iter()
                        .any(|l| *l == "sub" || l.contains("/sub") || l.ends_with("sub")),
                    "directories should not appear, got: {lines:?}"
                );
                assert!(
                    lines.iter().any(|l| l.contains("a.txt")),
                    "a.txt should be present: {lines:?}"
                );
            }
            other => panic!("expected Success, got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn glob_default_base_dir_is_cwd() {
        let dir = std::env::temp_dir().join("kf_code_glob_default_base_test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.txt"), "x").unwrap();

        let _cwd = crate::shared::test_util::CwdGuard::set(&dir).await;
        let glob = Glob::new(PathGuard::default());
        let args = serde_json::json!({ "pattern": "*.txt" });
        let outcome = glob.run(&ToolContext::default(), args).await;
        assert!(
            matches!(outcome, ToolOutcome::Success { .. }),
            "got {outcome:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn glob_def_has_correct_name_and_required_pattern() {
        let glob = Glob::new(PathGuard::default());
        let def = glob.def();
        assert_eq!(def.name, "glob");
        let required = def
            .parameters
            .get("required")
            .and_then(|r| r.as_array())
            .expect("required array");
        assert!(required.iter().any(|v| v.as_str() == Some("pattern")));
    }
}
