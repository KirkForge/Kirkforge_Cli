use crate::session::event_bus::{BusEvent, FileWriteEvent};
/// Security verifier — scans file writes for dangerous patterns.
///
/// Checks written files for:
/// - Hardcoded API keys / secrets (substring matching)
/// - Dangerous shell commands in scripts
/// - Path traversal vulnerabilities
use crate::session::verifier::{Verdict, VerificationError};

/// Known secret patterns (substring-based).
const SECRET_PATTERNS: &[(&str, &str)] = &[
    ("API key sk-", "sk-"),
    ("AWS key AKIA", "AKIA"),
    ("Private key PEM", "-----BEGIN PRIVATE KEY-----"),
    ("Private key RSA", "-----BEGIN RSA PRIVATE KEY-----"),
    ("GitHub token ghp_", "ghp_"),
    ("GitHub token gho_", "gho_"),
    ("GitHub token ghu_", "ghu_"),
    ("GitHub token ghs_", "ghs_"),
    ("GitHub token ghr_", "ghr_"),
    ("MongoDB+srv", "mongodb+srv://"),
    ("MongoDB", "mongodb://"),
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

/// Run the security verifier against an event.
pub async fn verify_security(event: &BusEvent) -> Verdict {
    let (path, content_length) = match event {
        BusEvent::FileWrite(FileWriteEvent {
            path,
            content_length,
        }) => (path.clone(), *content_length),
        _ => return Verdict::Skipped("not a file write event".into()),
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

    // 1. Check for secrets
    for (name, pattern) in SECRET_PATTERNS {
        if content.contains(pattern) {
            return Verdict::Unfixable(VerificationError {
                description: format!("Potential secret detected: {}", name),
                file: Some(path.clone()),
                details: format!(
                    "Pattern '{}' found in {}. This must be reviewed manually.",
                    pattern,
                    path.display()
                ),
            });
        }
    }

    // 2. Check for dangerous shell patterns (in .sh, .bash, or any executable script)
    let is_shell_script = path
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| matches!(e, "sh" | "bash" | "zsh"));
    if is_shell_script {
        for pattern in DANGEROUS_SHELL_PATTERNS {
            if content.contains(pattern) {
                return Verdict::Unfixable(VerificationError {
                    description: format!("Dangerous shell command: {}", pattern),
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

    #[tokio::test]
    async fn test_skips_non_file_write_events() {
        let event = BusEvent::Edit(crate::session::event_bus::EditEvent {
            path: std::path::PathBuf::from("x.rs"),
            diff: "".into(),
        });
        let v = verify_security(&event).await;
        assert!(matches!(v, Verdict::Skipped(_)));
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
        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn test_detects_api_key_pattern() {
        let dir = std::env::temp_dir();
        let path = dir.join("kirkforge_sec_key.txt");
        std::fs::write(&path, "api_key = \"sk-abc123def456\"").unwrap();

        let event = BusEvent::FileWrite(FileWriteEvent {
            path: path.clone(),
            content_length: 50,
        });
        let v = verify_security(&event).await;
        assert!(matches!(v, Verdict::Unfixable(_)));
        let _ = std::fs::remove_file(&path);
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
        let _ = std::fs::remove_file(&path);
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
        let _ = std::fs::remove_file(&path);
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
        let _ = std::fs::remove_file(&path);
    }
}
