//! Compiled-in Rust implementations of the `kf-plugin` tools (WO 29.1).
//!
//! Replaces the former `plugins/kf-plugin/tools/*.sh` shell wrappers (deleted
//! in WO 29.9) that each `exec node $CLI <cmd>`. `doctor`, `health`,
//! `tools`, `verify`, and `audit-verify` run fully natively here (WO 35.6
//! de-stubbed `verify` — security emitter via `kf_orchestrator::verifier` —
//! and `audit-verify` — JSONL walker over the WO 29.4 hash chain).
//! `verify-workspace` remains an honest deferral: assembling a
//! `ReducedStatePacket` needs the un-ported reducer.
//! The orchestrator's `ModelClient` now has a production impl
//! (`session::executor_adapter::ExecutorAdapter`, WO 35.6), but the verify
//! commands are deterministic by design and do not call it.
//!
//! Enabled by the `kf-plugin-tools` cargo feature (default on). When the
//! feature is off, no `kf-plugin` tools are registered — the shell/Node
//! fallback was removed in WO 29.9 when the TS tree was deleted.

use crate::shared::{ToolDef, ToolOutcome};
use crate::tools::{Tool, ToolContext};
use serde_json::json;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

// Names mirror the shell-plugin manifest (`plugins/kf-plugin/kf-code.toml`)
// so the compiled-in tools are exact drop-ins for the shell wrappers.

fn tool_def(name: &'static str, description: &'static str, params: serde_json::Value) -> ToolDef {
    ToolDef {
        name,
        description,
        parameters: params,
    }
}

// ── doctor: probe external linters on PATH ──────────────────────────────

struct ToolCap {
    name: &'static str,
    available: bool,
    version: Option<String>,
    source: &'static str,
}

/// Spawn `<name> --version` and treat a successful exit as available.
/// Mirrors `probeTool` from the former TS plugin SDK (deleted in WO 29.9).
/// ponytail: sequential probes — doctor is an on-demand diagnostic, not
/// hot-path; 5 × ~50ms is acceptable and avoids a parallel-join fan-out.
async fn probe_tool(name: &'static str) -> ToolCap {
    let result = tokio::process::Command::new(name)
        .arg("--version")
        .kill_on_drop(true)
        .output();
    let cap = match tokio::time::timeout(Duration::from_millis(5000), result).await {
        Ok(Ok(o)) if o.status.success() => {
            let stdout = String::from_utf8_lossy(&o.stdout);
            let first_line = stdout.lines().next().unwrap_or("").trim();
            let version = if first_line.is_empty() {
                None
            } else {
                Some(first_line.to_string())
            };
            (true, version)
        }
        _ => (false, None),
    };
    ToolCap {
        name,
        available: cap.0,
        version: cap.1,
        source: "external",
    }
}

/// Derive the supported-language list from which linters are present.
/// Mirrors the TS `doctor()` language derivation.
fn derive_languages(caps: &[ToolCap]) -> Vec<&'static str> {
    let has = |n: &str| caps.iter().any(|c| c.name == n && c.available);
    let mut langs = Vec::new();
    if has("eslint") || has("tsc") {
        langs.push("typescript");
        langs.push("javascript");
    }
    if has("ruff") || has("pyright") {
        langs.push("python");
    }
    langs.push("shell (advisory only)");
    langs.push("cpp (validator required)");
    langs.push("c (validator required)");
    langs.push("rust (validator required)");
    langs.push("go (validator required)");
    langs.push("sql (validator required)");
    if langs.is_empty() {
        langs.push("unknown");
    }
    langs
}

struct PluginDoctor {
    def: ToolDef,
}

#[async_trait::async_trait]
impl Tool for PluginDoctor {
    fn def(&self) -> ToolDef {
        self.def.clone()
    }

    async fn run(&self, _ctx: &ToolContext, args: serde_json::Value) -> ToolOutcome {
        let pretty = args
            .get("pretty")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let caps = vec![
            probe_tool("eslint").await,
            probe_tool("tsc").await,
            probe_tool("ruff").await,
            probe_tool("pyright").await,
            probe_tool("bandit").await,
        ];
        let languages = derive_languages(&caps);

        if pretty {
            let mut out = String::from("\n--- Tool Capability Report ---\n");
            for c in &caps {
                let status = match (c.available, &c.version) {
                    (true, Some(v)) => format!("available ({v}) [external]"),
                    (true, None) => "available (bundled) [external]".to_string(),
                    (false, _) => "not found [external]".to_string(),
                };
                let display = match c.name {
                    "tsc" => "TypeScript (tsc)",
                    other => other,
                };
                out.push_str(&format!("  {display}: {status}\n"));
            }
            out.push_str("  SecDev: available [internal] -- regex-based security scanner\n");
            out.push_str(&format!("  Languages: {}\n", languages.join(", ")));
            ToolOutcome::Success { content: out }
        } else {
            let mut tools = serde_json::Map::new();
            for c in &caps {
                tools.insert(
                    c.name.to_string(),
                    json!({
                        "available": c.available,
                        "version": c.version,
                        "source": c.source,
                    }),
                );
            }
            ToolOutcome::Success {
                content: serde_json::to_string_pretty(&json!({
                    "eslint": tools.get("eslint"),
                    "tsc": tools.get("tsc"),
                    "ruff": tools.get("ruff"),
                    "pyright": tools.get("pyright"),
                    "bandit": tools.get("bandit"),
                    "secdev": {
                        "available": true,
                        "source": "internal",
                        "note": "Regex-based security scanner (advisory for C/C++/Go/Rust/SQL).",
                    },
                    "languages": languages,
                }))
                .unwrap_or_else(|_| "{}".to_string()),
            }
        }
    }
}

// ── health: trivial status ──────────────────────────────────────────────

struct PluginHealth {
    def: ToolDef,
}

#[async_trait::async_trait]
impl Tool for PluginHealth {
    fn def(&self) -> ToolDef {
        self.def.clone()
    }

    async fn run(&self, _ctx: &ToolContext, _args: serde_json::Value) -> ToolOutcome {
        // The TS version bootstrapped the Node orchestrator for SLO stats.
        // The Rust runtime wires the orchestrator crate's ModelClient to
        // the executor adapter (WO 35.6), but SLO stats are still
        // unported — report the folded-tool path as healthy and flag it.
        ToolOutcome::Success {
            content: "Status:         ok\n\
                      Tools:          native (compiled-in, kf-plugin-tools feature)\n\
                      Orchestrator:   crate present (WO 29.7); ModelClient wired to the\n\
                      \x20executor adapter (WO 35.6); SLO stats pending\n"
                .to_string(),
        }
    }
}

// ── tools: static lint-engine list ──────────────────────────────────────

struct PluginToolsList {
    def: ToolDef,
}

#[async_trait::async_trait]
impl Tool for PluginToolsList {
    fn def(&self) -> ToolDef {
        self.def.clone()
    }

    async fn run(&self, _ctx: &ToolContext, _args: serde_json::Value) -> ToolOutcome {
        // Ported verbatim from the former TS CLI `tools` command (deleted WO 29.9).
        ToolOutcome::Success {
            content: "KirkForge Native Lint Engines (internal, always available):\n\
                      \x20 JS/TS:  tool-lint-ts (29 rules)\n\
                      \x20 Python: tool-lint-py (34 rules)\n\
                      \x20 Shell:  tool-lint-sh (9 rules)\n\
                      \x20 C/C++:  tool-lint-c (10 rules)\n\
                      \x20 Rust:   tool-lint-rs (8 rules)\n\
                      \x20 Go:     tool-lint-go (7 rules)\n\
                      \x20 SQL:    tool-lint-sql (6 rules)\n\
                      \n\
                      Type Checkers (external, required on PATH):\n\
                      \x20 JS/TS:  tsc\n\
                      \x20 Python: pyright\n"
                .to_string(),
        }
    }
}

// ── verify: deterministic security emitter (WO 35.6 de-stub) ────────────

use kf_orchestrator::verifier::{scan_files, SecurityFinding};

// ponytail: capped walk — verify is an on-demand diagnostic; 2000 files
// bounds the regex scan on large repos. Raise or make configurable when
// someone verifies a monorepo and hits the cap.
const VERIFY_MAX_FILES: usize = 2000;

const SCANNABLE_EXTS: &[&str] = &["ts", "tsx", "mjs", "cjs", "js", "jsx", "mts", "cts", "py"];

fn collect_scannable_files(root: &std::path::Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for entry in ignore::WalkBuilder::new(root)
        .hidden(true)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .build()
        .flatten()
    {
        if !entry.file_type().is_some_and(|ft| ft.is_file()) {
            continue;
        }
        let scannable = entry
            .path()
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|ext| SCANNABLE_EXTS.contains(&ext));
        if scannable {
            out.push(entry.into_path());
            if out.len() >= VERIFY_MAX_FILES {
                break;
            }
        }
    }
    out
}

fn render_verify(files: &[PathBuf], findings: &[SecurityFinding], pretty: bool) -> String {
    if !pretty {
        return serde_json::to_string_pretty(&json!({
            "security": {
                "status": if findings.is_empty() { "pass" } else { "fail" },
                "files_scanned": files.len(),
                "findings": findings.iter().map(|f| json!({
                    "rule": f.rule_id,
                    "file": f.file.display().to_string(),
                    "line": f.line,
                })).collect::<Vec<_>>(),
            },
            "lint": "emitter not ported",
            "type": "emitter not ported",
            "graph": "emitter not ported",
            "overall": if findings.is_empty() { "pass (security-only coverage)" } else { "fail" },
        }))
        .unwrap_or_else(|_| "{}".to_string());
    }
    let mut out = format!(
        "verify: security scan complete ({} files scanned)\n",
        files.len()
    );
    out.push_str(&format!(
        "  security: {}\n",
        if findings.is_empty() {
            "PASS (0 findings)".to_string()
        } else {
            format!("FAIL ({} findings)", findings.len())
        }
    ));
    for f in findings.iter().take(50) {
        out.push_str(&format!(
            "    {} {}:{}\n",
            f.rule_id,
            f.file.display(),
            f.line
        ));
    }
    if findings.len() > 50 {
        out.push_str(&format!("    ... and {} more\n", findings.len() - 50));
    }
    out.push_str("  lint:  emitter not ported (reducer + lint emitters pending)\n");
    out.push_str("  type:  emitter not ported\n");
    out.push_str("  graph: emitter not ported\n");
    out.push_str(&format!(
        "  overall: {}\n",
        if findings.is_empty() {
            "PASS (security-only coverage)"
        } else {
            "FAIL"
        }
    ));
    out
}

struct PluginVerify {
    def: ToolDef,
}

#[async_trait::async_trait]
impl Tool for PluginVerify {
    fn def(&self) -> ToolDef {
        self.def.clone()
    }

    async fn run(&self, _ctx: &ToolContext, args: serde_json::Value) -> ToolOutcome {
        // `task` exists for verifier language routing in the TS pipeline;
        // the security emitter scans by extension, so it is accepted and
        // unused here.
        let pretty = !args.get("json").and_then(|v| v.as_bool()).unwrap_or(false);
        let root = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let files = collect_scannable_files(&root);
        let findings = scan_files(&files);
        ToolOutcome::Success {
            content: render_verify(&files, &findings, pretty),
        }
    }
}

// ── verify-workspace: honest deferral (reducer not ported) ──────────────

fn deferred_message(cmd: &str, remaining: &str) -> String {
    format!(
        "{cmd}: not implemented (reducer not ported).\n\
         Assembling the ReducedStatePacket this command promises requires the\n\
         \x20deterministic reducer from `orchestrator/src/reducer.ts`, which is\n\
         \x20not ported to kf-orchestrator yet.\n\
         \x20Remaining work: {remaining}"
    )
}

struct PluginVerifyWorkspace {
    def: ToolDef,
}

#[async_trait::async_trait]
impl Tool for PluginVerifyWorkspace {
    fn def(&self) -> ToolDef {
        self.def.clone()
    }

    async fn run(&self, _ctx: &ToolContext, _args: serde_json::Value) -> ToolOutcome {
        ToolOutcome::Success {
            content: deferred_message(
                "verify-workspace",
                "port the reducer + ReducedStatePacket assembly to Rust, then wire\n\
                 \x20the workspace walk onto it.",
            ),
        }
    }
}

// ── audit-verify: JSONL hash-chain walker (WO 35.6 de-stub) ─────────────

use crate::shared::audit::{chain_hash_of, initial_hash, AuditEvent};

// Replay the chain in `path` from genesis. Ok((events, None)) = intact;
// Ok((events_before_break, Some(sequence))) = broken at that event.
fn verify_audit_jsonl(
    path: &std::path::Path,
    hmac_key: Option<&str>,
) -> Result<(usize, Option<u64>), String> {
    let content = std::fs::read_to_string(path)
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let mut prev = initial_hash(hmac_key);
    let mut count = 0usize;
    for line in content.lines().filter(|l| !l.trim().is_empty()) {
        let event: AuditEvent = serde_json::from_str(line)
            .map_err(|e| format!("line {}: not an audit event: {e}", count + 1))?;
        let expected = chain_hash_of(&prev, &event, hmac_key);
        if event.chain_hash != expected {
            return Ok((count, Some(event.sequence)));
        }
        prev = event.chain_hash;
        count += 1;
    }
    Ok((count, None))
}

struct PluginAuditVerify {
    def: ToolDef,
}

#[async_trait::async_trait]
impl Tool for PluginAuditVerify {
    fn def(&self) -> ToolDef {
        self.def.clone()
    }

    async fn run(&self, _ctx: &ToolContext, args: serde_json::Value) -> ToolOutcome {
        let file = match args.get("file").and_then(|v| v.as_str()) {
            Some(f) if !f.trim().is_empty() => f,
            _ => {
                return ToolOutcome::Failure(crate::shared::ToolError::invalid_args(
                    "Missing or empty 'file' argument",
                ));
            }
        };
        let pretty = !args.get("json").and_then(|v| v.as_bool()).unwrap_or(false);
        let hmac_key = args.get("hmac_key").and_then(|v| v.as_str());
        let result = verify_audit_jsonl(std::path::Path::new(file), hmac_key);
        let content = match result {
            Ok((events, None)) if pretty => format!("OK: {events} events, chain intact\n"),
            Ok((events, None)) => {
                serde_json::to_string_pretty(&json!({"status": "ok", "events": events}))
                    .unwrap_or_else(|_| "{}".into())
            }
            Ok((before, Some(seq))) if pretty => format!(
                "FAIL: chain broken at sequence {seq} ({before} events verified before the break)\n"
            ),
            Ok((before, Some(seq))) => serde_json::to_string_pretty(&json!({
                "status": "fail",
                "broken_at_sequence": seq,
                "events_verified": before,
            }))
            .unwrap_or_else(|_| "{}".into()),
            Err(e) if pretty => format!("ERROR: {e}\n"),
            Err(e) => serde_json::to_string_pretty(&json!({"status": "error", "error": e}))
                .unwrap_or_else(|_| "{}".into()),
        };
        ToolOutcome::Success { content }
    }
}

/// All six compiled-in `kf-plugin` tools, in manifest order.
/// Registered by `main/run_session.rs` when the feature is on and the
/// `kf-plugin` plugin is enabled (mirrors the stratum/budget pattern).
pub fn all_plugin_sdk_tools() -> Vec<Arc<dyn Tool>> {
    vec![
        Arc::new(PluginVerify {
            def: tool_def(
                "plugin_verify",
                "Run deterministic verification emitters without calling a model. Reports lint, type, security, graph, and overall status.",
                json!({
                    "type": "object",
                    "properties": {
                        "task": { "type": "string", "description": "Task description used only for verifier language routing" },
                        "json": { "type": "boolean", "description": "Emit JSON instead of human-readable text", "default": false }
                    }
                }),
            ),
        }) as Arc<dyn Tool>,
        Arc::new(PluginVerifyWorkspace {
            def: tool_def(
                "plugin_verify_workspace",
                "Run deterministic verification on a workspace directory and output a ReducedStatePacket.",
                json!({
                    "type": "object",
                    "properties": {
                        "workspace": { "type": "string", "description": "Path to the workspace directory" },
                        "file": { "anyOf": [
                            { "type": "string", "description": "A single file path to verify" },
                            { "type": "array", "items": { "type": "string" }, "description": "Multiple file paths to verify" }
                        ], "description": "Optional file path(s) to verify" },
                        "language": { "type": "string", "description": "Task language (typescript, javascript, python, etc.)" },
                        "description": { "type": "string", "description": "Task description for language profile detection" },
                        "taskId": { "type": "string", "description": "Task identifier for the verification run" }
                    },
                    "required": ["workspace"]
                }),
            ),
        }),
        Arc::new(PluginAuditVerify {
            def: tool_def(
                "plugin_audit_verify",
                "Verify the integrity of a KirkForge audit JSONL chain (checks sequential hashes).",
                json!({
                    "type": "object",
                    "properties": {
                        "file": { "type": "string", "description": "Path to audit JSONL file" },
                        "json": { "type": "boolean", "description": "Emit JSON instead of human-readable text", "default": false },
                        "hmac_key": { "type": "string", "description": "HMAC key the chain was sealed with (omit for plain SHA-256 chains)" }
                    },
                    "required": ["file"]
                }),
            ),
        }),
        Arc::new(PluginDoctor {
            def: tool_def(
                "plugin_doctor",
                "Probe local verification tools (ESLint, TypeScript, Ruff, Pyright, Bandit) and report capabilities.",
                json!({
                    "type": "object",
                    "properties": {
                        "pretty": { "type": "boolean", "description": "Human-readable output instead of JSON", "default": false }
                    }
                }),
            ),
        }),
        Arc::new(PluginHealth {
            def: tool_def(
                "plugin_health",
                "Show orchestrator health and SLO status.",
                json!({ "type": "object", "properties": {} }),
            ),
        }),
        Arc::new(PluginToolsList {
            def: tool_def(
                "plugin_tools",
                "List registered verification tools available in the KirkForge-Plugin SDK.",
                json!({ "type": "object", "properties": {} }),
            ),
        }),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn health_reports_wired_model_client() {
        let tool = PluginHealth {
            def: tool_def("plugin_health", "health", json!({})),
        };
        let out = tool.run(&ToolContext::new(), json!({})).await;
        match out {
            ToolOutcome::Success { content } => {
                assert!(content.contains("ok"), "health should report ok: {content}");
                assert!(
                    content.contains("WO 35.6"),
                    "should cite the wiring WO: {content}"
                );
                assert!(content.contains("SLO stats pending"));
            }
            other => panic!("expected Success, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn tools_lists_engines_and_checkers() {
        let tool = PluginToolsList {
            def: tool_def("plugin_tools", "tools", json!({})),
        };
        let out = tool.run(&ToolContext::new(), json!({})).await;
        match out {
            ToolOutcome::Success { content } => {
                assert!(content.contains("tool-lint-ts"));
                assert!(content.contains("tool-lint-py"));
                assert!(content.contains("pyright"));
                assert!(content.contains("tsc"));
            }
            other => panic!("expected Success, got {other:?}"),
        }
    }

    // WO 35.6: verify runs the real security emitter. The tool itself
    // scans the process cwd; the pure helper is tested against a
    // tempdir so the test does not depend on (or scan) the repo tree.
    #[test]
    fn verify_scan_finds_findings_in_tempdir() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("evil.py"), "eval('evil')\n").unwrap();
        std::fs::write(dir.path().join("clean.ts"), "export const x = 1;\n").unwrap();
        let files = collect_scannable_files(dir.path());
        assert_eq!(files.len(), 2, "both JS and Py files collected");
        let findings = scan_files(&files);
        assert!(findings.iter().any(|f| f.rule_id == "py-eval"));
        let text = render_verify(&files, &findings, true);
        assert!(text.contains("security: FAIL (1 findings)"), "{text}");
        assert!(text.contains("overall: FAIL"));
        assert!(text.contains("emitter not ported"));
    }

    #[test]
    fn verify_scan_clean_tempdir_passes_with_coverage_label() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("clean.py"), "print('hi')\n").unwrap();
        let files = collect_scannable_files(dir.path());
        let findings = scan_files(&files);
        assert!(findings.is_empty());
        let text = render_verify(&files, &findings, true);
        assert!(text.contains("security: PASS (0 findings)"), "{text}");
        assert!(text.contains("overall: PASS (security-only coverage)"));
        let as_json = render_verify(&files, &findings, false);
        assert!(as_json.contains("\"status\": \"pass\""), "{as_json}");
    }

    #[test]
    fn verify_scan_respects_gitignore() {
        let dir = tempfile::tempdir().unwrap();
        // The ignore crate honors .gitignore only inside a git repo
        // (require_git default), so give the tempdir one.
        std::process::Command::new("git")
            .arg("init")
            .arg("-q")
            .arg(dir.path())
            .status()
            .expect("git init");
        std::fs::write(dir.path().join(".gitignore"), "ignored.py\n").unwrap();
        std::fs::write(dir.path().join("ignored.py"), "eval('evil')\n").unwrap();
        std::fs::write(dir.path().join("kept.py"), "eval('also evil')\n").unwrap();
        let files = collect_scannable_files(dir.path());
        assert!(
            files
                .iter()
                .all(|f| f.file_name().is_some_and(|n| n != "ignored.py")),
            "gitignored files must be skipped: {files:?}"
        );
    }

    fn sealed_audit_jsonl(path: &std::path::Path, tamper: bool) {
        use crate::shared::audit::{AuditAction, AuditOutcome};
        let mut prev = initial_hash(None);
        let mut lines = Vec::new();
        for i in 0..3u64 {
            let mut event = AuditEvent {
                id: format!("evt-{i}"),
                sequence: i,
                timestamp: format!("2026-08-19T00:00:{i:02}Z"),
                action: AuditAction::ToolInvoke,
                outcome: AuditOutcome::Success,
                actor_id: "tester".into(),
                tenant_id: "default".into(),
                reason: "test event".into(),
                chain_hash: String::new(),
                policy_hash: None,
                trace_id: None,
                metadata: Some(json!({"i": i})),
            };
            event.chain_hash = chain_hash_of(&prev, &event, None);
            prev = event.chain_hash.clone();
            lines.push(serde_json::to_string(&event).unwrap());
        }
        if tamper {
            // Rewrite the middle event's reason without resealing the chain.
            let mut evt: serde_json::Value = serde_json::from_str(&lines[1]).unwrap();
            evt["reason"] = json!("tampered after the fact");
            lines[1] = serde_json::to_string(&evt).unwrap();
        }
        std::fs::write(path, lines.join("\n") + "\n").unwrap();
    }

    #[tokio::test]
    async fn audit_verify_ok_on_intact_chain() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("audit.jsonl");
        sealed_audit_jsonl(&path, false);
        let tool = PluginAuditVerify {
            def: tool_def("plugin_audit_verify", "audit", json!({})),
        };
        let out = tool
            .run(
                &ToolContext::new(),
                json!({"file": path.display().to_string()}),
            )
            .await;
        match out {
            ToolOutcome::Success { content } => {
                assert!(content.contains("OK: 3 events, chain intact"), "{content}");
            }
            other => panic!("expected Success, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn audit_verify_fails_on_tampered_chain() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("audit.jsonl");
        sealed_audit_jsonl(&path, true);
        let tool = PluginAuditVerify {
            def: tool_def("plugin_audit_verify", "audit", json!({})),
        };
        let out = tool
            .run(
                &ToolContext::new(),
                json!({"file": path.display().to_string(), "json": true}),
            )
            .await;
        match out {
            ToolOutcome::Success { content } => {
                assert!(content.contains("\"status\": \"fail\""), "{content}");
                assert!(content.contains("\"broken_at_sequence\": 1"), "{content}");
            }
            other => panic!("expected Success, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn audit_verify_missing_file_arg_is_invalid_args() {
        let tool = PluginAuditVerify {
            def: tool_def("plugin_audit_verify", "audit", json!({})),
        };
        assert!(matches!(
            tool.run(&ToolContext::new(), json!({})).await,
            ToolOutcome::Failure(_)
        ));
    }

    #[tokio::test]
    async fn verify_workspace_deferral_names_the_reducer() {
        let tool = PluginVerifyWorkspace {
            def: tool_def("plugin_verify_workspace", "vw", json!({})),
        };
        let out = tool.run(&ToolContext::new(), json!({})).await;
        match out {
            ToolOutcome::Success { content } => {
                assert!(content.contains("not implemented (reducer not ported)"));
                assert!(content.contains("ReducedStatePacket"));
            }
            other => panic!("expected Success, got {other:?}"),
        }
    }

    #[test]
    fn all_plugin_sdk_tools_registers_six() {
        let tools = all_plugin_sdk_tools();
        assert_eq!(
            tools.len(),
            6,
            "expected exactly 6 compiled-in plugin tools"
        );
        let names: Vec<&str> = tools.iter().map(|t| t.def().name).collect();
        assert!(names.contains(&"plugin_verify"));
        assert!(names.contains(&"plugin_verify_workspace"));
        assert!(names.contains(&"plugin_audit_verify"));
        assert!(names.contains(&"plugin_doctor"));
        assert!(names.contains(&"plugin_health"));
        assert!(names.contains(&"plugin_tools"));
    }

    #[test]
    fn derive_languages_when_no_tools() {
        let caps = vec![
            ToolCap {
                name: "eslint",
                available: false,
                version: None,
                source: "external",
            },
            ToolCap {
                name: "tsc",
                available: false,
                version: None,
                source: "external",
            },
            ToolCap {
                name: "ruff",
                available: false,
                version: None,
                source: "external",
            },
            ToolCap {
                name: "pyright",
                available: false,
                version: None,
                source: "external",
            },
            ToolCap {
                name: "bandit",
                available: false,
                version: None,
                source: "external",
            },
        ];
        let langs = derive_languages(&caps);
        assert!(langs.contains(&"shell (advisory only)"));
        assert!(!langs.contains(&"typescript"));
        assert!(!langs.contains(&"python"));
    }

    #[test]
    fn derive_languages_when_ts_and_py_present() {
        let caps = vec![
            ToolCap {
                name: "eslint",
                available: true,
                version: Some("9".into()),
                source: "external",
            },
            ToolCap {
                name: "tsc",
                available: false,
                version: None,
                source: "external",
            },
            ToolCap {
                name: "ruff",
                available: false,
                version: None,
                source: "external",
            },
            ToolCap {
                name: "pyright",
                available: true,
                version: None,
                source: "external",
            },
            ToolCap {
                name: "bandit",
                available: false,
                version: None,
                source: "external",
            },
        ];
        let langs = derive_languages(&caps);
        assert!(langs.contains(&"typescript"));
        assert!(langs.contains(&"javascript"));
        assert!(langs.contains(&"python"));
    }
}
