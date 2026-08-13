//! Concrete `TaskSpawner` that runs a subagent inside an isolated `Executor`.
//!
//! WO 28.1: moved here from `tools::task` so the `tools` layer no longer reaches
//! up into `session::executor` (the worst inversion — a *tool* constructing the
//! nested `Executor` that drives the loop which calls the tool). The
//! `TaskSpawner` port trait stays in `tools::task`; this is its session-layer
//! concrete implementation — the single intentional seam where the agent loop
//! plugs into the `task` tool.

use crate::adapters;
use crate::session::conversation::ConversationLog;
use crate::session::executor::{ApprovalRequest, ApprovalResponse, Executor};
use crate::shared::{Role, SharedConfig};
use crate::tools::task::{TaskConcurrencyMode, TaskRequest, TaskSpawner};
use crate::tools::toolset::{CompositeToolset, VecToolset};
use crate::tools::{Tool, UndoStackRef};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use tokio::sync::mpsc;

/// Spawn a subagent task inside an isolated `Executor` with a temporary
/// conversation log.
///
/// This reuses the persona tool restriction logic without requiring the
/// TUI's fork manager, so the `task` tool can run anywhere the executor
/// exists.
pub struct InProcessTaskSpawner {
    config: SharedConfig,
    model_name: String,
    ollama_host: String,
    undo_stack: Option<UndoStackRef>,
    supports_images: bool,
    /// Optional parent approval channel — if set, subagent approval requests
    /// are forwarded here so the parent's handler decides (WO 30.6).
    parent_approval: std::sync::Arc<std::sync::Mutex<Option<mpsc::UnboundedSender<ApprovalRequest>>>>,
}

impl InProcessTaskSpawner {
    pub fn new(
        config: SharedConfig,
        model_name: String,
        ollama_host: String,
        undo_stack: Option<UndoStackRef>,
        supports_images: bool,
    ) -> Self {
        Self {
            config,
            model_name,
            ollama_host,
            undo_stack,
            supports_images,
            parent_approval: std::sync::Arc::new(std::sync::Mutex::new(None)),
        }
    }

    /// Set the parent's approval channel so subagent requests are forwarded
    /// to the parent's interactive handler (WO 30.6).
    pub fn set_parent_approval(&self, tx: mpsc::UnboundedSender<ApprovalRequest>) {
        if let Ok(mut guard) = self.parent_approval.lock() {
            *guard = Some(tx);
        }
    }
}

#[async_trait::async_trait]
impl TaskSpawner for InProcessTaskSpawner {
    async fn run_task(&self, request: TaskRequest) -> Result<String, String> {
        let effective_model = request.model.as_deref().unwrap_or(&self.model_name);
        let cfg = crate::shared::read_shared_config(&self.config).clone();

        // If subagent_allowed_models is set, enforce the allowlist.
        if let Some(allowed) = &cfg.model.subagent_allowed_models {
            if !allowed.is_empty() && !allowed.iter().any(|m| m == effective_model) {
                return Err(format!(
                    "model '{effective_model}' not in allowed subagent models list"
                ));
            }
        }

        let adapter = adapters::caching::maybe_wrap_cached(
            adapters::adapter_for_with_provider(
                effective_model,
                &self.ollama_host,
                None,
                &cfg.model.anthropic_provider,
                cfg.model.request_timeout_secs,
                &cfg.model.opencode_zen_endpoint,
                cfg.model.opencode_zen_api_key.as_deref(),
                Some(&cfg.model.adapter_routing),
                &adapters::ProviderApiKeys {
                    anthropic: cfg.model.anthropic_api_key.clone(),
                    openai: cfg.model.openai_api_key.clone(),
                    deepseek: cfg.model.deepseek_api_key.clone(),
                    gemini: cfg.model.gemini_api_key.clone(),
                    kimi: cfg.model.kimi_api_key.clone(),
                },
                Some(&cfg.model.aws_region),
                if cfg.model.gcp_project_id.is_empty() {
                    None
                } else {
                    Some(cfg.model.gcp_project_id.as_str())
                },
                if cfg.model.gcp_region.is_empty() {
                    None
                } else {
                    Some(cfg.model.gcp_region.as_str())
                },
                cfg.model.gcp_service_account_path.clone(),
            ),
            &cfg,
        );

        let (deny_list, path_guard, _read_gate) = crate::shared::access::access_from_config(&cfg);
        let all = crate::tools::all_tools(&crate::tools::ToolContextBuilder {
            undo_stack: self.undo_stack.clone(),
            supports_images: self.supports_images,
            deny_list,
            path_guard,
            bash_sandbox_workdir: cfg.security.bash_sandbox_workdir,
            minify_write_side: cfg.tools.minify_write_side,
            minify_above_bytes: cfg.tools.minify_above_bytes,
            lsp_pool: None,
            computer_use_enabled: cfg.security.computer_use.enabled,
            computer_use_config: Some(cfg.security.computer_use.clone()),
            chrome_tab: None,
            session_launcher: None,
            docker_config: Some(cfg.security.docker.clone()),
            sandbox_config: cfg.security.sandbox.clone(),
            landlock_extra_paths: cfg
                .security
                .landlock_extra_paths
                .iter()
                .map(std::path::PathBuf::from)
                .collect(),
            block_edits: cfg.security.sandbox.block_edits,
            max_background_tasks: cfg.tools.max_background_tasks,
            task_concurrency_mode: cfg
                .tools
                .task_concurrency_mode
                .parse()
                .unwrap_or(TaskConcurrencyMode::Queue),
        });
        let tools: Vec<Arc<dyn Tool>> = match request.persona.as_str() {
            "explore" => all
                .into_iter()
                .filter(|t| {
                    matches!(
                        t.def().name,
                        "read_file"
                            | "read_image"
                            | "grep"
                            | "glob"
                            | "bash"
                            | "bash_status"
                            | "bash_cancel"
                            | "task"
                    )
                })
                .collect(),
            "plan" => all
                .into_iter()
                .filter(|t| {
                    matches!(
                        t.def().name,
                        "read_file" | "read_image" | "grep" | "glob" | "task"
                    )
                })
                .collect(),
            _ => all,
        };

        let temp_dir = std::env::temp_dir().join(format!(
            "kf-code-task-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&temp_dir)
            .map_err(|e| format!("failed to create task temp dir: {e}"))?;
        let log_path = temp_dir.join("conversation.ndjson");

        let conversation = ConversationLog::open_async(log_path.clone())
            .await
            .map_err(|e| format!("failed to open task conversation log: {e}"))?
            .0;

        let shared_config: SharedConfig = self.config.clone();
        let mut composite = CompositeToolset::empty();
        composite.add(Box::new(VecToolset::new("task", tools)));
        let mut executor = Executor::with_log_and_undo(
            adapter,
            composite,
            shared_config,
            conversation,
            None,
            self.undo_stack.clone(),
        )
        .map_err(|e| e.to_string())?;

        if request.persona == "explore" {
            executor.set_plan_mode(true);
        }

        let (approval_tx, mut approval_rx) = mpsc::unbounded_channel::<ApprovalRequest>();
        // WO 30.6: if the parent set its approval channel, forward subagent
        // requests to it so the parent's handler decides interactively.
        // Otherwise inherit auto_approve: approve in CI, deny otherwise (P0 fix).
        let parent_approval = self.parent_approval.lock()
            .ok()
            .and_then(|g| g.as_ref().cloned());
        let auto_approve = cfg.security.auto_approve;
        tokio::spawn(async move {
            while let Some(req) = approval_rx.recv().await {
                if let Some(ref parent) = parent_approval {
                    // Forward to parent — its handler decides + routes the response back.
                    crate::send_or_warn!(
                        parent.send(req),
                        "parent approval channel dropped; subagent request lost"
                    );
                } else {
                    let resp = if auto_approve {
                        ApprovalResponse::Approved
                    } else {
                        tracing::warn!(
                            tool = %req.tool_name,
                            "subagent approval DENIED: parent session is not in auto-approve mode"
                        );
                        ApprovalResponse::DeniedWithReason(
                            "subagent cannot approve destructive tools when the parent session \
                             is not in auto-approve mode; enable auto_approve or run the tool \
                             in the parent session".into(),
                        )
                    };
                    crate::send_or_warn!(
                        req.response.send(resp),
                        "task approval response receiver dropped"
                    );
                }
            }
        });

        let cancelled = Arc::new(AtomicBool::new(false));
        let prompt = build_task_prompt(&request.persona, &request.prompt);

        for turn_num in 0..request.max_turns {
            let input = if turn_num == 0 {
                prompt.as_str()
            } else {
                "continue"
            };
            executor
                .run_turn_collecting(input, &approval_tx, &cancelled)
                .await
                .map_err(|e| format!("task turn {turn_num} failed: {e}"))?;

            if turn_num + 1 >= request.max_turns {
                break;
            }

            let last = executor
                .conversation_log()
                .all()
                .iter()
                .rev()
                .find(|m| matches!(m.role, Role::Assistant));
            let finished = match last {
                Some(m) => {
                    m.tool_calls.is_none() || m.tool_calls.as_ref().is_none_or(|t| t.is_empty())
                }
                None => true,
            };
            if finished {
                break;
            }
        }

        let summary = executor
            .conversation_log()
            .all()
            .iter()
            .rev()
            .find(|m| matches!(m.role, Role::Assistant) && !m.content.is_empty())
            .map(|m| m.content.clone())
            .unwrap_or_else(|| "(no assistant response produced)".to_string());

        let _ = std::fs::remove_dir_all(&temp_dir);
        Ok(summary)
    }
}

fn build_task_prompt(persona: &str, task: &str) -> String {
    match persona {
        "explore" => format!(
            "You are an exploratory research assistant. Read files, search, and gather context. \
             Do not edit files or run destructive commands. Produce a concise summary.\n\nTask: {task}"
        ),
        "plan" => format!(
            "You are a software architect. Explore with read-only tools only. \
             Design a step-by-step implementation plan and end with: \"## Plan Complete\".\n\nTask: {task}"
        ),
        _ => format!(
            "You are a focused implementation assistant with the full toolset. \
             Work efficiently in this isolated context and summarize what you changed and why.\n\nTask: {task}"
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // WO 28.1: moved from tools::task — these test `build_task_prompt`, which
    // relocated here alongside the InProcessTaskSpawner that uses it.
    #[test]
    fn build_task_prompt_for_coder_persona_mentions_implementation() {
        let p = build_task_prompt("coder", "do X");
        assert!(p.contains("implementation") && p.contains("do X"));
    }

    #[test]
    fn build_task_prompt_for_explore_persona_mentions_research() {
        let p = build_task_prompt("explore", "explore Y");
        assert!(p.contains("research") && p.contains("explore Y"));
    }

    #[test]
    fn build_task_prompt_for_plan_persona_mentions_architect() {
        let p = build_task_prompt("plan", "plan Z");
        assert!(p.contains("architect") && p.contains("Plan Complete"));
    }
}
