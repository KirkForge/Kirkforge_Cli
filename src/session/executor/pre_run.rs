//! Phase-1 per-call pre-gate: decides whether a tool call may spawn or must be
//! skipped with a buffered failure (sandbox/approval/plan-mode/schema/hook).

use crate::session::access::GuardVerdict;
use crate::session::bash_runner::check_bash_command_str;
use crate::session::toolset::Toolset;
use crate::shared::permission::{evaluate, PermissionAction};
use crate::shared::{read_shared_config, ToolInvocation};

use super::helpers::*;
use super::types::{ApprovalDecision, TurnEvent};
use super::{ApprovalRequest, Executor};
use tokio::sync::mpsc;

impl Executor {
    /// Phase-1 pre-gate: decide whether a tool call should be spawned in
    /// parallel or skipped entirely because it failed a read-only safety
    /// check (unknown tool, plan mode, schema, permission rule, approval,
    /// deny list, URL deny list, bash command check, search-path check, or
    /// pre-tool hook).
    ///
    /// For file tools the path guard is also applied here (so oversized reads
    /// etc. never reach the tool body), but the read-before-edit gate is
    /// deferred to Phase 3 so `[read_file(X), edit_file(X)]` in the same batch
    /// can pass.
    pub(super) async fn pre_run_verdict(
        &mut self,
        tc: &ToolInvocation,
        approval_sender: &mpsc::UnboundedSender<ApprovalRequest>,
    ) -> anyhow::Result<PreRunVerdict> {
        let tool = match self.tools.resolve(&tc.name) {
            Some(t) => t,
            None => {
                return Ok(PreRunVerdict::Skip {
                    events: vec![TurnEvent::Error(format!("Unknown tool: {}", tc.name))],
                    message: format!("Unknown tool: {}", tc.name),
                });
            }
        };

        // Plan-mode enforcement: only read-only discovery tools may run.
        // Skipped entirely in non-interactive runs — plan mode is an
        // interactive aid (exit via `/implement`) and enforcing it would
        // brick a scripted run. (WO 30.9)
        if self.plan_mode && !self.non_interactive {
            let allowed = match tc.name.as_str() {
                "read_file" | "read_image" | "grep" | "glob" => true,
                "bash_status" | "bash_cancel" => true,
                "bash" => tc
                    .arguments
                    .get("command")
                    .and_then(|v| v.as_str())
                    .map(is_read_only_bash)
                    .unwrap_or(false),
                _ => false,
            };
            if !allowed {
                return Ok(PreRunVerdict::Skip {
                    events: vec![TurnEvent::ToolResult {
                        name: tc.name.clone(),
                        output: format!(
                            "📐 Plan mode blocked {}: only read-only discovery tools are allowed until you type /implement.",
                            tc.name
                        ),
                        success: false,
                    }],
                    message: format!(
                        "📐 Plan mode blocked {}: only read-only discovery tools are allowed until you type /implement.",
                        tc.name
                    ),
                });
            }
        }

        if let Some(reason) = validate_args_against_schema(&tc.arguments, &tool.def().parameters) {
            let err = format!("❌ Invalid arguments for {}: {reason}", tc.name);
            return Ok(PreRunVerdict::Skip {
                events: vec![TurnEvent::ToolResult {
                    name: tc.name.clone(),
                    output: err.clone(),
                    success: false,
                }],
                message: err,
            });
        }

        let (auto_approve, permission_rules) = {
            let cfg = read_shared_config(&self.config);
            (
                cfg.security.auto_approve,
                cfg.security.permission_rules.clone(),
            )
        };
        let is_destructive = matches!(tc.name.as_str(), "write_file" | "edit_file" | "bash");
        let is_read_only_bash_call = tc.name == "bash"
            && tc
                .arguments
                .get("command")
                .and_then(|v| v.as_str())
                .map(is_read_only_bash)
                .unwrap_or(false);

        // `auto_approve = true` is an operator opt-in: ALL destructive
        // tools (including non-read-only bash) proceed without asking.
        // The bash-specific downgrade that used to live here was the
        // recurring bug across WO 12/24/27/30 — it silently defeated the
        // opt-in for the most common destructive operation. Deny rules
        // still win (handled by `evaluate` below); only the *default*
        // changes.
        let default_action = if !is_destructive || is_read_only_bash_call || auto_approve {
            PermissionAction::Allow
        } else {
            PermissionAction::Ask
        };
        let (action, matched_rule_idx) =
            evaluate(&permission_rules, &tc.name, &tc.arguments, default_action);

        if let Some(idx) = matched_rule_idx {
            let r = &permission_rules[idx];
            tracing::debug!(
                tool = %tc.name,
                rule_index = idx,
                rule_tool = %r.tool,
                rule_key = %r.key,
                rule_pattern = %r.pattern,
                action = ?action,
                "permission rule matched",
            );
        }

        if matches!(action, PermissionAction::Deny) {
            let reason = format!(
                "❌ Permission rule denied {}:{}={}",
                tc.name,
                tc.arguments
                    .as_object()
                    .and_then(|o| o.keys().next().map(|s| s.as_str()))
                    .unwrap_or(""),
                tc.arguments
                    .as_object()
                    .and_then(|o| o.values().next())
                    .and_then(|v| v.as_str())
                    .unwrap_or(""),
            );
            if let Some(idx) = matched_rule_idx {
                let r = &permission_rules[idx];
                self.audit_log.log_destructive(
                    &tc.name,
                    &tc.arguments,
                    false,
                    Some(&format!(
                        "denied by rule #{} {}:{}={} -> deny",
                        idx + 1,
                        r.tool,
                        r.key,
                        r.pattern,
                    )),
                );
            }
            return Ok(PreRunVerdict::Skip {
                events: vec![TurnEvent::ToolResult {
                    name: tc.name.clone(),
                    output: reason.clone(),
                    success: false,
                }],
                message: reason,
            });
        }

        if matches!(action, PermissionAction::Ask) {
            match self.run_approval_flow(tc, approval_sender).await? {
                ApprovalDecision::Approved | ApprovalDecision::AlwaysApproved => {}
                ApprovalDecision::Denied { reason } => {
                    let msg = format!("❌ Approval denied: {reason}");
                    return Ok(PreRunVerdict::Skip {
                        events: vec![TurnEvent::ToolResult {
                            name: tc.name.clone(),
                            output: msg.clone(),
                            success: false,
                        }],
                        message: msg,
                    });
                }
            }
        }

        if let Some(denied) = check_url_in_args(&tc.arguments, &self.sandbox.deny_list) {
            return Ok(PreRunVerdict::Skip {
                events: vec![TurnEvent::ToolResult {
                    name: tc.name.clone(),
                    output: denied.clone(),
                    success: false,
                }],
                message: denied,
            });
        }

        if let Some(denied) = check_deny_list(&self.sandbox.deny_list, &tc.name, &tc.arguments) {
            return Ok(PreRunVerdict::Skip {
                events: vec![TurnEvent::ToolResult {
                    name: tc.name.clone(),
                    output: denied.clone(),
                    success: false,
                }],
                message: denied,
            });
        }

        if tc.name == "bash" {
            let bash_cmd = tc
                .arguments
                .get("command")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let bash_workdir = tc.arguments.get("workdir").and_then(|v| v.as_str());
            let bash_sandbox_workdir = read_shared_config(&self.config)
                .security
                .bash_sandbox_workdir;
            if let Some(denied) = check_bash_command_str(
                bash_cmd,
                bash_workdir,
                &self.sandbox.deny_list,
                &self.sandbox.path_guard,
                bash_sandbox_workdir,
            ) {
                return Ok(PreRunVerdict::Skip {
                    events: vec![TurnEvent::ToolResult {
                        name: tc.name.clone(),
                        output: denied.clone(),
                        success: false,
                    }],
                    message: denied,
                });
            }
        }

        if matches!(tc.name.as_str(), "grep" | "glob") {
            let path_str = match tc.name.as_str() {
                "glob" => tc
                    .arguments
                    .get("base_dir")
                    .and_then(|v| v.as_str())
                    .unwrap_or("."),
                _ => tc
                    .arguments
                    .get("path")
                    .and_then(|v| v.as_str())
                    .unwrap_or("."),
            };
            let path = std::path::Path::new(path_str);
            if let GuardVerdict::Denied(msg) = check_search_path(&self.sandbox.path_guard, path) {
                return Ok(PreRunVerdict::Skip {
                    events: vec![TurnEvent::ToolResult {
                        name: tc.name.clone(),
                        output: format!("🔒 Access denied: {msg}"),
                        success: false,
                    }],
                    message: format!("🔒 Access denied: {msg}"),
                });
            }
        }

        // File tools: run path guard here so oversized reads never reach the
        // tool body. Return the resolved path so Phase 3 can check the
        // read-before-edit gate and mark reads without re-resolving.
        if matches!(
            tc.name.as_str(),
            "read_file" | "read_image" | "write_file" | "edit_file"
        ) {
            let path_str = tc
                .arguments
                .get("path")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let path = std::path::Path::new(path_str);
            let verdict = if tc.name == "read_file" || tc.name == "read_image" {
                self.sandbox.check_read(path)
            } else {
                self.sandbox.check_write(path).await
            };
            match verdict {
                GuardVerdict::Allowed(resolved) => {
                    return Ok(PreRunVerdict::Spawn(tool, Some(resolved)));
                }
                GuardVerdict::Denied(msg) => {
                    return Ok(PreRunVerdict::Skip {
                        events: vec![TurnEvent::ToolResult {
                            name: tc.name.clone(),
                            output: format!("🔒 Access denied: {msg}"),
                            success: false,
                        }],
                        message: format!("🔒 Access denied: {msg}"),
                    });
                }
            }
        }

        // Pre-tool hook for non-file tools. File-tool hooks run after path
        // resolution in `record_tool_result` so they see resolved paths.
        let args_json = serde_json::to_string(&tc.arguments).unwrap_or_default();
        if let Some(reason) = self
            .run_pre_tool_hook(
                &format!("pre-tool-{}", tc.name),
                Some(&tc.name),
                Some(&args_json),
            )
            .await
        {
            let denied = format!("❌ Hook denied {}: {}", tc.name, reason);
            return Ok(PreRunVerdict::Skip {
                events: vec![TurnEvent::ToolResult {
                    name: tc.name.clone(),
                    output: denied.clone(),
                    success: false,
                }],
                message: denied,
            });
        }

        Ok(PreRunVerdict::Spawn(tool, None))
    }
}

/// Verdict produced by the Phase 1 pre-gate.  means the call is allowed
/// to run in parallel; `Skip` means it was denied before the tool body and the
/// supplied events/message should be recorded in input order during Phase 3.
pub(super) enum PreRunVerdict {
    Spawn(
        std::sync::Arc<dyn crate::tools::Tool>,
        Option<std::path::PathBuf>,
    ),
    Skip {
        events: Vec<TurnEvent>,
        message: String,
    },
}
