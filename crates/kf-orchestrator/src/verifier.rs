//! R7 (WO 32.19) — security scanner for the orchestrator's verify cycle.
//!
//! The `kf-orchestrator` crate is a library and cannot depend on the
//! binary's `src/session/verifier/security_emitter.rs`. This module ports
//! the 14 regex rules as a crate-local verify step so the correction loop
//! can populate `packet.verification.security` after each delegate turn.
//!
//! `ponytail:` the 14 regexes are duplicated from the binary's
//! `security_emitter.rs` (WO 29.2). The binary's copy is canonical for
//! the executor's verifier bus; this copy is the orchestrator-crate-local
//! verify step. Unifying them requires extracting a `kf-security` crate
//! (a larger refactor) or making kf-orchestrator depend on the binary
//! (a dep cycle). Both are out of scope for R7 (wiring, not
//! restructuring). Ceiling: two copies of 14 regexes drift risk; upgrade
//! path: extract `kf-security` crate when a third consumer appears.

use std::io::Read;
use std::path::PathBuf;
use std::sync::OnceLock;

use crate::routing::correction::{OverallVerdict, ReducedStatePacket, SecurityState};
use regex::Regex;

/// Cap on bytes read per scanned file (1 MiB). Delegate-written artifacts
/// are far under this; the cap bounds memory on pathological inputs
/// (mm-H32 / WO 47.33).
/// ponytail: files larger than the cap are scanned as a truncated prefix —
/// a pattern straddling the cap boundary is missed, and a multi-byte char
/// cut at the boundary makes the whole file unscannable (read_to_string
/// InvalidData → skipped). Upgrade path: chunked streaming scan if
/// artifacts ever legitimately exceed the cap.
const MAX_SCAN_BYTES: u64 = 1024 * 1024;

#[derive(Debug, Clone)]
pub struct SecurityFinding {
    pub rule_id: &'static str,
    pub file: PathBuf,
    pub line: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Lang {
    Js,
    Py,
}

struct SecurityRule {
    re: Regex,
    rule_id: &'static str,
    lang: Lang,
}

const RULE_SPEC: &[(&str, &str, Lang)] = &[
    (
        r#"\[\s*['\"]eval['\"]\s*\]\s*\("#,
        "no-bracket-eval",
        Lang::Js,
    ),
    (
        r#"\[\s*['\"]Function['\"]\s*\]"#,
        "no-bracket-function",
        Lang::Js,
    ),
    (
        r#"child_process\s*\[\s*['\"](?:exec|execSync|spawn|spawnSync|fork)['\"]\s*\]\s*\("#,
        "no-bracket-shell-exec",
        Lang::Js,
    ),
    (
        r"child_process\s*\.\s*(?:exec|execSync)\s*\(",
        "no-shell-exec-concat",
        Lang::Js,
    ),
    (
        r#"require\s*\(\s*['\"]child_process['\"]\s*\)\s*\.\s*(?:exec|execSync|spawn|spawnSync)\s*\("#,
        "no-required-shell-exec",
        Lang::Js,
    ),
    (
        r"\bvm\s*\.\s*(?:runInContext|runInNewContext|compileFunction)\s*\(",
        "no-vm-codegen",
        Lang::Js,
    ),
    (
        r"Reflect\s*\.\s*(?:apply|construct)\s*\(\s*eval\b",
        "no-reflect-eval",
        Lang::Js,
    ),
    (r"\beval\s*\(", "py-eval", Lang::Py),
    (r"\bexec\s*\(", "py-exec", Lang::Py),
    (
        r"\bos\s*\.\s*(?:system|popen)\s*\(",
        "py-os-system",
        Lang::Py,
    ),
    (
        r"subprocess\s*\.\s*(?:Popen|call|run|check_output|check_call)\s*\([^)]*?shell\s*=\s*True",
        "py-subprocess-shell",
        Lang::Py,
    ),
    (
        r#"(?:__builtins__\s*\[\s*['\"]eval['\"]|getattr\s*\(\s*__builtins__\s*,\s*['\"]eval['\"])"#,
        "py-builtin-eval-alias",
        Lang::Py,
    ),
    (r"\bpickle\s*\.\s*loads?\s*\(", "py-pickle-load", Lang::Py),
    (r"\byaml\s*\.\s*load\s*\(", "py-yaml-load", Lang::Py),
];

fn rules() -> &'static [SecurityRule] {
    static RULES: OnceLock<Vec<SecurityRule>> = OnceLock::new();
    RULES.get_or_init(|| {
        RULE_SPEC
            .iter()
            .map(|(pat, id, lang)| SecurityRule {
                re: Regex::new(pat).expect("invalid security regex"),
                rule_id: id,
                lang: *lang,
            })
            .collect()
    })
}

const JS_EXTS: &[&str] = &["ts", "tsx", "mjs", "cjs", "js", "jsx", "mts", "cts"];

fn block_comment_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"/\*[\s\S]*?\*/").expect("valid block-comment regex"))
}

fn line_comment_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"//[^\n]*").expect("valid line-comment regex"))
}

fn py_comment_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?m)^\s*#[^\n]*").expect("valid py-comment regex"))
}

fn strip_comments(src: &str, lang: Lang) -> String {
    let out = block_comment_re().replace_all(src, "");
    let out = line_comment_re().replace_all(&out, "");
    if lang == Lang::Py {
        py_comment_re().replace_all(&out, "").into_owned()
    } else {
        out.into_owned()
    }
}

fn line_of(src: &str, byte_idx: usize) -> u32 {
    let mut line = 1u32;
    for (i, b) in src.bytes().enumerate() {
        if i >= byte_idx {
            break;
        }
        if b == b'\n' {
            line += 1;
        }
    }
    line
}

/// Scan `files` for the 14 dangerous-call patterns. Unreadable or
/// non-JS/Py files are skipped. Returns one `SecurityFinding` per match.
pub fn scan_files(files: &[PathBuf]) -> Vec<SecurityFinding> {
    let mut out = Vec::new();
    for file in files {
        let ext = match file.extension().and_then(|e| e.to_str()) {
            Some(e) => e,
            None => continue,
        };
        let lang = if JS_EXTS.contains(&ext) {
            Lang::Js
        } else if ext == "py" {
            Lang::Py
        } else {
            continue;
        };
        // WO 47.33 (mm-H32): read at most MAX_SCAN_BYTES — never buffer an
        // unbounded file. Read errors (unreadable, non-UTF-8, or a char cut
        // at the cap boundary) skip the file, as before.
        let src = match std::fs::File::open(file) {
            Ok(f) => {
                let mut s = String::new();
                match std::io::BufReader::new(f)
                    .take(MAX_SCAN_BYTES)
                    .read_to_string(&mut s)
                {
                    Ok(_) => s,
                    Err(_) => continue,
                }
            }
            Err(_) => continue,
        };
        let clean = strip_comments(&src, lang);
        for rule in rules() {
            if rule.lang != lang {
                continue;
            }
            for m in rule.re.find_iter(&clean) {
                out.push(SecurityFinding {
                    rule_id: rule.rule_id,
                    file: file.clone(),
                    line: line_of(&clean, m.start()),
                });
            }
        }
    }
    out
}

/// Populate a `ReducedStatePacket`'s `verification.security` from the
/// findings. All 14 rules are arbitrary-code-execution patterns → all
/// map to `critical`. Sets `overall` to `Fail` when any finding exists.
pub fn apply_security_findings(packet: &mut ReducedStatePacket, findings: &[SecurityFinding]) {
    packet.verification.security = SecurityState {
        findings: findings.len() as i64,
        critical: findings.len() as i64,
        high: 0,
    };
    if !findings.is_empty() {
        packet.verification.overall = OverallVerdict::Fail;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_tmp(name: &str, body: &str) -> PathBuf {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join(name);
        std::fs::write(&path, body).expect("write fixture");
        std::mem::forget(dir);
        path
    }

    #[test]
    fn scans_python_eval() {
        let path = write_tmp("a.py", "eval('evil')\n");
        let findings = scan_files(&[path]);
        assert!(findings.iter().any(|f| f.rule_id == "py-eval"));
    }

    #[test]
    fn scans_js_bracket_eval() {
        let path = write_tmp("a.ts", "window[\"eval\"](\"x\")\n");
        let findings = scan_files(&[path]);
        assert!(findings.iter().any(|f| f.rule_id == "no-bracket-eval"));
    }

    #[test]
    fn clean_file_produces_no_findings() {
        let path = write_tmp("a.ts", "export const x = 1;\n");
        assert!(scan_files(&[path]).is_empty());
    }

    #[test]
    fn skips_non_scannable_extensions() {
        let path = write_tmp("notes.md", "eval('evil')\n");
        assert!(scan_files(&[path]).is_empty());
    }

    #[test]
    fn ignores_patterns_in_comments() {
        let path = write_tmp("a.ts", "// window[\"eval\"](\"x\")\nexport const x = 1;\n");
        assert!(scan_files(&[path]).is_empty());
    }

    #[test]
    fn rule_table_has_fourteen_rules() {
        assert_eq!(RULE_SPEC.len(), 14);
    }

    // WO 47.33 (mm-H32): files over the scan cap are scanned as a
    // truncated prefix — the pattern before the cap is still found.
    #[test]
    fn oversized_file_scans_prefix() {
        let mut body = String::from("eval('early')\n# ");
        body.push_str(&"x".repeat(MAX_SCAN_BYTES as usize + 512));
        body.push('\n');
        let path = write_tmp("big.py", &body);
        let findings = scan_files(&[path]);
        assert!(
            findings.iter().any(|f| f.rule_id == "py-eval"),
            "pattern before the cap must still be found"
        );
    }

    // Pins the documented ceiling: a pattern that only appears past the
    // cap is NOT reported. If this test fails after an intentional
    // upgrade to a streaming scan, delete it with the ceiling comment.
    #[test]
    fn pattern_beyond_scan_cap_is_not_reported() {
        let mut body = String::from("# ");
        body.push_str(&"x".repeat(MAX_SCAN_BYTES as usize + 256));
        body.push_str("\neval('late')\n");
        let path = write_tmp("late.py", &body);
        let findings = scan_files(&[path]);
        assert!(
            findings.iter().all(|f| f.rule_id != "py-eval"),
            "pattern past the cap is outside the scanned prefix (documented ceiling)"
        );
    }

    #[test]
    fn apply_findings_populates_critical_and_sets_fail() {
        let mut packet = ReducedStatePacket::default();
        let path = write_tmp("a.py", "eval('x')\n");
        let findings = scan_files(&[path]);
        assert!(!findings.is_empty());
        apply_security_findings(&mut packet, &findings);
        assert_eq!(packet.verification.security.findings, findings.len() as i64);
        assert_eq!(packet.verification.security.critical, findings.len() as i64);
        assert_eq!(packet.verification.security.high, 0);
        assert_eq!(packet.verification.overall, OverallVerdict::Fail);
    }

    #[test]
    fn apply_no_findings_leaves_packet_clean() {
        let mut packet = ReducedStatePacket {
            verification: crate::routing::correction::Verification {
                overall: OverallVerdict::Pass,
                ..Default::default()
            },
            ..Default::default()
        };
        apply_security_findings(&mut packet, &[]);
        assert_eq!(packet.verification.security.findings, 0);
        assert_eq!(packet.verification.security.critical, 0);
        assert_eq!(packet.verification.overall, OverallVerdict::Pass);
    }
}
