//! Rust-native security emitter — ports the 14 regex rules from the former
//! TS orchestrator's `security-emitter.ts` (deleted in WO 29.9).
//!
//! WO 29.2: replaces the `bridge-emitter.ts` Node subprocess. The
//! verifier bus calls [`emit_security_findings`] directly and gets typed
//! [`VerdictEntry`]s — no subprocess, no NDJSON round-trip. This is the
//! last Rust→TS call path, now eliminated.
//!
//! `ponytail:` token/regex dangerous-call scanner, not tree-sitter or
//! semgrep/bandit. The obfuscated patterns here (bracket-keyed access,
//! string-concatenation shell exec, `vm.*`) evade the literal lint rules
//! (`no-eval`, `no-shell-exec`); this emitter closes that gap. semgrep/
//! bandit remain the upgrade path when available.

use std::path::PathBuf;
use std::sync::OnceLock;

use regex::Regex;

use crate::session::verifier::{Severity, VerdictEntry, VerifierSource};

/// Language a rule applies to. Every rule targets exactly one language,
/// so a single tag is smaller and safer than a `&[&str]` set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Lang {
    Js,
    Py,
}

/// Static rule spec. The compiled [`Regex`]es are built once via [`rules`].
struct SecurityRule {
    re: Regex,
    severity: Severity,
    rule_id: &'static str,
    message: &'static str,
    lang: Lang,
}

/// (pattern, severity, rule_id, message, lang). Ported verbatim from the
/// TS `RULES` array. All rules are critical or high → both map to
/// [`Severity::Error`] (mirrors `bridge-emitter.ts` `mapSeverity`).
const RULE_SPEC: &[(&str, Severity, &str, &str, Lang)] = &[
    // --- Obfuscated eval (bracket-keyed) — the lint `no-eval` misses these ---
    (
        r#"\[\s*['\"]eval['\"]\s*\]\s*\("#,
        Severity::Error,
        "no-bracket-eval",
        "Bracket-keyed eval (e.g. window['eval']) — obfuscated arbitrary code execution; use JSON.parse or a sandboxed VM",
        Lang::Js,
    ),
    (
        r#"\[\s*['\"]Function['\"]\s*\]"#,
        Severity::Error,
        "no-bracket-function",
        "Bracket-keyed Function constructor — string-to-code compilation via evasion",
        Lang::Js,
    ),
    // --- Obfuscated shell exec (bracket-keyed) — `no-shell-exec` misses these ---
    (
        r#"child_process\s*\[\s*['\"](?:exec|execSync|spawn|spawnSync|fork)['\"]\s*\]\s*\("#,
        Severity::Error,
        "no-bracket-shell-exec",
        "Bracket-keyed child_process exec/spawn — obfuscated shell execution; use execFile with a static command + args array",
        Lang::Js,
    ),
    // child_process.exec with any call (the lint rule only flags ${} interpolation;
    // string concatenation like 'ls ' + x evades it).
    (
        r"child_process\s*\.\s*(?:exec|execSync)\s*\(",
        Severity::Error,
        "no-shell-exec-concat",
        "child_process.exec spawns a shell — use execFile with a static command and args array to prevent injection (string concatenation evades the interpolation-only lint rule)",
        Lang::Js,
    ),
    (
        r#"require\s*\(\s*['\"]child_process['\"]\s*\)\s*\.\s*(?:exec|execSync|spawn|spawnSync)\s*\("#,
        Severity::Error,
        "no-required-shell-exec",
        "Inline-required child_process exec/spawn — use execFile with a static command and args array",
        Lang::Js,
    ),
    // --- vm code generation ---
    (
        r"\bvm\s*\.\s*(?:runInContext|runInNewContext|compileFunction)\s*\(",
        Severity::Error,
        "no-vm-codegen",
        "vm.runIn*/compileFunction executes arbitrary code — avoid compiling untrusted strings",
        Lang::Js,
    ),
    (
        r"Reflect\s*\.\s*(?:apply|construct)\s*\(\s*eval\b",
        Severity::Error,
        "no-reflect-eval",
        "Reflect.apply/construct(eval) — aliased arbitrary code execution",
        Lang::Js,
    ),
    // --- Python ---
    (
        r"\beval\s*\(",
        Severity::Error,
        "py-eval",
        "Python eval() executes arbitrary code — use ast.literal_eval for data",
        Lang::Py,
    ),
    (
        r"\bexec\s*\(",
        Severity::Error,
        "py-exec",
        "Python exec() executes arbitrary code — restructure to avoid runtime code generation",
        Lang::Py,
    ),
    (
        r"\bos\s*\.\s*(?:system|popen)\s*\(",
        Severity::Error,
        "py-os-system",
        "os.system/os.popen spawns a shell — use subprocess with a static arg list",
        Lang::Py,
    ),
    (
        r"subprocess\s*\.\s*(?:Popen|call|run|check_output|check_call)\s*\([^)]*?shell\s*=\s*True",
        Severity::Error,
        "py-subprocess-shell",
        "subprocess with shell=True is shell injection — pass a static arg list with shell=False",
        Lang::Py,
    ),
    (
        r#"(?:__builtins__\s*\[\s*['\"]eval['\"]|getattr\s*\(\s*__builtins__\s*,\s*['\"]eval['\"])"#,
        Severity::Error,
        "py-builtin-eval-alias",
        "Obfuscated Python eval via __builtins__ — arbitrary code execution",
        Lang::Py,
    ),
    (
        r"\bpickle\s*\.\s*loads?\s*\(",
        Severity::Error,
        "py-pickle-load",
        "pickle.loads executes arbitrary code on untrusted input — use JSON or a safe format",
        Lang::Py,
    ),
    (
        r"\byaml\s*\.\s*load\s*\(",
        Severity::Error,
        "py-yaml-load",
        "yaml.load is unsafe — use yaml.safe_load",
        Lang::Py,
    ),
];

/// Compile the rule table once and cache it for the process lifetime.
fn rules() -> &'static [SecurityRule] {
    static RULES: OnceLock<Vec<SecurityRule>> = OnceLock::new();
    RULES.get_or_init(|| {
        RULE_SPEC
            .iter()
            .map(|(pat, sev, id, msg, lang)| SecurityRule {
                re: Regex::new(pat).expect("invalid security regex"),
                severity: *sev,
                rule_id: id,
                message: msg,
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

/// Strip comments from `src`. Mirrors the TS `stripComments`:
/// block `/* */` and line `//` are stripped for all languages (the `//`
/// strip is harmless for Python floor-division); `#` is stripped only for
/// Python. Newlines inside block comments are consumed (line numbers after
/// a block comment shift) — this matches the TS behaviour exactly.
///
/// `ponytail:` strips comments only (NOT strings) — the obfuscated
/// patterns are string-keyed, so stripping strings would erase the very
/// signal we scan for.
fn strip_comments(src: &str, lang: Lang) -> String {
    let out = block_comment_re().replace_all(src, "");
    let out = line_comment_re().replace_all(&out, "");
    if lang == Lang::Py {
        py_comment_re().replace_all(&out, "").into_owned()
    } else {
        out.into_owned()
    }
}

/// 1-based line number of `byte_idx` within `src` (counts `\n` before it).
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

/// Scan `files` for the 14 dangerous-call patterns and return one
/// [`VerdictEntry`] per match. Unreadable or non-JS/Py files are skipped.
/// Findings are tagged [`VerifierSource::Custom`]`("ts:security")` for
/// wire parity with the old NDJSON bridge (so downstream consumers see
/// the same source label).
pub fn emit_security_findings(files: &[PathBuf]) -> Vec<VerdictEntry> {
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
        let src = match std::fs::read_to_string(file) {
            Ok(s) => s,
            Err(_) => continue,
        };
        let clean = strip_comments(&src, lang);
        for rule in rules() {
            if rule.lang != lang {
                continue;
            }
            for m in rule.re.find_iter(&clean) {
                out.push(VerdictEntry {
                    source: VerifierSource::Custom("ts:security".into()),
                    severity: rule.severity,
                    message: format!("[{}] {}", rule.rule_id, rule.message),
                    file: Some(file.clone()),
                    line: Some(line_of(&clean, m.start())),
                    fix: None,
                });
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_tmp(name: &str, body: &str) -> PathBuf {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join(name);
        std::fs::write(&path, body).expect("write fixture");
        // Leak the tempdir so the file survives the call (the security
        // emitter reads paths lazily). Tests are short-lived.
        std::mem::forget(dir);
        path
    }

    fn rule_ids(entries: &[VerdictEntry]) -> Vec<String> {
        entries
            .iter()
            .filter_map(|e| {
                e.message
                    .strip_prefix('[')
                    .and_then(|s| s.split(']').next())
                    .map(String::from)
            })
            .collect()
    }

    fn has_rule(entries: &[VerdictEntry], rule_id: &str) -> bool {
        rule_ids(entries).iter().any(|r| r == rule_id)
    }

    #[test]
    fn emits_nothing_for_empty_file_list() {
        let entries = emit_security_findings(&[]);
        assert!(entries.is_empty());
    }

    #[test]
    fn skips_non_scannable_extensions() {
        let path = write_tmp("notes.md", "eval('evil')\n");
        let entries = emit_security_findings(&[path]);
        assert!(entries.is_empty(), "non-JS/Py files are skipped");
    }

    #[test]
    fn skips_missing_files() {
        let entries =
            emit_security_findings(&[PathBuf::from("/nonexistent/definitely-not-here-xyz.ts")]);
        assert!(entries.is_empty());
    }

    // ── JS rules ──

    #[test]
    fn flags_bracket_keyed_eval() {
        let path = write_tmp(
            "a.ts",
            "const x = (window as any)[\"eval\"](\"alert(1)\");\n",
        );
        let entries = emit_security_findings(&[path]);
        assert!(
            has_rule(&entries, "no-bracket-eval"),
            "got: {:?}",
            rule_ids(&entries)
        );
        assert!(entries.iter().all(|e| e.severity == Severity::Error));
        assert!(entries
            .iter()
            .all(|e| matches!(e.source, VerifierSource::Custom(ref s) if s == "ts:security")));
    }

    #[test]
    fn flags_bracket_keyed_function() {
        let path = write_tmp("a.ts", "const F = obj[\"Function\"];\n");
        let entries = emit_security_findings(&[path]);
        assert!(has_rule(&entries, "no-bracket-function"));
    }

    #[test]
    fn flags_bracket_keyed_shell_exec() {
        let path = write_tmp(
            "a.ts",
            "import child_process from \"child_process\";\nchild_process[\"exec\"](\"rm -rf /tmp/x\");\n",
        );
        let entries = emit_security_findings(&[path]);
        assert!(
            has_rule(&entries, "no-bracket-shell-exec"),
            "got: {:?}",
            rule_ids(&entries)
        );
    }

    #[test]
    fn flags_concat_shell_exec() {
        let path = write_tmp(
            "a.ts",
            "import child_process from \"child_process\";\nchild_process.exec(\"ls \" + userInput);\n",
        );
        let entries = emit_security_findings(&[path]);
        assert!(has_rule(&entries, "no-shell-exec-concat"));
    }

    #[test]
    fn flags_required_shell_exec() {
        let path = write_tmp("a.ts", "require(\"child_process\").exec(cmd);\n");
        let entries = emit_security_findings(&[path]);
        assert!(has_rule(&entries, "no-required-shell-exec"));
    }

    #[test]
    fn flags_vm_codegen() {
        let path = write_tmp(
            "a.ts",
            "import vm from \"vm\";\nvm.runInNewContext(untrusted);\n",
        );
        let entries = emit_security_findings(&[path]);
        assert!(has_rule(&entries, "no-vm-codegen"));
    }

    #[test]
    fn flags_reflect_eval() {
        let path = write_tmp("a.ts", "Reflect.apply(eval, null, [\"1+1\"]);\n");
        let entries = emit_security_findings(&[path]);
        assert!(has_rule(&entries, "no-reflect-eval"));
    }

    // ── Python rules ──

    #[test]
    fn flags_python_eval_exec_os_subprocess_pickle() {
        let body = "import os, subprocess, pickle\n\
                    eval(\"1+1\")\n\
                    exec(\"code\")\n\
                    os.system(\"ls\")\n\
                    subprocess.run([\"echo\", x], shell=True)\n\
                    pickle.loads(blob)\n";
        let path = write_tmp("a.py", body);
        let entries = emit_security_findings(&[path]);
        let ids = rule_ids(&entries);
        assert!(ids.contains(&"py-eval".to_string()), "got: {ids:?}");
        assert!(ids.contains(&"py-exec".to_string()));
        assert!(ids.contains(&"py-os-system".to_string()));
        assert!(ids.contains(&"py-subprocess-shell".to_string()));
        assert!(ids.contains(&"py-pickle-load".to_string()));
        assert!(entries.len() >= 5);
    }

    #[test]
    fn flags_python_yaml_load() {
        let path = write_tmp("a.py", "cfg = yaml.load(stream)\n");
        let entries = emit_security_findings(&[path]);
        assert!(has_rule(&entries, "py-yaml-load"));
    }

    #[test]
    fn flags_python_builtin_eval_alias() {
        let path = write_tmp("a.py", "x = __builtins__[\"eval\"](\"1\")\n");
        let entries = emit_security_findings(&[path]);
        assert!(has_rule(&entries, "py-builtin-eval-alias"));
    }

    #[test]
    fn flags_python_getattr_builtin_eval_alias() {
        let path = write_tmp("a.py", "x = getattr(__builtins__, \"eval\")(\"1\")\n");
        let entries = emit_security_findings(&[path]);
        assert!(has_rule(&entries, "py-builtin-eval-alias"));
    }

    // ── Severity + shape ──

    #[test]
    fn all_findings_are_errors_with_ts_security_source() {
        let path = write_tmp("a.ts", "window[\"eval\"](\"x\");\n");
        let entries = emit_security_findings(&[path.clone()]);
        assert!(!entries.is_empty());
        for e in &entries {
            assert_eq!(e.severity, Severity::Error);
            assert!(matches!(e.source, VerifierSource::Custom(ref s) if s == "ts:security"));
            assert!(e.message.starts_with("[no-bracket-eval]"));
            assert_eq!(e.file.as_deref(), Some(path.as_path()));
            assert!(e.line.is_some());
        }
    }

    #[test]
    fn line_number_points_at_the_offending_line() {
        // No comments above it; the `eval(` is on line 2.
        let path = write_tmp("a.py", "x = 1\neval(\"boom\")\n");
        let entries = emit_security_findings(&[path]);
        let ev = entries
            .iter()
            .find(|e| e.message.contains("[py-eval]"))
            .expect("py-eval should fire");
        assert_eq!(ev.line, Some(2), "eval on line 2 → line 2");
    }

    // ── Comment stripping (parity with TS) ──

    #[test]
    fn clean_code_emits_no_findings() {
        let path = write_tmp(
            "a.ts",
            "export const add = (a: number, b: number) => a + b;\n",
        );
        let entries = emit_security_findings(&[path]);
        assert!(entries.is_empty(), "got: {entries:?}");
    }

    #[test]
    fn ignores_patterns_inside_line_comments() {
        // Mirrors the TS `does not flag dangerous patterns inside comments`.
        let path = write_tmp(
            "a.ts",
            "// don't use window[\"eval\"] or child_process[\"exec\"] here\nexport const x = 1;\n",
        );
        let entries = emit_security_findings(&[path]);
        assert!(
            entries.is_empty(),
            "commented patterns must not fire: {entries:?}"
        );
    }

    #[test]
    fn ignores_patterns_inside_python_hash_comments() {
        let path = write_tmp("a.py", "# eval(\"x\") is bad\nx = 1\n");
        let entries = emit_security_findings(&[path]);
        assert!(
            entries.is_empty(),
            "commented py patterns must not fire: {entries:?}"
        );
    }

    #[test]
    fn ignores_patterns_inside_block_comments() {
        let path = write_tmp(
            "a.ts",
            "/* window[\"eval\"](\"x\") */\nexport const x = 1;\n",
        );
        let entries = emit_security_findings(&[path]);
        assert!(
            entries.is_empty(),
            "block-comment patterns must not fire: {entries:?}"
        );
    }

    // ── Unit tests for the helpers ──

    #[test]
    fn strip_comments_removes_block_and_line_forms() {
        let cleaned = strip_comments("/* block */ code // trailing\nlet x = 1;", Lang::Js);
        assert!(!cleaned.contains("block"));
        assert!(!cleaned.contains("trailing"));
        assert!(cleaned.contains("code"));
        assert!(cleaned.contains("let x = 1;"));
    }

    #[test]
    fn strip_comments_removes_python_hash_comments_only_for_py() {
        let py = strip_comments("# comment\nx = 1\n", Lang::Py);
        assert!(!py.contains("comment"));
        // JS files keep `#` (e.g. shebangs in .ts are rare but `#` is not a JS comment).
        let js = strip_comments("# not-a-comment\nlet x = 1;\n", Lang::Js);
        assert!(js.contains("# not-a-comment"));
    }

    #[test]
    fn line_of_counts_newlines_one_based() {
        assert_eq!(line_of("abc", 0), 1);
        assert_eq!(line_of("abc\ndef", 4), 2);
        assert_eq!(line_of("a\nb\nc", 4), 3);
    }

    #[test]
    fn rule_table_has_fourteen_rules() {
        assert_eq!(RULE_SPEC.len(), 14, "WO 29.2 requires all 14 rules");
    }

    #[test]
    fn all_rule_regexes_compile() {
        // `rules()` panics if any pattern is invalid; touching it here gives
        // a focused failure point instead of a lazy init mid-scan.
        assert_eq!(rules().len(), 14);
    }
}
