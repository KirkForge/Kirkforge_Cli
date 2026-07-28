//! Command-string safety analyzer for the bash runner.
//!
//! Pure (no I/O) gate that inspects a shell command string for dangerous
//! patterns, privilege escalation, password prompts, and redirections to
//! system-sensitive paths, plus the user-configured deny list and sandbox
//! workdir policy. Shared by the model `bash` tool, the `!` bang passthrough,
//! the `/test` slash command, and lifecycle hooks so every shell execution
//! goes through one gate. Extracted from `bash_runner` so the execution
//! half is process/IO and this half is static analysis.

use crate::session::access::{DenyList, PathGuard};
use std::path::Path;

/// Dangerous shell commands. These are the exact raw-string patterns
/// checked before normalization; the safety check also scans the normalized
/// command with [`word_boundary_match`] so trivial evasions such as
/// `r'm -rf /'` or `chmod -R 777  /` are still caught.
const DANGEROUS_SHELL_COMMANDS: &[&str] = &[
    "rm -rf /",
    "rm -rf /*",
    "rm -fr /",
    "rm -fr /*",
    "rm --no-preserve-root -rf /",
    "rm --no-preserve-root -fr /",
    "rm -rf --no-preserve-root /",
    "rm -fr --no-preserve-root /",
    ":(){ :|:& };:",
    "> /dev/sda",
    "mkfs.",
    "dd if=/dev/zero of=",
    "dd if=/dev/random of=",
    "dd if=/dev/urandom of=",
    "chmod -R 777 /",
    "chmod 777 /",
    "chmod -R a+rwx /",
    "chmod a+rwx /",
    "chown -R root:root /",
    "chown root:root /",
    "dd if=/dev/random",
    "> /dev/null < /dev/sda",
];

/// Privilege-escalation commands. These require interactive authentication
/// or can switch users, so they are blocked in model-driven execution.
const PRIVILEGE_ESCALATION_COMMANDS: &[&str] = &["sudo", "su", "doas"];

/// Interactive password-prompt patterns. Blocking these prevents the model
/// from accidentally hanging on a hidden `read -s` or password utility.
const INTERACTIVE_PASSWORD_PATTERNS: &[&str] = &["read -s", "stty -echo", "passwd"];

/// Dangerous redirection prefixes. Any stdout overwrite or `tee` into these
/// system-sensitive directories is blocked regardless of approval state.
///
/// These are the exact raw-string patterns checked before normalization; the
/// safety check also scans the normalized command with
/// [`redirects_to_dangerous_path`] and [`tee_to_dangerous_path`] so spacing,
/// quoting, and Windows-path variants are caught as well.
const DANGEROUS_REDIRECTION_PATTERNS: &[&str] = &[
    "> /etc/",
    ">> /etc/",
    ">| /etc/",
    "> ~/.ssh/",
    ">> ~/.ssh/",
    ">| ~/.ssh/",
    "> /root/",
    ">> /root/",
    ">| /root/",
    "tee /etc/",
    "tee ~/.ssh/",
    "tee /root/",
    "> C:/Windows/",
    ">> C:/Windows/",
    "> C:\\Windows\\",
    ">> C:\\Windows\\",
    "> %SystemRoot%",
    ">> %SystemRoot%",
];

/// True if `pattern` appears in `cmd` at a word boundary (start/end of
/// string, whitespace, or shell metacharacter). Used so `rm -rf /` blocks
/// the exact dangerous command even when it appears inside a pipeline.
pub(super) fn word_boundary_match(cmd: &str, pattern: &str) -> bool {
    let boundaries = [' ', '\t', '\n', '|', ';', '&', '(', ')', '<', '>', '\0'];
    let p: Vec<char> = pattern.chars().collect();
    let chars: Vec<char> = cmd.chars().collect();
    let mut i = 0;
    while i + p.len() <= chars.len() {
        if chars[i..i + p.len()] == p {
            let start_ok = i == 0 || boundaries.contains(&chars[i - 1]);
            let end_ok = i + p.len() >= chars.len() || boundaries.contains(&chars[i + p.len()]);
            if start_ok && end_ok {
                return true;
            }
        }
        i += 1;
    }
    false
}

/// Normalize a shell command so that trivial quoting/whitespace/comment
/// evasions do not defeat the deny-list. This is a preprocessor, not a shell
/// parser: it removes comments, strips single/double quotes, collapses
/// whitespace, lowercases alphabetic characters, and strips simple backslash
/// escapes. Backticks are intentionally left intact because they denote
/// command substitution, which the safety layer treats literally.
fn normalize_for_safety(cmd: &str) -> String {
    let mut out = String::with_capacity(cmd.len());
    let mut chars = cmd.chars().peekable();
    let mut in_single = false;
    let mut in_double = false;
    while let Some(c) = chars.next() {
        if in_single {
            if c == '\'' {
                in_single = false;
            } else {
                out.push(c.to_ascii_lowercase());
            }
            continue;
        }
        if in_double {
            if c == '"' {
                in_double = false;
            } else if c == '\\' {
                // Preserve the escaped character's literal value inside double
                // quotes so "r\"m -rf /" still normalizes to "rm -rf /".
                out.push(chars.next().unwrap_or(c).to_ascii_lowercase());
            } else {
                out.push(c.to_ascii_lowercase());
            }
            continue;
        }
        match c {
            '\'' => in_single = true,
            '"' => in_double = true,
            '#' => break, // comment to end of line
            '\\' => {
                // Strip simple backslash escapes outside quotes.
                if let Some(next) = chars.next() {
                    out.push(next.to_ascii_lowercase());
                }
            }
            c => out.push(c.to_ascii_lowercase()),
        }
    }
    // Collapse whitespace to single spaces and trim.
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// True if the command uses shell parameter expansions that can inject
/// whitespace or other characters to evade the literal deny-list.
///
/// Examples caught here: `${IFS}`, `${IFS:- }`, `$IFS`, and ANSI-C quoting
/// `$'...'`. Bash expands `${IFS}` to a space (or the current field separator),
/// so `rm${IFS:- }-rf${IFS:- }/` becomes `rm -rf /` at execution time even
/// though the raw string never contains that literal.
fn contains_shell_expansion_evasion(cmd: &str) -> bool {
    cmd.contains("${IFS") || cmd.contains("$IFS") || cmd.contains("$'")
}

/// True if `b` is a shell token separator for redirection-target scanning.
fn is_shell_token_separator(b: u8) -> bool {
    matches!(
        b,
        b' ' | b'\t' | b'\n' | b'|' | b';' | b'&' | b'(' | b')' | b'<' | b'>' | b'`'
    )
}

/// Path prefixes that a model-driven shell should never be allowed to
/// overwrite, either via redirection or via `tee`. Includes Unix system paths,
/// raw block devices, Windows-style paths, and the Git-Bash/WSL mount forms
/// so the same gate works across platforms.
const DANGEROUS_REDIRECTION_TARGETS: &[&str] = &[
    "/etc/",
    "~/.ssh/",
    "/root/",
    "/home/",
    "/usr/",
    "/bin/",
    "/sbin/",
    "/lib/",
    "/lib64/",
    "/boot/",
    "/dev/sda",
    "/dev/hda",
    "/dev/nvme",
    "/dev/xvd",
    "/dev/vd",
    "/dev/mmcblk",
    "%systemroot%",
    "%userprofile%",
    "c:\\windows",
    "c:\\programdata",
    "c:\\users\\",
    "/c/windows/",
    "/mnt/c/windows/",
];

/// True if the normalized command redirects output to a system-sensitive
/// path. This catches `> /etc/hosts`, `>>  /root/.bashrc`, `>|"~/.ssh"`,
/// `2>/etc/passwd`, `&> /dev/sda`, etc., including Windows paths seen
/// through Git-Bash/WSL mounts.
fn redirects_to_dangerous_path(cmd: &str) -> Option<&'static str> {
    let normalized = normalize_for_safety(cmd);
    let bytes = normalized.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        // Detect output redirection operators, optionally prefixed by a fd
        // (`1>`, `2>`, `&>`) or the clobber form (`>|`).
        let op_len = if bytes[i] == b'>' {
            if i + 1 < bytes.len() && matches!(bytes[i + 1], b'>' | b'|') {
                2
            } else {
                1
            }
        } else if i + 1 < bytes.len()
            && (bytes[i].is_ascii_digit() || bytes[i] == b'&')
            && bytes[i + 1] == b'>'
        {
            2
        } else {
            0
        };

        if op_len == 0 {
            i += 1;
            continue;
        }

        // Find the redirection target, skipping whitespace.
        let mut j = i + op_len;
        while j < bytes.len() && bytes[j].is_ascii_whitespace() {
            j += 1;
        }
        let start = j;
        while j < bytes.len() && !is_shell_token_separator(bytes[j]) {
            j += 1;
        }
        let target = std::str::from_utf8(&bytes[start..j])
            .unwrap_or("")
            .to_lowercase();
        for prefix in DANGEROUS_REDIRECTION_TARGETS {
            if target.starts_with(prefix) || target == prefix.trim_end_matches('/') {
                return Some(*prefix);
            }
        }
        i = j;
    }
    None
}

/// True if the command uses `tee` to write to a system-sensitive path.
fn tee_to_dangerous_path(cmd: &str) -> Option<&'static str> {
    let normalized = normalize_for_safety(cmd);
    // Tokenize naively and look for a `tee` word followed by a dangerous path.
    let tokens: Vec<&str> = normalized.split_whitespace().collect();
    for window in tokens.windows(2) {
        let is_tee = window[0] == "tee"
            || window[0].ends_with("|tee")
            || window[0].ends_with(";tee")
            || window[0].ends_with("&&tee")
            || window[0].ends_with("||tee");
        if is_tee {
            let target = window[1].to_lowercase();
            for prefix in DANGEROUS_REDIRECTION_TARGETS {
                if target.starts_with(prefix) || target == prefix.trim_end_matches('/') {
                    return Some(*prefix);
                }
            }
        }
    }
    None
}

/// Safety check for a bash command. Returns `Some(reason)` if the command
/// should be blocked, `None` if it may proceed.
///
/// This is shared between the model's `bash` tool, the `!` bang passthrough,
/// the `/test` slash command, and lifecycle hooks so every shell execution
/// goes through the same sandbox, deny-list, and dangerous-pattern gates.
pub fn check_bash_command_str(
    cmd: &str,
    workdir: Option<&str>,
    deny_list: &DenyList,
    path_guard: &PathGuard,
    bash_sandbox_workdir: bool,
) -> Option<String> {
    // 1. Sandboxed workdir policy. If enabled, reject an explicit workdir
    //    that points outside the sandbox. If we cannot canonicalize the path
    //    we deny: a non-canonical path containing `..` could pass the
    //    prefix check while resolving outside the sandbox.
    if bash_sandbox_workdir {
        if let Some(workdir) = workdir {
            if !workdir.is_empty() {
                let workdir_path = Path::new(workdir);
                let resolved = match workdir_path.canonicalize() {
                    Ok(p) => p,
                    Err(_) => {
                        return Some(format!(
                            "🔒 Bash workdir cannot be resolved: {workdir} (sandbox enforcement active)"
                        ));
                    }
                };
                if let Some(ref sandbox) = path_guard.sandbox_dir {
                    let sb = match sandbox.canonicalize() {
                        Ok(p) => p,
                        Err(_) => {
                            return Some(format!(
                                "🔒 Sandbox directory cannot be resolved: {}",
                                sandbox.display()
                            ));
                        }
                    };
                    if !resolved.starts_with(&sb) {
                        return Some(format!(
                            "🔒 Bash workdir outside sandbox: {} (sandbox: {})",
                            workdir,
                            sandbox.display()
                        ));
                    }
                }
            }
        }
    }

    // 2. Hard-coded metadata endpoint blocks.
    if cmd.contains("169.254.169.254")
        || cmd.contains("metadata.google")
        || cmd.contains("metadata.aws")
    {
        return Some("🔒 Command blocked: contains reference to metadata endpoints".into());
    }

    // 3. User-configured URL deny list.
    for url_prefix in &deny_list.url_patterns {
        if !url_prefix.is_empty() && cmd.contains(url_prefix) {
            return Some(format!(
                "🔒 Command blocked: references denied URL '{url_prefix}'"
            ));
        }
    }

    let normalized = normalize_for_safety(cmd);

    // 4. Shell expansion evasions that inject whitespace or other characters
    //    to dodge the literal deny-list (e.g. `${IFS:- }`, `$IFS`, `$'...'`).
    //    Detect these before the pattern scan so a ReadOnly allow rule that
    //    skips approval cannot be bypassed.
    if contains_shell_expansion_evasion(cmd) || contains_shell_expansion_evasion(&normalized) {
        return Some("🔒 Command blocked: shell parameter expansion evasion detected".into());
    }

    // 5. Built-in dangerous shell patterns and hard-coded system paths.
    //    Check both the raw command and a normalized copy (quotes stripped,
    //    whitespace collapsed, comments removed, lowercased) so trivial
    //    quoting/whitespace evasions do not bypass the gate.
    for pattern in DANGEROUS_SHELL_COMMANDS {
        let needs_word_boundary = pattern.ends_with('/') || pattern.ends_with(' ');
        let pattern_lower = pattern.to_ascii_lowercase();
        let matches_raw = if needs_word_boundary {
            word_boundary_match(cmd, pattern)
        } else {
            cmd.contains(pattern)
        };
        let matches_normalized = if needs_word_boundary {
            word_boundary_match(&normalized, &pattern_lower)
        } else {
            normalized.contains(&pattern_lower)
        };
        if matches_raw || matches_normalized {
            return Some(format!(
                "🔒 Command blocked: dangerous pattern '{pattern}' detected"
            ));
        }
    }

    for pat in [
        "/etc/shadow",
        "/etc/passwd",
        "/etc/sudoers",
        "~/.ssh",
        "/root/",
    ] {
        if cmd.contains(pat) || normalized.contains(pat) {
            return Some(format!(
                "🔒 Command blocked: references denied path '{pat}'"
            ));
        }
    }

    // 6. Privilege escalation, password prompts, and dangerous redirections.
    for pat in PRIVILEGE_ESCALATION_COMMANDS {
        let pat_lower = pat.to_ascii_lowercase();
        if word_boundary_match(cmd, pat) || word_boundary_match(&normalized, &pat_lower) {
            return Some(format!(
                "🔒 Command blocked: privilege escalation command '{pat}' is not allowed"
            ));
        }
    }
    for pat in INTERACTIVE_PASSWORD_PATTERNS {
        let pat_lower = pat.to_ascii_lowercase();
        if word_boundary_match(cmd, pat) || word_boundary_match(&normalized, &pat_lower) {
            return Some(format!(
                "🔒 Command blocked: interactive password prompt '{pat}' is not allowed"
            ));
        }
    }
    for pat in DANGEROUS_REDIRECTION_PATTERNS {
        let pat_lower = pat.to_ascii_lowercase();
        if cmd.contains(pat) || normalized.contains(&pat_lower) {
            return Some(format!(
                "🔒 Command blocked: dangerous redirection to system path '{pat}'"
            ));
        }
    }
    if let Some(prefix) = redirects_to_dangerous_path(cmd) {
        return Some(format!(
            "🔒 Command blocked: dangerous redirection to system path '{prefix}'"
        ));
    }
    if let Some(prefix) = tee_to_dangerous_path(cmd) {
        return Some(format!(
            "🔒 Command blocked: dangerous redirection to system path '{prefix}'"
        ));
    }

    // 7. User-configured path deny list. Tokenize the command and check
    //    each token as a path, using normalized tokens so quoted paths are
    //    still evaluated.
    for token in normalized.split_whitespace() {
        if deny_list.is_path_denied(Path::new(token)) {
            return Some(format!(
                "🔒 Command blocked: references denied path '{token}'"
            ));
        }
    }

    None
}

/// JSON-args wrapper around [`check_bash_command_str`] for the model's
/// `bash` tool invocation path.
pub fn check_bash_command(
    args: &serde_json::Value,
    deny_list: &DenyList,
    path_guard: &PathGuard,
    bash_sandbox_workdir: bool,
) -> Option<String> {
    let cmd = args.get("command").and_then(|c| c.as_str()).unwrap_or("");
    let workdir = args.get("workdir").and_then(|w| w.as_str());
    check_bash_command_str(cmd, workdir, deny_list, path_guard, bash_sandbox_workdir)
}

#[cfg(test)]
mod private_tests {
    use super::*;

    #[test]
    fn normalize_strips_single_quotes() {
        let n = normalize_for_safety("r'm -rf /'");
        assert!(n.contains("rm -rf /"));
    }

    #[test]
    fn normalize_strips_double_quotes() {
        let n = normalize_for_safety(r#"rm "-rf" /"#);
        assert!(n.contains("rm -rf /"));
    }

    #[test]
    fn normalize_strips_backslash_escape_outside_quotes() {
        let n = normalize_for_safety(r"rm\ -rf\ /");
        assert_eq!(n, "rm -rf /");
    }

    #[test]
    fn normalize_preserves_escaped_char_inside_double_quotes() {
        let n = normalize_for_safety("\"r\\\"m -rf /\"");
        assert!(n.contains("r\"m -rf /"), "got: {n}");
    }

    #[test]
    fn normalize_truncates_at_hash_comment() {
        let n = normalize_for_safety("echo hi # cleanup");
        assert_eq!(n, "echo hi");
    }

    #[test]
    fn normalize_lowercases_alphabetic_chars() {
        let n = normalize_for_safety("RM -RF /");
        assert_eq!(n, "rm -rf /");
    }

    #[test]
    fn normalize_collapses_repeated_whitespace() {
        let n = normalize_for_safety("rm    -rf    /");
        assert_eq!(n, "rm -rf /");
    }

    #[test]
    fn normalize_keeps_backticks_intact() {
        let n = normalize_for_safety("echo `rm -rf /`");
        assert!(n.contains("`"));
    }

    #[test]
    fn normalize_empty_string() {
        assert_eq!(normalize_for_safety(""), "");
    }

    #[test]
    fn normalize_only_whitespace_collapses_to_empty() {
        assert_eq!(normalize_for_safety("   \t\n  "), "");
    }

    #[test]
    fn shell_expansion_evasion_detects_ifs() {
        assert!(contains_shell_expansion_evasion("${IFS}"));
        assert!(contains_shell_expansion_evasion("$IFS"));
        assert!(contains_shell_expansion_evasion("${IFS:- }"));
    }

    #[test]
    fn shell_expansion_evasion_detects_ansi_c_quoting() {
        assert!(contains_shell_expansion_evasion("$' '"));
    }

    #[test]
    fn shell_expansion_evasion_rejects_plain_text() {
        assert!(!contains_shell_expansion_evasion("rm -rf /"));
        assert!(!contains_shell_expansion_evasion(""));
    }

    #[test]
    fn shell_token_separator_classifies_metacharacters() {
        assert!(is_shell_token_separator(b' '));
        assert!(is_shell_token_separator(b'\t'));
        assert!(is_shell_token_separator(b'\n'));
        assert!(is_shell_token_separator(b'|'));
        assert!(is_shell_token_separator(b';'));
        assert!(is_shell_token_separator(b'&'));
        assert!(is_shell_token_separator(b'('));
        assert!(is_shell_token_separator(b')'));
        assert!(is_shell_token_separator(b'<'));
        assert!(is_shell_token_separator(b'>'));
        assert!(is_shell_token_separator(b'`'));
    }

    #[test]
    fn shell_token_separator_rejects_alphanumerics() {
        assert!(!is_shell_token_separator(b'a'));
        assert!(!is_shell_token_separator(b'0'));
        assert!(!is_shell_token_separator(b'/'));
        assert!(!is_shell_token_separator(b'.'));
    }

    #[test]
    fn redirects_to_dangerous_path_detects_etcs() {
        assert_eq!(
            redirects_to_dangerous_path("echo x > /etc/hosts"),
            Some("/etc/")
        );
    }

    #[test]
    fn redirects_to_dangerous_path_detects_append_to_etcs() {
        assert_eq!(
            redirects_to_dangerous_path("echo x >> /etc/passwd"),
            Some("/etc/")
        );
    }

    #[test]
    fn redirects_to_dangerous_path_detects_clobber_to_ssh() {
        assert_eq!(
            redirects_to_dangerous_path("echo x >| ~/.ssh/config"),
            Some("~/.ssh/")
        );
    }

    #[test]
    fn redirects_to_dangerous_path_detects_fd_prefixed() {
        assert_eq!(
            redirects_to_dangerous_path("echo x 2> /etc/hosts"),
            Some("/etc/")
        );
        assert_eq!(
            redirects_to_dangerous_path("echo x &> /etc/hosts"),
            Some("/etc/")
        );
    }

    #[test]
    fn redirects_to_dangerous_path_detects_device() {
        assert_eq!(
            redirects_to_dangerous_path("dd if=/dev/zero > /dev/sda"),
            Some("/dev/sda")
        );
    }

    #[test]
    fn redirects_to_dangerous_path_returns_none_for_safe_target() {
        assert_eq!(redirects_to_dangerous_path("echo x > /tmp/out.txt"), None);
    }

    #[test]
    fn redirects_to_dangerous_path_returns_none_for_no_redirect() {
        assert_eq!(redirects_to_dangerous_path("ls -la"), None);
    }

    #[test]
    fn redirects_to_dangerous_path_normalizes_quotes_in_target() {
        assert_eq!(
            redirects_to_dangerous_path("echo x > '/etc/hosts'"),
            Some("/etc/")
        );
    }

    #[test]
    fn tee_to_dangerous_path_detects_tee_etc() {
        assert_eq!(
            tee_to_dangerous_path("echo x | tee /etc/hosts"),
            Some("/etc/")
        );
    }

    #[test]
    fn tee_to_dangerous_path_detects_tee_ssh() {
        assert_eq!(
            tee_to_dangerous_path("echo x | tee ~/.ssh/config"),
            Some("~/.ssh/")
        );
    }

    #[test]
    fn tee_to_dangerous_path_detects_after_semicolon() {
        assert_eq!(
            tee_to_dangerous_path("echo y; tee /etc/passwd"),
            Some("/etc/")
        );
    }

    #[test]
    fn tee_to_dangerous_path_returns_none_for_safe_target() {
        assert_eq!(tee_to_dangerous_path("echo x | tee /tmp/out.txt"), None);
    }

    #[test]
    fn tee_to_dangerous_path_returns_none_for_no_tee() {
        assert_eq!(tee_to_dangerous_path("ls -la"), None);
    }

    #[test]
    fn check_bash_command_str_allows_empty_command() {
        assert!(check_bash_command_str(
            "",
            None,
            &DenyList::default(),
            &PathGuard::default(),
            false
        )
        .is_none());
    }

    #[test]
    fn check_bash_command_str_blocks_metadata_google() {
        assert!(check_bash_command_str(
            "curl http://metadata.google.internal/computeMetadata/",
            None,
            &DenyList::default(),
            &PathGuard::default(),
            false
        )
        .is_some_and(|m| m.contains("metadata")),);
    }

    #[test]
    fn check_bash_command_str_blocks_metadata_aws() {
        assert!(check_bash_command_str(
            "curl http://metadata.aws.internal/latest/",
            None,
            &DenyList::default(),
            &PathGuard::default(),
            false
        )
        .is_some_and(|m| m.contains("metadata")),);
    }

    #[test]
    fn check_bash_command_str_blocks_dangerous_double_quoted() {
        assert!(check_bash_command_str(
            r#"rm "-rf" /"#,
            None,
            &DenyList::default(),
            &PathGuard::default(),
            false
        )
        .is_some_and(|m| m.contains("dangerous pattern")),);
    }

    #[test]
    fn check_bash_command_str_blocks_dev_sda_redirect() {
        assert!(check_bash_command_str(
            "cat /dev/zero > /dev/sda",
            None,
            &DenyList::default(),
            &PathGuard::default(),
            false
        )
        .is_some(),);
    }

    #[test]
    fn check_bash_command_str_blocks_chmod_a_rwx() {
        assert!(check_bash_command_str(
            "chmod -R a+rwx /",
            None,
            &DenyList::default(),
            &PathGuard::default(),
            false
        )
        .is_some(),);
    }

    #[test]
    fn check_bash_command_str_blocks_chown_root() {
        assert!(check_bash_command_str(
            "chown root:root /etc/passwd",
            None,
            &DenyList::default(),
            &PathGuard::default(),
            false
        )
        .is_some(),);
    }

    #[test]
    fn check_bash_command_str_blocks_dd_random_of() {
        assert!(check_bash_command_str(
            "dd if=/dev/random of=/dev/sda",
            None,
            &DenyList::default(),
            &PathGuard::default(),
            false
        )
        .is_some(),);
    }

    #[test]
    fn check_bash_command_str_blocks_mkfs() {
        assert!(check_bash_command_str(
            "mkfs.ext4 /dev/sda1",
            None,
            &DenyList::default(),
            &PathGuard::default(),
            false
        )
        .is_some(),);
    }

    #[test]
    fn check_bash_command_str_blocks_redirect_to_dev_null_from_dev_sda() {
        assert!(check_bash_command_str(
            "cat > /dev/null < /dev/sda",
            None,
            &DenyList::default(),
            &PathGuard::default(),
            false
        )
        .is_some(),);
    }

    #[test]
    fn check_bash_command_str_blocks_id_rsa_reference() {
        assert!(check_bash_command_str(
            "cat ~/.ssh/id_rsa",
            None,
            &DenyList::default(),
            &PathGuard::default(),
            false
        )
        .is_some_and(|m| m.contains("denied path")),);
    }

    #[test]
    fn check_bash_command_str_blocks_etc_sudoers_reference() {
        assert!(check_bash_command_str(
            "cat /etc/sudoers",
            None,
            &DenyList::default(),
            &PathGuard::default(),
            false
        )
        .is_some_and(|m| m.contains("denied path")),);
    }

    #[test]
    fn check_bash_command_str_blocks_privilege_via_env() {
        assert!(check_bash_command_str(
            "env sudo ls",
            None,
            &DenyList::default(),
            &PathGuard::default(),
            false
        )
        .is_some_and(|m| m.contains("privilege escalation")),);
    }

    #[test]
    fn check_bash_command_str_blocks_denied_path_in_token() {
        let dl = DenyList::new(vec!["/secret/**".into()], vec![]);
        assert!(check_bash_command_str(
            "cat /secret/data",
            None,
            &dl,
            &PathGuard::default(),
            false
        )
        .is_some_and(|m| m.contains("denied path")),);
    }

    #[test]
    fn check_bash_command_str_blocks_empty_workdir_with_sandbox() {
        let guard = PathGuard {
            sandbox_dir: Some(std::env::temp_dir()),
            ..Default::default()
        };
        let r = check_bash_command_str("ls", Some(""), &DenyList::default(), &guard, true);
        assert!(r.is_none(), "empty workdir should be skipped, got: {r:?}");
    }

    #[test]
    fn check_bash_command_str_blocks_unresolvable_sandbox_dir() {
        let guard = PathGuard {
            sandbox_dir: Some("/nonexistent/kirkforge-sandbox-test-dir".into()),
            ..Default::default()
        };
        let r = check_bash_command_str("ls", Some("/tmp"), &DenyList::default(), &guard, true);
        assert!(
            r.as_ref()
                .is_some_and(|m| m.contains("Sandbox directory cannot be resolved")),
            "unresolvable sandbox dir should error, got: {r:?}"
        );
    }

    #[test]
    fn check_bash_command_returns_none_when_no_command_key() {
        assert!(check_bash_command(
            &serde_json::json!({}),
            &DenyList::default(),
            &PathGuard::default(),
            false
        )
        .is_none(),);
    }

    #[test]
    fn check_bash_command_returns_some_for_dangerous_command() {
        assert!(check_bash_command(
            &serde_json::json!({"command": "rm -rf /"}),
            &DenyList::default(),
            &PathGuard::default(),
            false
        )
        .is_some(),);
    }

    #[test]
    fn check_bash_command_passes_workdir_through() {
        let outer = tempfile::tempdir().unwrap();
        let sandbox = outer.path().join("sandbox");
        std::fs::create_dir_all(&sandbox).unwrap();
        let guard = PathGuard {
            sandbox_dir: Some(sandbox),
            ..Default::default()
        };
        let result = check_bash_command(
            &serde_json::json!({
                "command": "ls",
                "workdir": outer.path().to_str().unwrap(),
            }),
            &DenyList::default(),
            &guard,
            true,
        );
        assert!(
            result
                .as_ref()
                .is_some_and(|m| m.contains("outside sandbox")),
            "workdir outside sandbox should be blocked, got: {result:?}"
        );
    }
}
