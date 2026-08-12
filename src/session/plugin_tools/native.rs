//! Compiled-in Rust implementations of the `kf-plugin` tools (WO 29.1).
//!
//! Replaces the `plugins/kf-plugin/tools/*.sh` shell wrappers that each
//! `exec node $CLI <cmd>`. Three of the six commands (`doctor`, `health`,
//! `tools`) run fully natively here; the three verify commands defer to the
//! Node SDK until the orchestrator pipeline is ported in WO 29.7.
//!
//! Enabled by the `kf-plugin-tools` cargo feature (default on). When the
//! feature is off, the shell-plugin dir loads via `PluginToolWrapper` as a
//! graceful fallback (ADR-050).

use crate::shared::{ToolDef, ToolOutcome};
use crate::tools::{Tool, ToolContext};
use serde_json::json;
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
/// Mirrors `probeTool` in `npm/kf-plugin/packages/plugin/src/index.ts`.
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
        // The TS version bootstraps the Node orchestrator for SLO stats.
        // The Rust runtime has no embedded orchestrator yet (WO 29.7), so
        // report the folded-tool path as healthy and point at the SDK.
        ToolOutcome::Success {
            content:
                "Status:         ok\n\
                      Tools:          native (compiled-in, kf-plugin-tools feature)\n\
                      Orchestrator:   not embedded — verify/SLO stats need the Node SDK (WO 29.7)\n"
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
        // Ported verbatim from npm/kf-plugin/apps/cli/src/commands/tools.ts.
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

// ── verify / verify-workspace / audit-verify: deferred to WO 29.7 ───────

fn deferred_message(cmd: &str, remaining: &str) -> String {
    format!(
        "{cmd}: not yet implemented as a native Rust call (WO 29.1 Phase 1).\n\
         The verification pipeline ports in WO 29.7; the audit hash-chain lands in WO 29.4.\n\
         Remaining work: {remaining}\n\
         To run this now, rebuild with `--no-default-features` (shell/Node wrapper fallback)\n\
         or invoke the Node SDK directly: `node <kf-plugin-cli> {cmd}`."
    )
}

struct PluginVerify {
    def: ToolDef,
}

#[async_trait::async_trait]
impl Tool for PluginVerify {
    fn def(&self) -> ToolDef {
        self.def.clone()
    }

    async fn run(&self, _ctx: &ToolContext, _args: serde_json::Value) -> ToolOutcome {
        ToolOutcome::Success {
            content: deferred_message(
                "verify",
                "port orchestrator.verify() + the lint/type/security/graph emitters to Rust.",
            ),
        }
    }
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
                "port verifyWorkspace() + ReducedStatePacket assembly to Rust.",
            ),
        }
    }
}

struct PluginAuditVerify {
    def: ToolDef,
}

#[async_trait::async_trait]
impl Tool for PluginAuditVerify {
    fn def(&self) -> ToolDef {
        self.def.clone()
    }

    async fn run(&self, _ctx: &ToolContext, _args: serde_json::Value) -> ToolOutcome {
        ToolOutcome::Success {
            content: deferred_message(
                "audit-verify",
                "port the core-events hash-chain (chainHashOf/initialHash) to Rust, then the JSONL walker.",
            ),
        }
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
                        "json": { "type": "boolean", "description": "Emit JSON instead of human-readable text", "default": false }
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
    async fn health_returns_ok_status() {
        let tool = PluginHealth {
            def: tool_def("plugin_health", "health", json!({})),
        };
        let out = tool.run(&ToolContext::new(), json!({})).await;
        match out {
            ToolOutcome::Success { content } => {
                assert!(content.contains("ok"), "health should report ok: {content}");
                assert!(content.contains("WO 29.7"));
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

    #[tokio::test]
    async fn verify_deferred_message_is_explicit() {
        let tool = PluginVerify {
            def: tool_def("plugin_verify", "verify", json!({})),
        };
        let out = tool.run(&ToolContext::new(), json!({})).await;
        match out {
            ToolOutcome::Success { content } => {
                assert!(content.contains("not yet implemented"));
                assert!(content.contains("WO 29.7"));
                assert!(content.contains("--no-default-features"));
            }
            other => panic!("expected Success, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn audit_verify_deferred_message_mentions_hash_chain() {
        let tool = PluginAuditVerify {
            def: tool_def("plugin_audit_verify", "audit", json!({})),
        };
        let out = tool.run(&ToolContext::new(), json!({})).await;
        match out {
            ToolOutcome::Success { content } => {
                assert!(content.contains("hash-chain"));
                assert!(content.contains("WO 29.4"));
            }
            other => panic!("expected Success, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn verify_workspace_deferred_message() {
        let tool = PluginVerifyWorkspace {
            def: tool_def("plugin_verify_workspace", "vw", json!({})),
        };
        let out = tool.run(&ToolContext::new(), json!({})).await;
        match out {
            ToolOutcome::Success { content } => {
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
