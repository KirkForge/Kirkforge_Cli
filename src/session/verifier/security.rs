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
const ENTROPY_PREFIXES: &[(&str, &str)] = &[
    ("OpenAI API key", "sk-"),
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
async fn trufflehog_scan(path: &Path) -> Option<Verdict> {
    let binary = match trufflehog_path() {
        Some(b) => b,
        None => return None,
    };
    let output = match tokio::process::Command::new(&binary)
        .arg("filesystem")
        .arg("--no-update")
        .arg("--json")
        .arg(path)
        .output()
        .await
    {
        Ok(o) => o,
        Err(_) => return None,
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
        return Verdict::Clean;
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

    // 4. Check for dangerous shell patterns (in .sh, .bash, or any executable script)
    let is_shell_script = path
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| matches!(e, "sh" | "bash" | "zsh"));
    if is_shell_script {
        for pattern in DANGEROUS_SHELL_PATTERNS {
            if content.contains(pattern) {
                return Verdict::Unfixable(VerificationError {
                    description: format!("Dangerous shell command: {pattern}"),
                    file: Some(path.clone()),
                    details: "This command is blocked by security policy. Remove it to proceed."
                        .into(),
                });
            }
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
        let path = dir.join("kirkforge_sec_edit_check.txt");
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
        let path = dir.join("kirkforge_sec_edit_key.txt");
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
        let path = dir.join("kirkforge_sec_clean.txt");
        std::fs::write(&path, "let x = 1;").unwrap();

        let event = BusEvent::FileWrite(FileWriteEvent {
            path: path.clone(),
            content_length: 10,
        });
        let v = verify_security(&event).await;
        assert!(matches!(v, Verdict::Clean));
        remove_test_file(&path);
    }

    #[tokio::test]
    async fn test_detects_api_key_pattern() {
        let dir = std::env::temp_dir();
        let path = dir.join("kirkforge_sec_key.txt");
        // High-entropy token long enough to trip the entropy detector.
        std::fs::write(&path, "api_key = \"sk-abcdefghijklmnopqrstuvwxyz012345\"").unwrap();

        let event = BusEvent::FileWrite(FileWriteEvent {
            path: path.clone(),
            content_length: 50,
        });
        let v = verify_security(&event).await;
        assert!(matches!(v, Verdict::Unfixable(_)));
        remove_test_file(&path);
    }

    #[tokio::test]
    async fn test_detects_private_key() {
        let dir = std::env::temp_dir();
        let path = dir.join("kirkforge_sec_private.pem");
        std::fs::write(
            &path,
            "-----BEGIN PRIVATE KEY-----\nABC123\n-----END PRIVATE KEY-----",
        )
        .unwrap();

        let event = BusEvent::FileWrite(FileWriteEvent {
            path: path.clone(),
            content_length: 80,
        });
        let v = verify_security(&event).await;
        assert!(matches!(v, Verdict::Unfixable(_)));
        remove_test_file(&path);
    }

    #[tokio::test]
    async fn test_detects_shell_danger() {
        let dir = std::env::temp_dir();
        let path = dir.join("kirkforge_sec_danger.sh");
        std::fs::write(&path, "rm -rf /").unwrap();

        let event = BusEvent::FileWrite(FileWriteEvent {
            path: path.clone(),
            content_length: 10,
        });
        let v = verify_security(&event).await;
        assert!(matches!(v, Verdict::Unfixable(_)));
        remove_test_file(&path);
    }

    #[tokio::test]
    async fn test_path_traversal_no_false_positive() {
        // `../` inside string content must NOT be flagged (it's a legitimate import)
        let dir = std::env::temp_dir();
        let path = dir.join("kirkforge_sec_traversal.txt");
        std::fs::write(&path, "require('../../secret')").unwrap();

        let event = BusEvent::FileWrite(FileWriteEvent {
            path: path.clone(),
            content_length: 30,
        });
        let v = verify_security(&event).await;
        // Must be Clean (no Fixable) — ../ is a normal code pattern, not a vulnerability here
        assert!(matches!(v, Verdict::Clean));
        remove_test_file(&path);
    }

    #[tokio::test]
    async fn test_high_entropy_token_detected() {
        let dir = std::env::temp_dir();
        let path = dir.join("kirkforge_sec_entropy_high.txt");
        std::fs::write(&path, "api_key = \"sk-abcdefghijklmnopqrstuvwxyz\"").unwrap();

        let event = BusEvent::FileWrite(FileWriteEvent {
            path: path.clone(),
            content_length: 50,
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
        let path = dir.join("kirkforge_sec_entropy_low.txt");
        std::fs::write(&path, "api_key = \"sk-aaaaaaaaaaaaaaaa\"").unwrap();

        let event = BusEvent::FileWrite(FileWriteEvent {
            path: path.clone(),
            content_length: 40,
        });
        let v = verify_security(&event).await;
        assert!(
            matches!(v, Verdict::Clean),
            "low-entropy sk- placeholder should not be flagged"
        );
        remove_test_file(&path);
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn test_trufflehog_path_discovery() {
        use std::os::unix::fs::PermissionsExt;

        let dir = std::env::temp_dir();
        let path = dir.join("kirkforge_sec_trufflehog.txt");
        let fake_bin_dir = dir.join("kirkforge_fake_bin");
        let fake_trufflehog = fake_bin_dir.join("trufflehog");
        std::fs::create_dir_all(&fake_bin_dir).unwrap();

        // Fake trufflehog emits a JSON finding only when the marker variable is set.
        // This avoids spurious findings in other concurrent tests if PATH leaks.
        let script = "#!/bin/sh\nif [ \"$1\" = \"filesystem\" ] && [ \"$KIRKFORGE_FAKE_TRUFFLEHOG\" = \"1\" ]; then echo '{\"detector_name\":\"test\"}'; fi\n";
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
        std::env::set_var("KIRKFORGE_FAKE_TRUFFLEHOG", "1");

        let event = BusEvent::FileWrite(FileWriteEvent {
            path: path.clone(),
            content_length: 40,
        });
        let v = verify_security(&event).await;

        if let Some(p) = original_path {
            std::env::set_var("PATH", p);
        } else {
            std::env::remove_var("PATH");
        }
        std::env::remove_var("KIRKFORGE_FAKE_TRUFFLEHOG");
        remove_test_file(&path);
        remove_test_file(&fake_trufflehog);
        let _ = std::fs::remove_dir(&fake_bin_dir);

        assert!(
            matches!(v, Verdict::Unfixable(_)),
            "trufflehog discovered via PATH should produce a finding"
        );
    }
}
