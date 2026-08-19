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
use crate::session::worktree::WorktreeSession;
use crate::shared::{Config, Role, SharedConfig};
use crate::tools::task::{TaskConcurrencyMode, TaskRequest, TaskSpawner};
use crate::tools::toolset::{CompositeToolset, VecToolset};
use crate::tools::{Tool, UndoStackRef};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use tokio::sync::mpsc;

// WO 35.2: only writer personas need filesystem isolation — `explore` and
// `plan` get read-only toolsets, so they keep the parent sandbox. The `_`
// arm in the toolset filter below (full toolset) is the same predicate.
fn subagent_worktree_wanted(cfg: &Config, persona: &str) -> bool {
    cfg.session.worktree_enabled && !matches!(persona, "explore" | "plan")
}

// The repo the subagent worktree branches from: the parent's sandbox when it
// is itself a git worktree (session-level --worktree mode, so the subagent
// sees the parent worktree's HEAD), otherwise the process CWD — the same
// root `run_session` uses for the session worktree.
fn subagent_worktree_root(cfg: &Config) -> std::path::PathBuf {
    cfg.security
        .sandbox_dir
        .as_deref()
        .filter(|s| !s.is_empty())
        .map(std::path::PathBuf::from)
        .filter(|p| p.join(".git").exists())
        .unwrap_or_else(|| {
            std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."))
        })
}

// Drop-based cleanup for the subagent temp dir (WO 35.3 item, done here
// because run_task is restructured in the same pass): end-of-function
// `remove_dir_all` leaked the conversation log + checkpoints on every
// error path (`?` returns) and on cancellation. The guard runs on all of
// them, plus panics.
struct TempDirGuard(std::path::PathBuf);

impl Drop for TempDirGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn task_temp_tag() -> String {
    format!(
        "{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0)
    )
}

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
    parent_approval:
        std::sync::Arc<std::sync::Mutex<Option<mpsc::UnboundedSender<ApprovalRequest>>>>,
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

    /// Clear the parent-approval forwarder, dropping the cloned sender.
    /// Called at turn end so the approval channel closes when the caller
    /// releases its own sender — without this, a handler that loops on
    /// `recv().await` until channel closure blocks forever.
    pub fn clear_parent_approval(&self) {
        if let Ok(mut guard) = self.parent_approval.lock() {
            *guard = None;
        }
    }
}

#[async_trait::async_trait]
impl TaskSpawner for InProcessTaskSpawner {
    async fn run_task(&self, request: TaskRequest) -> Result<String, String> {
        let mut cfg = crate::shared::read_shared_config(&self.config).clone();

        // WO 30.0.6: subagent provider override. Resolution order for the
        // model: per-call `task` arg → subagent_provider.model → parent's
        // model. Host and API keys fall back to the parent when the
        // subagent override is unset, so a partial [subagent_provider]
        // block (e.g. model + host only) still inherits the parent keys.
        let sub = &cfg.model.subagent_provider;
        let effective_model = request
            .model
            .as_deref()
            .or(sub.model.as_deref().filter(|m| !m.is_empty()))
            .unwrap_or(&self.model_name);
        let effective_host = sub
            .ollama_host
            .as_deref()
            .filter(|h| !h.is_empty())
            .unwrap_or(&self.ollama_host);

        // If subagent_allowed_models is set, enforce the allowlist.
        if let Some(allowed) = &cfg.model.subagent_allowed_models {
            if !allowed.is_empty() && !allowed.iter().any(|m| m == effective_model) {
                return Err(format!(
                    "model '{effective_model}' not in allowed subagent models list"
                ));
            }
        }

        // WO 35.2: per-subagent worktree isolation for writer personas.
        // Mirrors run_session.rs: sandbox_dir is set BEFORE access_from_config
        // so the path guard, landlock extra paths, and the executor's guard
        // tower all center on the worktree. Creation failure is a hard error
        // (same policy as the session-level worktree in run_session.rs).
        let worktree = if subagent_worktree_wanted(&cfg, &request.persona) {
            let tag = format!("task-{}", task_temp_tag());
            let root = subagent_worktree_root(&cfg);
            Some(
                WorktreeSession::create(&tag, &root)
                    .map_err(|e| format!("subagent worktree creation failed: {e}"))?,
            )
        } else {
            None
        };
        if let Some(wt) = &worktree {
            cfg.security.sandbox_dir = Some(wt.path().to_string_lossy().to_string());
        }

        let adapter = adapters::caching::maybe_wrap_cached(
            adapters::adapter_for_with_provider(
                effective_model,
                effective_host,
                None,
                &cfg.model.anthropic_provider,
                cfg.model.request_timeout_secs,
                &cfg.model.opencode_zen_endpoint,
                cfg.model.opencode_zen_api_key.as_deref(),
                Some(&cfg.model.adapter_routing),
                &adapters::ProviderApiKeys {
                    anthropic: sub
                        .anthropic_api_key
                        .clone()
                        .or(cfg.model.anthropic_api_key.clone()),
                    openai: sub
                        .openai_api_key
                        .clone()
                        .or(cfg.model.openai_api_key.clone()),
                    deepseek: sub
                        .deepseek_api_key
                        .clone()
                        .or(cfg.model.deepseek_api_key.clone()),
                    gemini: sub
                        .gemini_api_key
                        .clone()
                        .or(cfg.model.gemini_api_key.clone()),
                    kimi: sub.kimi_api_key.clone().or(cfg.model.kimi_api_key.clone()),
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
            // WO 32.4: include the sandbox_dir (worktree path when worktree
            // isolation is active) in the landlock allow-list so subagent
            // bash calls get full r/w to the worktree. Without this, the
            // landlock workspace is the bash workdir (process CWD = repo
            // root), and the worktree at a temp path is only covered by the
            // read-only /tmp rule — writes to the worktree hit EACCES.
            landlock_extra_paths: {
                let mut paths: Vec<std::path::PathBuf> = cfg
                    .security
                    .landlock_extra_paths
                    .iter()
                    .map(std::path::PathBuf::from)
                    .collect();
                if let Some(dir) = &cfg.security.sandbox_dir {
                    if !dir.is_empty() {
                        let pb = std::path::PathBuf::from(dir);
                        if !paths.contains(&pb) {
                            paths.push(pb);
                        }
                    }
                }
                paths
            },
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

        let temp_dir = std::env::temp_dir().join(format!("kf-code-task-{}", task_temp_tag()));
        std::fs::create_dir_all(&temp_dir)
            .map_err(|e| format!("failed to create task temp dir: {e}"))?;
        let _temp_guard = TempDirGuard(temp_dir.clone());
        let log_path = temp_dir.join("conversation.ndjson");

        let conversation = ConversationLog::open_async(log_path.clone())
            .await
            .map_err(|e| format!("failed to open task conversation log: {e}"))?
            .0;

        // The executor gets a frozen clone of the (worktree-adjusted) config,
        // not the parent's live SharedConfig: its guard tower is built from
        // this snapshot, so writes must be gated against the worktree
        // sandbox, and a mid-run parent config edit should not move a
        // subagent's sandbox underneath it.
        let exec_config: SharedConfig = Arc::new(std::sync::RwLock::new(cfg.clone()));
        let mut composite = CompositeToolset::empty();
        composite.add(Box::new(VecToolset::new("task", tools)));
        let mut executor = Executor::with_log_and_undo(
            adapter,
            composite,
            exec_config,
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
        let parent_approval = self
            .parent_approval
            .lock()
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
                             in the parent session"
                                .into(),
                        )
                    };
                    crate::send_or_warn!(
                        req.response.send(resp),
                        "task approval response receiver dropped"
                    );
                }
            }
        });

        // WO 35.3: the cancel flag from the TaskRequest (shared with the
        // TaskHandle) drives the executor's existing AtomicBool machinery;
        // the token is attached to the executor so in-flight tool calls
        // die on cancel. `cancel(None)` requests are uncancellable — the
        // flag is a local no-op like before.
        let cancelled = request
            .cancel
            .as_ref()
            .map(|c| Arc::clone(&c.flag))
            .unwrap_or_else(|| Arc::new(AtomicBool::new(false)));
        if let Some(c) = &request.cancel {
            executor.set_cancel_token(Some(c.token.clone()));
        }
        let prompt = build_task_prompt(&request.persona, &request.prompt);

        for turn_num in 0..request.max_turns {
            // Cooperative cancel exit (WO 35.3): checked before each turn —
            // a task cancelled before start or between turns returns its
            // partial summary + patch instead of starting more model work.
            if cancelled.load(std::sync::atomic::Ordering::SeqCst) {
                break;
            }
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

        // WO 35.2: capture the subagent's uncommitted worktree edits as an
        // appliable patch BEFORE the WorktreeSession Drop runs
        // `git worktree remove --force` (which would discard them). The
        // patch rides in the summary so the caller — the parent model —
        // can review and `git apply` it. Ceiling: on an Err return the
        // worktree is dropped without a patch (infra failures mid-coder;
        // upgrade path: capture in a catch-all before `?` propagation).
        // ponytail: trait signature stays Result<String, String> — the
        // executor's tool-result slicing already bounds context growth.
        let mut result = summary;
        if let Some(wt) = &worktree {
            let patch = wt.diff_patch();
            if !patch.trim().is_empty() {
                result = format!(
                    "{result}\n\n--- subagent patch (uncommitted worktree changes; apply in \
                     the parent with `git apply`) ---\n{patch}"
                );
            }
        }
        Ok(result)
    }
}

pub(crate) fn build_task_prompt(persona: &str, task: &str) -> String {
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

    // ── WO 35.2: worktree gating + temp-dir hygiene ──

    #[test]
    fn subagent_worktree_gated_on_flag_and_writer_persona() {
        let mut cfg = Config::default();
        cfg.session.worktree_enabled = true;
        assert!(subagent_worktree_wanted(&cfg, "coder"));
        assert!(!subagent_worktree_wanted(&cfg, "explore"));
        assert!(!subagent_worktree_wanted(&cfg, "plan"));
        cfg.session.worktree_enabled = false;
        assert!(
            !subagent_worktree_wanted(&cfg, "coder"),
            "flag off = shared sandbox"
        );
    }

    #[test]
    fn temp_dir_guard_removes_dir_on_drop() {
        let dir = std::env::temp_dir().join(format!("kf-code-guard-test-{}", task_temp_tag()));
        std::fs::create_dir_all(dir.join("nested")).unwrap();
        std::fs::write(dir.join("nested").join("conversation.ndjson"), "{}").unwrap();
        drop(TempDirGuard(dir.clone()));
        assert!(!dir.exists(), "guard must remove the tree on drop");
    }

    fn task_temp_dirs_for_this_pid() -> Vec<std::path::PathBuf> {
        let mut dirs: Vec<_> = std::fs::read_dir(std::env::temp_dir())
            .map(|rd| {
                rd.flatten()
                    .map(|e| e.path())
                    .filter(|p| {
                        p.file_name().is_some_and(|n| {
                            n.to_string_lossy()
                                .starts_with(&format!("kf-code-task-{}", std::process::id()))
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        dirs.sort();
        dirs
    }

    // The two run_task tests below scan the shared `kf-code-task-<pid>-*`
    // temp namespace; serialize them so a concurrent run_task's live temp
    // dir can't fail the other's leak assertion under threaded libtest.
    // (nextest runs each test in its own process and never contends.)
    // tokio Mutex: the guard is held across the run_task await.
    static RUN_TASK_TMP_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    // Error returns from a turn must not leak the temp dir (WO 35.3 item,
    // covered here because the guard landed with the 35.2 restructure).
    // A dead host makes the model request fail (ECONNREFUSED); the
    // adapter's retry backoff (~3.7s total) runs in real time.
    #[tokio::test]
    async fn run_task_error_return_still_cleans_temp_dir() {
        let _tmp_lock = RUN_TASK_TMP_LOCK.lock().await;
        let mut cfg = Config::default();
        cfg.model.request_timeout_secs = 5;
        let config: SharedConfig = Arc::new(std::sync::RwLock::new(cfg));
        let spawner = InProcessTaskSpawner::new(
            config,
            "test-model".into(),
            "127.0.0.1:1".into(),
            None,
            false,
        );
        let request = TaskRequest {
            prompt: "doomed".into(),
            persona: "coder".into(),
            model: None,
            max_turns: 1,
            cancel: None,
        };
        let result = spawner.run_task(request).await;
        assert!(
            result.is_err(),
            "dead host must surface an error, got {result:?}"
        );
        let leftover = task_temp_dirs_for_this_pid();
        assert!(
            leftover.is_empty(),
            "temp dir leaked on error return: {leftover:?}"
        );
    }

    // WO 35.3: a pre-cancelled task must exit cooperatively before any
    // model work (turn loop checks the flag up front), still return its
    // (empty) summary, and clean the temp dir. No network involved.
    #[tokio::test]
    async fn run_task_precancelled_exits_early_and_cleans_temp_dir() {
        let _tmp_lock = RUN_TASK_TMP_LOCK.lock().await;
        let config: SharedConfig = Arc::new(std::sync::RwLock::new(Config::default()));
        let spawner = InProcessTaskSpawner::new(
            config,
            "test-model".into(),
            "127.0.0.1:1".into(),
            None,
            false,
        );
        let cancel = crate::tools::task::TaskCancel {
            flag: Arc::new(AtomicBool::new(true)),
            token: tokio_util::sync::CancellationToken::new(),
        };
        let request = TaskRequest {
            prompt: "never starts".into(),
            persona: "coder".into(),
            model: None,
            max_turns: 3,
            cancel: Some(cancel),
        };
        let result = spawner.run_task(request).await;
        assert_eq!(
            result.unwrap(),
            "(no assistant response produced)",
            "pre-cancelled task returns its empty summary without model calls"
        );
        let leftover = task_temp_dirs_for_this_pid();
        assert!(
            leftover.is_empty(),
            "temp dir leaked on cancel: {leftover:?}"
        );
    }
}
