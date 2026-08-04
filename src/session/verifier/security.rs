use crate::session::event_bus::{BusEvent, EditEvent, FileWriteEvent};
/// Security verifier — scans file writes and edits for dangerous patterns.
///
/// Checks written/edited files for:
/// - Hardcoded API keys / secrets (substring matching)
/// - High-entropy tokens that look like random API keys
/// - Dangerous shell commands in scripts
/// - Path traversal vulnerabilities
///
/// Optionally, if `trufflehog` is installed on `PATH` (or at a known fallback
/// location), the verifier also runs `trufflehog filesystem --no-update --json <path>`
/// as a second opinion.
use crate::session::verifier::{Verdict, VerificationError};
use std::path::{Path, PathBuf};

/// Minimum length of a candidate high-entropy token after its prefix.
const MIN_TOKEN_LEN: usize = 16;

/// Shannon-entropy threshold (bits per character). Genuine random tokens
/// are well above this; repeated-character placeholders fall well below.
const ENTROPY_THRESHOLD: f64 = 3.5;

/// Known secret patterns (substring-based). These are cheap fast-path
/// checks for obvious secrets where entropy alone would not be enough
/// (e.g. PEM headers, connection strings). Prefix-style tokens such as
/// `sk-`, `ghp_`, or `AKIA` are handled by the high-entropy detector so
/// low-entropy placeholders are not false positives.
const SECRET_PATTERNS: &[(&str, &str)] = &[
    ("Private key PEM", "-----BEGIN PRIVATE KEY-----"),
    ("Private key RSA", "-----BEGIN RSA PRIVATE KEY-----"),
    ("MongoDB+srv connection string", "mongodb+srv://"),
    ("MongoDB connection string", "mongodb://"),
];

/// Secret prefixes that are followed by a high-entropy value. Used after the
/// fast-path substring scan as a more precise detector for random tokens.
///
/// These intentionally overlap with the fast-path list used by the pre-commit
/// `git_sanitation.rs` scanner so both passes agree on the most common secret
/// prefixes (`sk-`, `ghp_`, `github_pat_`, `glpat-`, `AKIA`).
///
/// `ceiling:` this list is supplementary — the primary detectors are the
/// substring scan above, the entropy+length gate (`MIN_TOKEN_LEN` +
/// `ENTROPY_THRESHOLD`), and the external `trufflehog` pass. Two prefixes
/// are intentionally NOT listed: `claude-` (would false-positive on
/// legitimate model-name references like `claude-3-opus-20240229`, whose
/// 18-char tail clears the entropy gate) and `key-` (a generic English
/// fragment whose 16+ char high-entropy tail is too often benign config).
/// Anthropic keys (`sk-ant-...`) are already caught by the `sk-` entry.
const ENTROPY_PREFIXES: &[(&str, &str)] = &[
    ("OpenAI API key", "sk-"),
    ("xAI API key", "xai-"),
    ("HuggingFace access token", "hf_"),
    ("GitHub personal-access token", "ghp_"),
    ("GitHub fine-grained PAT", "github_pat_"),
    ("GitHub OAuth token", "gho_"),
    ("GitHub user-to-server token", "ghu_"),
    ("GitHub server-to-server token", "ghs_"),
    ("GitHub refresh token", "ghr_"),
    ("GitLab personal-access token", "glpat-"),
    ("AWS access key", "AKIA"),
];

/// Dangerous shell patterns.
const DANGEROUS_SHELL_PATTERNS: &[&str] = &[
    "rm -rf /",
    ":(){ :|:& };:",
    "> /dev/sda",
    "mkfs.",
    "dd if=/dev/zero of=",
    "chmod -R 777 /",
];

/// Characters considered part of a token after a known secret prefix.
#[inline]
fn is_token_char(c: char) -> bool {
    c.is_alphanumeric() || c == '-' || c == '_' || c == '+' || c == '/' || c == '=' || c == '.'
}

/// True if a line is a comment whose content should be skipped by the
/// dangerous-shell-pattern check. WO 15.10 (bucketlist 2.9): patterns
/// like `rm -rf /` substring-matched in ALL file content, including
/// comments and docstrings, returning `Verdict::Unfixable` and blocking
/// the correction loop for documentation that documents the dangerous
/// command. This is a simple line-prefix filter (not a full parser):
/// a line is treated as a comment if, after trimming leading
/// whitespace, it starts with `//`, `#`, `/*`, or `*` (the common
/// comment markers across the languages this repo scans).
#[inline]
fn is_comment_line(line: &str) -> bool {
    let trimmed = line.trim_start();
    trimmed.starts_with("//")
        || trimmed.starts_with('#')
        || trimmed.starts_with("/*")
        || trimmed.starts_with('*')
}

/// Shannon entropy in bits per character for the ASCII string `s`.
fn shannon_entropy(s: &str) -> f64 {
    let len = s.len() as f64;
    if len == 0.0 {
        return 0.0;
    }
    let mut counts = [0u64; 256];
    for b in s.bytes() {
        counts[b as usize] += 1;
    }
    counts
        .iter()
        .filter(|&&c| c > 0)
        .map(|&c| {
            let p = c as f64 / len;
            -p * p.log2()
        })
        .sum()
}

/// Extract the token immediately following `prefix` inside `content` and,
/// if it is long and high-entropy, return an `Unfixable` verdict.
fn scan_entropy_prefix(
    content: &str,
    prefix: &str,
    name: &str,
    path: &std::path::Path,
) -> Option<Verdict> {
    for (idx, matched) in content.match_indices(prefix) {
        let start = idx + matched.len();
        let rest = &content[start..];
        let end = rest.find(|c: char| !is_token_char(c)).unwrap_or(rest.len());
        let token = &rest[..end];
        if token.len() >= MIN_TOKEN_LEN && shannon_entropy(token) > ENTROPY_THRESHOLD {
            return Some(Verdict::Unfixable(VerificationError {
                description: format!("High-entropy {name} detected"),
                file: Some(path.to_path_buf()),
                details: format!(
                    "A value following the '{prefix}' prefix in {} looks like a random secret (entropy {:.2} bits/char).",
                    path.display(),
                    shannon_entropy(token)
                ),
            }));
        }
    }
    None
}

/// Scan the file content for high-entropy secret-like tokens.
fn entropy_scan(content: &str, path: &std::path::Path) -> Option<Verdict> {
    for (name, prefix) in ENTROPY_PREFIXES {
        if let Some(verdict) = scan_entropy_prefix(content, prefix, name, path) {
            return Some(verdict);
        }
    }
    None
}

/// Find the `trufflehog` executable.
///
/// Searches `PATH` first, then falls back to the two common installation
/// locations. Returns `None` if no binary is found.
fn trufflehog_path() -> Option<PathBuf> {
    find_in_path("trufflehog")
        .or_else(|| probe_path("/usr/local/bin/trufflehog"))
        .or_else(|| probe_path("/usr/bin/trufflehog"))
}

/// Probe a single absolute path for an executable `trufflehog` binary.
fn probe_path(p: &str) -> Option<PathBuf> {
    let pb = PathBuf::from(p);
    if pb.is_file() {
        Some(pb)
    } else {
        None
    }
}

/// Search `PATH` for an executable named `name`.
///
/// On Windows it also tries `name.exe`. This avoids a shell dependency so the
/// search works on Unix, macOS, and Windows without extra crates.
fn find_in_path(name: &str) -> Option<PathBuf> {
    let path_env = std::env::var_os("PATH")?;
    let sep = if cfg!(windows) { ';' } else { ':' };
    #[cfg(windows)]
    let exe_name = format!("{name}.exe");
    for dir in path_env.to_str()?.split(sep) {
        let candidate = PathBuf::from(dir).join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
        #[cfg(windows)]
        {
            let candidate_exe = PathBuf::from(dir).join(&exe_name);
            if candidate_exe.is_file() {
                return Some(candidate_exe);
            }
        }
    }
    None
}

/// Run `trufflehog filesystem --no-update --json <path>` if a `trufflehog`
/// binary is available. Any JSON output line is treated as a finding and
/// produces an `Unfixable` verdict.
///
/// WO 15.10: the spawn is wrapped in `tokio::time::timeout` so a hung
/// trufflehog (network stall, git fetch hang) cannot block the correction
/// loop indefinitely. On timeout the verifier returns `None` (no finding)
/// rather than hanging — a missed finding is better than a deadlocked
/// correction loop. The timeout is generous (60s) because trufflehog's
/// filesystem scan is local and fast when healthy. The test override
/// shrinks the cap so the timeout test runs in seconds, not minutes
/// (same `#[cfg(test)]` pattern `undo.rs` uses for its size cap).
#[cfg(not(test))]
const TRUFFLEHOG_TIMEOUT_SECS: u64 = 60;
#[cfg(test)]
const TRUFFLEHOG_TIMEOUT_SECS: u64 = 2;

async fn trufflehog_scan(path: &Path) -> Option<Verdict> {
    let binary = match trufflehog_path() {
        Some(b) => b,
        None => return None,
    };
    let output = match tokio::time::timeout(
        std::time::Duration::from_secs(TRUFFLEHOG_TIMEOUT_SECS),
        tokio::process::Command::new(&binary)
            .arg("filesystem")
            .arg("--no-update")
            .arg("--json")
            .arg(path)
            .output(),
    )
    .await
    {
        Ok(Ok(o)) => o,
        // Spawn failed — trufflehog missing/unavailable. Not a finding.
        Ok(Err(_)) => return None,
        // Timed out: do not block the correction loop. A hung trufflehog
        // is an environment issue, not a security finding.
        Err(_) => {
            tracing::warn!(
                path = %path.display(),
                timeout_secs = TRUFFLEHOG_TIMEOUT_SECS,
                "trufflehog_scan timed out; skipping (no finding)"
            );
            return None;
        }
    };
    let stdout = String::from_utf8_lossy(&output.stdout);
    for line in stdout.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if line.starts_with('{') {
            return Some(Verdict::Unfixable(VerificationError {
                description: "trufflehog detected a potential secret".into(),
                file: Some(path.to_path_buf()),
                details: format!(
                    "trufflehog reported a finding in {}: {line}",
                    path.display()
                ),
            }));
        }
    }
    None
}

/// Run the security verifier against an event.
/// Handles FileWrite (full content scan) and Edit (post-edit content scan).
pub async fn verify_security(event: &BusEvent) -> Verdict {
    let (path, content_length) = match event {
        BusEvent::FileWrite(FileWriteEvent {
            path,
            content_length,
            ..
        }) => (path.clone(), *content_length),
        BusEvent::Edit(EditEvent { path, .. }) => {
            // For edits, re-read the file after the edit to check for secrets/shell
            let meta = match std::fs::metadata(path) {
                Ok(m) => m,
                Err(_) => {
                    return Verdict::Skipped(format!("cannot stat edited file: {}", path.display()))
                }
            };
            (path.clone(), meta.len() as usize)
        }
        _ => return Verdict::Skipped("not a file write or edit event".into()),
    };

    // Only scan if content is reasonable (under 1MB)
    if content_length > 1_000_000 {
        return Verdict::Skipped(format!("file exceeds 1MB scan limit: {}", path.display()));
    }

    // Read the file content
    let content = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(_) => return Verdict::Skipped(format!("cannot read: {}", path.display())),
    };

    // 1. Check for obvious secret patterns (cheap fast path)
    for (name, pattern) in SECRET_PATTERNS {
        if content.contains(pattern) {
            return Verdict::Unfixable(VerificationError {
                description: format!("Potential secret detected: {name}"),
                file: Some(path.clone()),
                details: format!(
                    "Pattern '{}' found in {}. This must be reviewed manually.",
                    pattern,
                    path.display()
                ),
            });
        }
    }

    // 2. High-entropy token detector for random-looking API keys/tokens.
    if let Some(verdict) = entropy_scan(&content, &path) {
        return verdict;
    }

    // 3. Optional second opinion from trufflehog.
    if let Some(verdict) = trufflehog_scan(&path).await {
        return verdict;
    }

    // 4. Check for dangerous shell patterns in any file content.
    // WO 15.10 (bucketlist 2.9): skip comment lines so documentation
    // that mentions `rm -rf /` is not flagged as `Unfixable`. Only the
    // non-comment lines are substring-matched against the patterns.
    for pattern in DANGEROUS_SHELL_PATTERNS {
        let in_code = content
            .lines()
            .any(|line| !is_comment_line(line) && line.contains(pattern));
        if in_code {
            return Verdict::Unfixable(VerificationError {
                description: format!("Dangerous shell command: {pattern}"),
                file: Some(path.clone()),
                details: "This command is blocked by security policy. Remove it to proceed.".into(),
            });
        }
    }

    Verdict::Clean
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shared::test_util::remove_test_file;

    #[tokio::test]
    async fn test_skips_unrelated_events() {
        // Only FileWrite and Edit are scanned; BashExec, FileRead, etc. should skip
        let event = BusEvent::BashExec(crate::session::event_bus::BashExecEvent {
            command: "echo hi".into(),
            exit_code: 0,
            stdout_len: 0,
            stderr_len: 0,
            workdir: None,
        });
        let v = verify_security(&event).await;
        assert!(matches!(v, Verdict::Skipped(_)));
    }

    #[tokio::test]
    async fn test_scans_edit_event() {
        let dir = std::env::temp_dir();
        let path = dir.join("kf_code_sec_edit_check.txt");
        std::fs::write(&path, "let x = 1;").unwrap();

        let event = BusEvent::Edit(EditEvent {
            path: path.clone(),
            diff: "".into(),
        });
        let v = verify_security(&event).await;
        // Clean file written and then edited should still pass
        assert!(matches!(v, Verdict::Clean));
        remove_test_file(&path);
    }

    #[tokio::test]
    async fn test_edit_event_detects_key() {
        let dir = std::env::temp_dir();
        let path = dir.join("kf_code_sec_edit_key.txt");
        // Use a long high-entropy token so the entropy detector catches it.
        std::fs::write(&path, "api_key = \"sk-abcdefghijklmnopqrstuvwxyz012345\"").unwrap();

        let event = BusEvent::Edit(EditEvent {
            path: path.clone(),
            diff: "".into(),
        });
        let v = verify_security(&event).await;
        // Even though it's an Edit event, the file content should be scanned
        assert!(matches!(v, Verdict::Unfixable(_)));
        remove_test_file(&path);
    }

    #[tokio::test]
    async fn test_clean_file_passes() {
        let dir = std::env::temp_dir();
        let path = dir.join("kf_code_sec_clean.txt");
        std::fs::write(&path, "let x = 1;").unwrap();

        let event = BusEvent::FileWrite(FileWriteEvent {
            path: path.clone(),
            content_length: 10,
            content_hash: 0,
        });
        let v = verify_security(&event).await;
        assert!(matches!(v, Verdict::Clean));
        remove_test_file(&path);
    }

    #[tokio::test]
    async fn test_detects_api_key_pattern() {
        let dir = std::env::temp_dir();
        let path = dir.join("kf_code_sec_key.txt");
        // High-entropy token long enough to trip the entropy detector.
        std::fs::write(&path, "api_key = \"sk-abcdefghijklmnopqrstuvwxyz012345\"").unwrap();

        let event = BusEvent::FileWrite(FileWriteEvent {
            path: path.clone(),
            content_length: 50,
            content_hash: 0,
        });
        let v = verify_security(&event).await;
        assert!(matches!(v, Verdict::Unfixable(_)));
        remove_test_file(&path);
    }

    #[tokio::test]
    async fn test_detects_private_key() {
        let dir = std::env::temp_dir();
        let path = dir.join("kf_code_sec_private.pem");
        std::fs::write(
            &path,
            "-----BEGIN PRIVATE KEY-----\nABC123\n-----END PRIVATE KEY-----",
        )
        .unwrap();

        let event = BusEvent::FileWrite(FileWriteEvent {
            path: path.clone(),
            content_length: 80,
            content_hash: 0,
        });
        let v = verify_security(&event).await;
        assert!(matches!(v, Verdict::Unfixable(_)));
        remove_test_file(&path);
    }

    #[tokio::test]
    async fn test_detects_shell_danger() {
        let dir = std::env::temp_dir();
        let path = dir.join("kf_code_sec_danger.sh");
        std::fs::write(&path, "rm -rf /").unwrap();

        let event = BusEvent::FileWrite(FileWriteEvent {
            path: path.clone(),
            content_length: 10,
            content_hash: 0,
        });
        let v = verify_security(&event).await;
        assert!(matches!(v, Verdict::Unfixable(_)));
        remove_test_file(&path);
    }

    #[tokio::test]
    async fn test_detects_shell_danger_in_non_shell_file() {
        let dir = std::env::temp_dir();
        let path = dir.join("kf_code_sec_danger.txt");
        std::fs::write(&path, "rm -rf /").unwrap();

        let event = BusEvent::FileWrite(FileWriteEvent {
            path: path.clone(),
            content_length: 10,
            content_hash: 0,
        });
        let v = verify_security(&event).await;
        assert!(
            matches!(v, Verdict::Unfixable(_)),
            "dangerous shell content should be flagged regardless of extension"
        );
        remove_test_file(&path);
    }

    #[tokio::test]
    async fn test_large_file_is_skipped_not_clean() {
        let dir = std::env::temp_dir();
        let path = dir.join("kf_code_sec_large.txt");
        std::fs::write(&path, "tiny content").unwrap();

        let event = BusEvent::FileWrite(FileWriteEvent {
            path: path.clone(),
            content_length: 1_000_001,
            content_hash: 0,
        });
        let v = verify_security(&event).await;
        assert!(
            matches!(v, Verdict::Skipped(_)),
            "files over 1MB should be skipped, not reported as clean"
        );
        remove_test_file(&path);
    }

    #[tokio::test]
    async fn test_path_traversal_no_false_positive() {
        // `../` inside string content must NOT be flagged (it's a legitimate import)
        let dir = std::env::temp_dir();
        let path = dir.join("kf_code_sec_traversal.txt");
        std::fs::write(&path, "require('../../secret')").unwrap();

        let event = BusEvent::FileWrite(FileWriteEvent {
            path: path.clone(),
            content_length: 30,
            content_hash: 0,
        });
        let v = verify_security(&event).await;
        // Must be Clean (no Fixable) — ../ is a normal code pattern, not a vulnerability here
        assert!(matches!(v, Verdict::Clean));
        remove_test_file(&path);
    }

    #[tokio::test]
    async fn test_high_entropy_token_detected() {
        let dir = std::env::temp_dir();
        let path = dir.join("kf_code_sec_entropy_high.txt");
        std::fs::write(&path, "api_key = \"sk-abcdefghijklmnopqrstuvwxyz\"").unwrap();

        let event = BusEvent::FileWrite(FileWriteEvent {
            path: path.clone(),
            content_length: 50,
            content_hash: 0,
        });
        let v = verify_security(&event).await;
        assert!(
            matches!(v, Verdict::Unfixable(_)),
            "high-entropy sk- token should be flagged"
        );
        remove_test_file(&path);
    }

    #[tokio::test]
    async fn test_low_entropy_token_not_detected() {
        let dir = std::env::temp_dir();
        let path = dir.join("kf_code_sec_entropy_low.txt");
        std::fs::write(&path, "api_key = \"sk-aaaaaaaaaaaaaaaa\"").unwrap();

        let event = BusEvent::FileWrite(FileWriteEvent {
            path: path.clone(),
            content_length: 40,
            content_hash: 0,
        });
        let v = verify_security(&event).await;
        assert!(
            matches!(v, Verdict::Clean),
            "low-entropy sk- placeholder should not be flagged"
        );
        remove_test_file(&path);
    }

    // WO 15.10 (bucketlist 2.9): a dangerous shell pattern that appears
    // only inside a comment must not be flagged as Unfixable. The
    // correction loop would otherwise block on documentation that
    // documents the dangerous command.
    #[tokio::test]
    async fn test_shell_danger_in_slash_comment_is_skipped() {
        let dir = std::env::temp_dir();
        let path = dir.join("kf_code_sec_comment_slash.txt");
        std::fs::write(&path, "// do not run: rm -rf /\nlet x = 1;\n").unwrap();

        let event = BusEvent::FileWrite(FileWriteEvent {
            path: path.clone(),
            content_length: 40,
            content_hash: 0,
        });
        let v = verify_security(&event).await;
        assert!(
            matches!(v, Verdict::Clean),
            "dangerous shell pattern in a // comment must not be flagged"
        );
        remove_test_file(&path);
    }

    #[tokio::test]
    async fn test_shell_danger_in_hash_comment_is_skipped() {
        let dir = std::env::temp_dir();
        let path = dir.join("kf_code_sec_comment_hash.txt");
        std::fs::write(&path, "# do not run: rm -rf /\nx = 1\n").unwrap();

        let event = BusEvent::FileWrite(FileWriteEvent {
            path: path.clone(),
            content_length: 35,
            content_hash: 0,
        });
        let v = verify_security(&event).await;
        assert!(
            matches!(v, Verdict::Clean),
            "dangerous shell pattern in a # comment must not be flagged"
        );
        remove_test_file(&path);
    }

    #[tokio::test]
    async fn test_shell_danger_in_block_comment_is_skipped() {
        let dir = std::env::temp_dir();
        let path = dir.join("kf_code_sec_comment_block.txt");
        std::fs::write(
            &path,
            "/* warning: do not run rm -rf / on this host */\nlet x = 1;\n",
        )
        .unwrap();

        let event = BusEvent::FileWrite(FileWriteEvent {
            path: path.clone(),
            content_length: 55,
            content_hash: 0,
        });
        let v = verify_security(&event).await;
        assert!(
            matches!(v, Verdict::Clean),
            "dangerous shell pattern in a /* */ comment must not be flagged"
        );
        remove_test_file(&path);
    }

    #[tokio::test]
    async fn test_shell_danger_in_star_comment_line_is_skipped() {
        let dir = std::env::temp_dir();
        let path = dir.join("kf_code_sec_comment_star.txt");
        std::fs::write(
            &path,
            "/**\n * beware: rm -rf / wipes the disk\n */\nlet x = 1;\n",
        )
        .unwrap();

        let event = BusEvent::FileWrite(FileWriteEvent {
            path: path.clone(),
            content_length: 50,
            content_hash: 0,
        });
        let v = verify_security(&event).await;
        assert!(
            matches!(v, Verdict::Clean),
            "dangerous shell pattern on a ` * ` doc line must not be flagged"
        );
        remove_test_file(&path);
    }

    // Regression guard: a dangerous pattern on a real code line (no
    // comment prefix) must still trip the detector after the 2.9 fix.
    #[tokio::test]
    async fn test_shell_danger_on_code_line_still_flagged() {
        let dir = std::env::temp_dir();
        let path = dir.join("kf_code_sec_code_line.txt");
        std::fs::write(&path, "system(\"rm -rf /\");\n").unwrap();

        let event = BusEvent::FileWrite(FileWriteEvent {
            path: path.clone(),
            content_length: 20,
            content_hash: 0,
        });
        let v = verify_security(&event).await;
        assert!(
            matches!(v, Verdict::Unfixable(_)),
            "dangerous shell pattern on a code line must still be flagged"
        );
        remove_test_file(&path);
    }

    #[test]
    fn is_comment_line_recognises_common_prefixes() {
        assert!(is_comment_line("// rm -rf /"));
        assert!(is_comment_line("# rm -rf /"));
        assert!(is_comment_line("/* rm -rf /"));
        assert!(is_comment_line("* rm -rf /"));
        // Leading whitespace is trimmed before the prefix check.
        assert!(is_comment_line("    // rm -rf /"));
        assert!(is_comment_line("\t# rm -rf /"));
    }

    #[test]
    fn is_comment_line_rejects_code_lines() {
        assert!(!is_comment_line("rm -rf /"));
        assert!(!is_comment_line("system(\"rm -rf /\");"));
        assert!(!is_comment_line("let x = 1;"));
        assert!(!is_comment_line(""));
    }

    // WO 15.10 (bucketlist 2.14): a hung trufflehog (network/git-fetch
    // stall) must not block the correction loop indefinitely. The
    // verifier wraps the spawn in `tokio::time::timeout` and returns
    // `None` (no finding) on timeout. This test injects a fake
    // trufflehog that sleeps past the timeout and asserts the verifier
    // returns `Clean` rather than hanging.
    #[tokio::test]
    #[cfg(unix)]
    async fn test_trufflehog_timeout_does_not_block() {
        use std::os::unix::fs::PermissionsExt;

        let dir = std::env::temp_dir();
        let path = dir.join("kf_code_sec_trufflehog_timeout.txt");
        let fake_bin_dir = dir.join("kf_code_fake_bin_timeout");
        let fake_trufflehog = fake_bin_dir.join("trufflehog");
        std::fs::create_dir_all(&fake_bin_dir).unwrap();

        // Fake trufflehog sleeps well past the verifier timeout. Gated
        // on the marker env var so it can't leak into other tests if
        // PATH leaks; the sleep is long enough to prove the timeout
        // fires but bounded so a leaked process self-terminates.
        let script =
            "#!/bin/sh\nif [ \"$KF_CODE_FAKE_TRUFFLEHOG_SLEEP\" = \"1\" ]; then sleep 30; fi\n";
        std::fs::write(&fake_trufflehog, script).unwrap();
        let mut perms = std::fs::metadata(&fake_trufflehog).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&fake_trufflehog, perms).unwrap();

        // Use a low-entropy file so the local entropy check stays Clean
        // and the verifier reaches the trufflehog step.
        std::fs::write(&path, "api_key = \"sk-aaaaaaaaaaaaaaaa\"").unwrap();

        let original_path = std::env::var_os("PATH").clone();
        let new_path = format!(
            "{}:{}",
            fake_bin_dir.display(),
            original_path
                .as_ref()
                .map(|s| s.to_string_lossy())
                .unwrap_or_default()
        );
        std::env::set_var("PATH", new_path);
        std::env::set_var("KF_CODE_FAKE_TRUFFLEHOG_SLEEP", "1");

        let event = BusEvent::FileWrite(FileWriteEvent {
            path: path.clone(),
            content_length: 40,
            content_hash: 0,
        });
        // Drive verify_security with a per-test timeout shorter than the
        // fake trufflehog's 30s sleep but longer than the 2s test-mode
        // trufflehog cap. If the inner timeout regresses to an unbounded
        // wait, this outer timeout trips and the test fails. The test
        // `TRUFFLEHOG_TIMEOUT_SECS` override (2s) keeps this fast.
        let v = tokio::time::timeout(std::time::Duration::from_secs(15), verify_security(&event))
            .await
            .expect("verify_security should resolve before the 15s test budget");

        if let Some(p) = original_path {
            std::env::set_var("PATH", p);
        } else {
            std::env::remove_var("PATH");
        }
        std::env::remove_var("KF_CODE_FAKE_TRUFFLEHOG_SLEEP");
        remove_test_file(&path);
        remove_test_file(&fake_trufflehog);
        let _ = std::fs::remove_dir(&fake_bin_dir);

        assert!(
            matches!(v, Verdict::Clean),
            "a timed-out trufflehog must not block the correction loop (expected Clean, got {v:?})"
        );
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn test_trufflehog_path_discovery() {
        use std::os::unix::fs::PermissionsExt;

        let dir = std::env::temp_dir();
        let path = dir.join("kf_code_sec_trufflehog.txt");
        let fake_bin_dir = dir.join("kf_code_fake_bin");
        let fake_trufflehog = fake_bin_dir.join("trufflehog");
        std::fs::create_dir_all(&fake_bin_dir).unwrap();

        // Fake trufflehog emits a JSON finding only when the marker variable is set.
        // This avoids spurious findings in other concurrent tests if PATH leaks.
        let script = "#!/bin/sh\nif [ \"$1\" = \"filesystem\" ] && [ \"$KF_CODE_FAKE_TRUFFLEHOG\" = \"1\" ]; then echo '{\"detector_name\":\"test\"}'; fi\n";
        std::fs::write(&fake_trufflehog, script).unwrap();
        let mut perms = std::fs::metadata(&fake_trufflehog).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&fake_trufflehog, perms).unwrap();

        // Use a low-entropy file so the local entropy check stays Clean.
        std::fs::write(&path, "api_key = \"sk-aaaaaaaaaaaaaaaa\"").unwrap();

        let original_path = std::env::var_os("PATH").clone();
        let new_path = format!(
            "{}:{}",
            fake_bin_dir.display(),
            original_path
                .as_ref()
                .map(|s| s.to_string_lossy())
                .unwrap_or_default()
        );
        std::env::set_var("PATH", new_path);
        std::env::set_var("KF_CODE_FAKE_TRUFFLEHOG", "1");

        let event = BusEvent::FileWrite(FileWriteEvent {
            path: path.clone(),
            content_length: 40,
            content_hash: 0,
        });
        let v = verify_security(&event).await;

        if let Some(p) = original_path {
            std::env::set_var("PATH", p);
        } else {
            std::env::remove_var("PATH");
        }
        std::env::remove_var("KF_CODE_FAKE_TRUFFLEHOG");
        remove_test_file(&path);
        remove_test_file(&fake_trufflehog);
        let _ = std::fs::remove_dir(&fake_bin_dir);

        assert!(
            matches!(v, Verdict::Unfixable(_)),
            "trufflehog discovered via PATH should produce a finding"
        );
    }

    #[test]
    fn is_token_char_accepts_alphanumeric_and_secret_punctuation() {
        assert!(is_token_char('a'));
        assert!(is_token_char('Z'));
        assert!(is_token_char('0'));
        assert!(is_token_char('9'));
        assert!(is_token_char('-'));
        assert!(is_token_char('_'));
        assert!(is_token_char('+'));
        assert!(is_token_char('/'));
        assert!(is_token_char('='));
        assert!(is_token_char('.'));
    }

    #[test]
    fn is_token_char_rejects_whitespace_and_most_punctuation() {
        assert!(!is_token_char(' '));
        assert!(!is_token_char('\n'));
        assert!(!is_token_char('"'));
        assert!(!is_token_char('\''));
        assert!(!is_token_char('{'));
        assert!(!is_token_char('!'));
        assert!(!is_token_char('@'));
        assert!(!is_token_char('#'));
    }

    #[test]
    fn shannon_entropy_empty_is_zero() {
        assert_eq!(shannon_entropy(""), 0.0);
    }

    #[test]
    fn shannon_entropy_single_repeated_char_is_zero() {
        // A single symbol with probability 1 → -1*log2(1) = 0
        assert!(
            shannon_entropy(&"a".repeat(64)).abs() < 1e-12,
            "single repeated char has zero entropy"
        );
    }

    #[test]
    fn shannon_entropy_two_equal_symbols_is_one_bit() {
        // Half 'a' / half 'b' → 1 bit/char
        let s: String = "ab".repeat(32);
        let e = shannon_entropy(&s);
        assert!((e - 1.0).abs() < 1e-12, "expected 1.0 bit, got {e}");
    }

    #[test]
    fn shannon_entropy_uniform_random_is_high() {
        // A 64-char random-looking ASCII string should have >3 bits/char.
        let token = "9f8a7d6c5b4e3210ABCDxyzWVUtsrq";
        assert!(
            shannon_entropy(token) > 3.0,
            "random token should have high entropy, got {}",
            shannon_entropy(token)
        );
    }

    #[test]
    fn shannon_entropy_is_non_decreasing_with_diversity() {
        let low = "aaaaaa";
        let med = "aabbcc";
        let high = "abcdef";
        let el = shannon_entropy(low);
        let em = shannon_entropy(med);
        let eh = shannon_entropy(high);
        assert!(el < em, "low < med: {el} {em}");
        assert!(em < eh, "med < high: {em} {eh}");
    }

    #[test]
    fn scan_entropy_prefix_finds_high_entropy_token() {
        let content = "api_key = sk-9f8a7d6c5b4e3210ABCDxyzWVUtsrq";
        let path = std::path::Path::new("config.rs");
        let verdict = scan_entropy_prefix(content, "sk-", "OpenAI API key", path);
        assert!(
            verdict.is_some(),
            "high-entropy sk- token should be flagged"
        );
        if let Some(Verdict::Unfixable(err)) = verdict {
            assert!(err.description.contains("OpenAI API key"));
            assert!(err.file.as_ref().is_some_and(|f| f == path));
        } else {
            panic!("expected Unfixable verdict");
        }
    }

    #[test]
    fn scan_entropy_prefix_ignores_short_placeholder_tokens() {
        let content = "api_key = sk-short";
        let path = std::path::Path::new("config.rs");
        let verdict = scan_entropy_prefix(content, "sk-", "OpenAI API key", path);
        assert!(
            verdict.is_none(),
            "short low-entropy token must not be flagged"
        );
    }

    #[test]
    fn scan_entropy_prefix_ignores_low_entropy_repeated_token() {
        let content = format!("sk-{}", "a".repeat(40));
        let path = std::path::Path::new("config.rs");
        let verdict = scan_entropy_prefix(content.as_str(), "sk-", "OpenAI API key", path);
        assert!(
            verdict.is_none(),
            "long-but-low-entropy (all same char) token must not be flagged"
        );
    }

    #[test]
    fn scan_entropy_prefix_finds_multiple_occurrences() {
        let content = "sk-9f8a7d6c5b4e3210ABCDxyzWVUtsrq sk-9f8a7d6c5b4e3210ABCDxyzWVUtsrq";
        let path = std::path::Path::new("config.rs");
        let verdict = scan_entropy_prefix(content, "sk-", "OpenAI API key", path);
        assert!(verdict.is_some(), "any high-entropy hit should be flagged");
    }

    #[test]
    fn scan_entropy_prefix_no_match_returns_none() {
        let content = "no secret here";
        let path = std::path::Path::new("config.rs");
        assert!(scan_entropy_prefix(content, "sk-", "OpenAI API key", path).is_none());
    }

    #[test]
    fn entropy_scan_finds_openai_key_among_known_prefixes() {
        let content = "config: sk-9f8a7d6c5b4e3210ABCDxyzWVUtsrq";
        let path = std::path::Path::new("config.rs");
        let verdict = entropy_scan(content, path);
        assert!(verdict.is_some());
        if let Some(Verdict::Unfixable(err)) = verdict {
            assert!(err.description.contains("OpenAI API key"));
        }
    }

    #[test]
    fn entropy_scan_finds_aws_access_key() {
        let content = "AKIA9f8a7d6c5b4e3210ABCD";
        let path = std::path::Path::new("config.rs");
        let verdict = entropy_scan(content, path);
        assert!(verdict.is_some(), "AWS access key prefix should be scanned");
        if let Some(Verdict::Unfixable(err)) = verdict {
            assert!(err.description.contains("AWS access key"));
        }
    }

    #[test]
    fn entropy_scan_finds_xai_and_huggingface_tokens() {
        let path = std::path::Path::new("config.rs");
        let xai = entropy_scan("key = xai-9f8a7d6c5b4e3210ABCDxyzWVUtsrq", path);
        assert!(xai.is_some(), "xai- prefix should be scanned");
        if let Some(Verdict::Unfixable(err)) = xai {
            assert!(err.description.contains("xAI API key"));
        }
        let hf = entropy_scan("key = hf_9f8a7d6c5b4e3210ABCDxyzWVUtsrq", path);
        assert!(hf.is_some(), "hf_ prefix should be scanned");
        if let Some(Verdict::Unfixable(err)) = hf {
            assert!(err.description.contains("HuggingFace access token"));
        }
    }

    #[test]
    fn entropy_scan_does_not_flag_claude_model_name() {
        let path = std::path::Path::new("config.rs");
        let verdict = entropy_scan("model = claude-3-opus-20240229", path);
        assert!(
            verdict.is_none(),
            "claude- is intentionally excluded (model-name false positives)"
        );
    }

    #[test]
    fn entropy_scan_returns_none_for_clean_content() {
        let content = "fn main() { println!(\"hello\"); }";
        let path = std::path::Path::new("config.rs");
        assert!(entropy_scan(content, path).is_none());
    }

    #[test]
    fn probe_path_returns_path_when_file_exists() {
        let tmp = tempfile::tempdir().unwrap();
        let f = tmp.path().join("probe_target");
        std::fs::write(&f, b"x").unwrap();
        assert_eq!(probe_path(f.to_str().unwrap()), Some(f.clone()));
    }

    #[test]
    fn probe_path_returns_none_for_missing_file() {
        assert!(probe_path("/nonexistent/path/please/missing-binary-xyz").is_none());
    }

    #[test]
    fn probe_path_returns_none_for_directory() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(probe_path(tmp.path().to_str().unwrap()).is_none());
    }

    #[test]
    fn find_in_path_returns_none_when_path_unset() {
        let original = std::env::var_os("PATH");
        std::env::remove_var("PATH");
        let result = find_in_path("definitely-not-on-path-xyz-987");
        if let Some(p) = original {
            std::env::set_var("PATH", p);
        }
        assert!(
            result.is_none(),
            "with PATH unset, lookup should return None"
        );
    }

    #[test]
    fn find_in_path_returns_none_for_missing_binary() {
        let result = find_in_path("definitely-not-on-path-xyz-987");
        assert!(
            result.is_none(),
            "made-up binary name must not resolve on PATH"
        );
    }
}
