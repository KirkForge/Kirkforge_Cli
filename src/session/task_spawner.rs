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
use crate::session::executor::{ApprovalRequest, ApprovalResponse, Executor, TurnEvent};
use crate::session::worktree::WorktreeSession;
use crate::shared::{Config, Role, SharedConfig};
use crate::tools::task::{TaskConcurrencyMode, TaskRequest, TaskSpawner};
use crate::tools::toolset::{CompositeToolset, VecToolset};
use crate::tools::{Tool, UndoStackRef};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;

// Drain + clear the shared pending-messages queue (inter-subagent
// messaging). Returns the joined messages (blank-line separated) or an
// empty String when the queue is absent or empty. Called before each
// run_turn_collecting so send_message appends land as a context addition
// prepended to the turn input.
fn drain_pending_messages(queue: &Option<Arc<Mutex<Vec<String>>>>) -> String {
    let Some(q) = queue else {
        return String::new();
    };
    let mut guard = q.lock().unwrap_or_else(|e| e.into_inner());
    if guard.is_empty() {
        return String::new();
    }
    let joined = guard.join("\n\n");
    guard.clear();
    joined
}

// Marker separating the coder's change summary from its worktree patch in
// the string run_task returns (WO 35.2). pub(crate) so the parallel
// orchestrator can extract the patch without duplicating the literal
// (WO 35.1).
pub(crate) const SUBAGENT_PATCH_MARKER: &str =
    "--- subagent patch (uncommitted worktree changes; apply in the parent with `git apply`) ---";

// WO 35.2: only writer personas need filesystem isolation — `explore` and
// `plan` get read-only toolsets, so they keep the parent sandbox. The `_`
// arm in the toolset filter below (full toolset) is the same predicate.
// Subagent audit 2026-09-04: an agent's `isolation: worktree` frontmatter
// forces a worktree even when the global policy + persona would not.
fn subagent_worktree_wanted(
    cfg: &Config,
    persona: &str,
    agent_def: Option<&crate::session::agents::AgentDef>,
) -> bool {
    if agent_def.is_some_and(|a| a.isolation == crate::session::agents::AgentIsolation::Worktree) {
        return true;
    }
    cfg.session.artifact_policy.is_worktree_enabled() && !matches!(persona, "explore" | "plan")
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

// WO 38.4: identity mints from the process-global task counter, never a
// clock. Two `task` calls in one assistant message land in the same
// millisecond; with pid+millis they collided on the temp dir (two
// subagents sharing one conversation.ndjson — first finisher deleted it
// under the other) and on the worktree path (stale recovery
// force-removed a LIVE sibling worktree). The pid prefix stays for
// debuggability; the counter guarantees uniqueness.
fn task_temp_tag() -> String {
    format!(
        "{}-{}",
        std::process::id(),
        crate::tools::task::next_unique_id()
    )
}

// Create the per-task temp dir for a freshly minted tag (WO 38.4). A
// pre-existing dir at a globally unique tag means leftover state or
// corruption — error instead of silently sharing (and eventually
// deleting) a sibling's conversation log.
fn create_task_temp_dir(tag: &str) -> Result<std::path::PathBuf, String> {
    let dir = std::env::temp_dir().join(format!("kf-code-task-{tag}"));
    if dir.exists() {
        return Err(format!(
            "task temp dir already exists (stale state?): {}",
            dir.display()
        ));
    }
    std::fs::create_dir_all(&dir).map_err(|e| format!("failed to create task temp dir: {e}"))?;
    Ok(dir)
}

// Build the model adapter for a subagent, using the same provider
// config (API keys, routing, endpoint overrides) as the primary
// adapter. Factored out so the fallback path can build a fresh
// adapter for the fallback model name without duplicating the
// 20-argument `adapter_for_with_provider` call.
fn build_subagent_adapter(
    model: &str,
    host: &str,
    cfg: &Config,
    sub: &crate::shared::SubagentProvider,
) -> Box<dyn crate::adapters::ModelAdapter> {
    adapters::caching::maybe_wrap_cached(
        adapters::adapter_for_with_provider(
            model,
            host,
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
            &cfg.model.anthropic_api_base,
        ),
        cfg,
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
    /// Dynamic agent registry (WO 39.3). An unknown-persona request is
    /// looked up here before falling back to the full toolset. `None` =
    /// registry not loaded (behaves as today: full toolset).
    agents: std::sync::Arc<crate::session::agents::AgentRegistry>,
}

impl InProcessTaskSpawner {
    pub fn new(
        config: SharedConfig,
        model_name: String,
        ollama_host: String,
        undo_stack: Option<UndoStackRef>,
        supports_images: bool,
    ) -> Self {
        // WO 39.3: load the workspace agent registry once, honoring the
        // trust gate. An empty registry (dir absent or refused) is fine —
        // the `_` arm falls back to the full toolset as before.
        let trust_workspace = crate::shared::read_shared_config(&config)
            .tools
            .plugin_trust_workspace;
        let agents = crate::session::agents::global_registry(trust_workspace);
        Self {
            config,
            model_name,
            ollama_host,
            undo_stack,
            supports_images,
            parent_approval: std::sync::Arc::new(std::sync::Mutex::new(None)),
            agents,
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

    /// Test/explicit constructor with a pre-built agent registry (WO 39.3).
    /// The default `new()` loads the global workspace registry; this lets
    /// tests inject a populated registry without touching the filesystem.
    pub fn with_agent_registry(
        config: SharedConfig,
        model_name: String,
        ollama_host: String,
        undo_stack: Option<UndoStackRef>,
        supports_images: bool,
        agents: std::sync::Arc<crate::session::agents::AgentRegistry>,
    ) -> Self {
        Self {
            config,
            model_name,
            ollama_host,
            undo_stack,
            supports_images,
            parent_approval: std::sync::Arc::new(std::sync::Mutex::new(None)),
            agents,
        }
    }
}

// WO 35.6: rich outcome of one subagent run. `run_task` (the trait
// method) keeps its summary-only shape; the executor adapter consumes
// the detail to fill kf-orchestrator's `Emission` fields.
pub(crate) struct TaskRunDetail {
    pub summary: String,
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    /// "stop" | "tool_calls" | "length" — derived from the turn outcome
    /// (continuation exhaustion → "length"; trailing tool calls →
    /// "tool_calls"; else "stop"). Mirrors the FinishReason vocabulary
    /// kf-orchestrator's modes parse.
    pub finish_reason: String,
}

#[async_trait::async_trait]
impl TaskSpawner for InProcessTaskSpawner {
    async fn run_task(&self, request: TaskRequest) -> Result<String, String> {
        self.run_task_detailed(request).await.map(|d| d.summary)
    }
}

impl InProcessTaskSpawner {
    /// `run_task` plus the accounting the orchestrator adapter needs:
    /// summed CostStats token counts and a finish-reason string.
    pub(crate) async fn run_task_detailed(
        &self,
        request: TaskRequest,
    ) -> Result<TaskRunDetail, String> {
        let mut cfg = crate::shared::read_shared_config(&self.config).clone();

        // WO 39.3: resolve the dynamic agent (`.claude/agents/*.md`) for the
        // persona BEFORE the model resolution, so the agent's `model`
        // frontmatter field participates in the chain (WO 45.31 wired this
        // field — previously parsed but never read).
        let agent_def = self.agents.get(&request.persona).cloned();

        // WO 30.0.6 + 45.31: subagent provider override. Resolution order
        // for the model: per-call `task` arg → agent_def.model (frontmatter)
        // → subagent_provider.model → parent's model. Host and API keys
        // fall back to the parent when the subagent override is unset, so a
        // partial [subagent_provider] block (e.g. model + host only) still
        // inherits the parent keys.
        let sub = &cfg.model.subagent_provider;
        let effective_model = request
            .model
            .as_deref()
            .or(agent_def
                .as_ref()
                .and_then(|a| a.model.as_deref().filter(|m| !m.is_empty())))
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
        let worktree = if subagent_worktree_wanted(&cfg, &request.persona, agent_def.as_ref()) {
            let tag = format!("task-{}", task_temp_tag());
            let root = subagent_worktree_root(&cfg);
            Some(
                WorktreeSession::create(&tag, &root)
                    .await
                    .map_err(|e| format!("subagent worktree creation failed: {e}"))?,
            )
        } else {
            None
        };
        if let Some(wt) = &worktree {
            cfg.security.sandbox_dir = Some(wt.path().to_string_lossy().to_string());
        }

        let adapter = build_subagent_adapter(effective_model, effective_host, &cfg, sub);

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
            max_subagent_turns: cfg.tools.max_subagent_turns,
            max_subagent_depth: cfg.tools.max_subagent_depth,
            task_concurrency_mode: cfg
                .tools
                .task_concurrency_mode
                .parse()
                .unwrap_or(TaskConcurrencyMode::Queue),
        });
        // WO 39.3: an unknown persona name is looked up in the dynamic
        // agent registry (`.claude/agents/*.md`). A hit restricts the
        // toolset to the agent's `tools` frontmatter (translated through
        // the Claude→native alias table) and records the agent def so the
        // system prompt is prepended below. A miss keeps the full toolset.
        // `agent_def` was resolved above (before the model chain) so its
        // `model` field participates in model selection.
        let agent_allowlist: Vec<String> = agent_def
            .as_ref()
            .map(|a| crate::session::agents::translate_tool_list(&a.tools))
            .unwrap_or_default();
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
            _ if !agent_allowlist.is_empty() => all
                .into_iter()
                .filter(|t| agent_allowlist.iter().any(|n| *n == t.def().name))
                .collect(),
            _ => all,
        };

        let temp_dir = create_task_temp_dir(&task_temp_tag())?;
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
        // Subagent audit 2026-09-04: an agent's `permissionMode: "plan"`
        // frontmatter forces plan_mode on the subagent executor. Other
        // Claude permission modes have no kf-code equivalent and are
        // ignored. `explore` already set plan_mode above; this is additive.
        if let Some(a) = &agent_def {
            if a.permission_mode.as_deref() == Some("plan") {
                executor.set_plan_mode(true);
            }
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
        // WO 36.2: tag this executor's tool calls with the owning task id
        // so background bash jobs the subagent spawns are attributable —
        // TaskManager::cancel kills exactly those via cancel_by_owner.
        executor.set_task_owner(request.owner.clone());
        executor.set_subagent_depth(request.subagent_depth);
        // WO 35.1: the prompt is used verbatim — callers apply persona
        // preambles (build_task_prompt) or role prompts (the parallel
        // orchestrator) themselves, so a role prompt is no longer
        // double-wrapped in a generic "You are..." preamble.
        // WO 39.3: when the persona resolved to a dynamic agent, the
        // caller (build_task_prompt) already substituted the agent's
        // system prompt + alias suffix for the generic preamble, so the
        // prompt here is already agent-shaped and needs no further wrap.
        let prompt = request.prompt.as_str();

        let mut prompt_tokens: i64 = 0;
        let mut completion_tokens: i64 = 0;
        let mut truncated = false;

        for turn_num in 0..request.max_turns {
            // Cooperative cancel exit (WO 35.3): checked before each turn —
            // a task cancelled before start or between turns returns its
            // partial summary + patch instead of starting more model work.
            if cancelled.load(std::sync::atomic::Ordering::SeqCst) {
                break;
            }
            // Inter-subagent messaging (batch4): drain pending messages
            // queued by `send_message` and prepend them to the turn input
            // so they land as a system-level context addition. The queue
            // is shared with the TaskHandle via Arc<Mutex<Vec>>, so
            // appends from any task are visible here. An empty drain
            // yields the plain turn input unchanged.
            let inbox = drain_pending_messages(&request.pending_messages);
            let base_input = if turn_num == 0 { prompt } else { "continue" };
            let input: String = if inbox.is_empty() {
                base_input.to_string()
            } else {
                format!(
                    "[incoming message from another agent]\n{inbox}\n[end incoming message]\n\n{base_input}"
                )
            };
            let result = executor
                .run_turn_collecting(&input, &approval_tx, &cancelled)
                .await;
            let events = match result {
                Ok(events) => events,
                Err(e) if turn_num == 0 => {
                    // First turn failed — try the fallback model if one is
                    // configured. The per-subagent-provider fallback wins
                    // over the top-level field. Only the FIRST turn gets a
                    // fallback: subsequent turns use whatever adapter is
                    // active, so a mid-task model failure still propagates.
                    let fallback = sub
                        .fallback_model
                        .as_deref()
                        .filter(|m| !m.is_empty())
                        .or(cfg
                            .model
                            .subagent_fallback_model
                            .as_deref()
                            .filter(|m| !m.is_empty()));
                    if let Some(fallback_model) = fallback {
                        tracing::warn!(
                            error = %e,
                            fallback_model,
                            primary_model = effective_model,
                            "subagent model failed on first turn, trying fallback"
                        );
                        let fallback_adapter =
                            build_subagent_adapter(fallback_model, effective_host, &cfg, sub);
                        executor.swap_adapter(fallback_adapter, fallback_model);
                        executor
                            .run_turn_collecting(&input, &approval_tx, &cancelled)
                            .await
                            .map_err(|e2| {
                                format!(
                                    "task turn 0 failed with both primary ({effective_model}) \
                                     and fallback ({fallback_model}): {e} → {e2}"
                                )
                            })?
                    } else {
                        return Err(format!("task turn 0 failed: {e}"));
                    }
                }
                Err(e) => return Err(format!("task turn {turn_num} failed: {e}")),
            };
            for ev in &events {
                match ev {
                    TurnEvent::CostStats {
                        prompt_tokens: p,
                        completion_tokens: c,
                        ..
                    } => {
                        prompt_tokens += *p as i64;
                        completion_tokens += *c as i64;
                    }
                    // Emitted before the exhaustion check, so round > max
                    // marks a truncation the executor stopped continuing.
                    TurnEvent::ContinuationRound { round, max } if round > max => {
                        truncated = true;
                    }
                    _ => {}
                }
            }

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
            let patch = wt.diff_patch().await;
            if !patch.trim().is_empty() {
                result = format!("{result}\n\n{SUBAGENT_PATCH_MARKER}\n{patch}");
            }
        }

        // WO 35.6: finish reason for the Emission. Precedence: truncation
        // (continuation exhausted) > trailing tool calls (session hit
        // max_turns mid-dialog) > clean stop.
        let last_assistant = executor
            .conversation_log()
            .all()
            .iter()
            .rev()
            .find(|m| matches!(m.role, Role::Assistant));
        let has_pending_tools =
            last_assistant.is_some_and(|m| m.tool_calls.as_ref().is_some_and(|t| !t.is_empty()));
        let finish_reason = if truncated {
            "length"
        } else if has_pending_tools {
            "tool_calls"
        } else {
            "stop"
        };

        Ok(TaskRunDetail {
            summary: result,
            prompt_tokens,
            completion_tokens,
            finish_reason: finish_reason.to_string(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── batch4: pending_messages drain helper ──

    #[test]
    fn drain_pending_messages_none_queue_returns_empty() {
        assert_eq!(drain_pending_messages(&None), "");
    }

    #[test]
    fn drain_pending_messages_empty_queue_returns_empty() {
        let q: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        assert_eq!(drain_pending_messages(&Some(q.clone())), "");
        // Still empty after drain.
        assert!(q.lock().unwrap().is_empty());
    }

    #[test]
    fn drain_pending_messages_joins_and_clears() {
        let q: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(vec!["hello".into(), "world".into()]));
        let drained = drain_pending_messages(&Some(q.clone()));
        assert_eq!(drained, "hello\n\nworld");
        // Drain clears the queue.
        assert!(q.lock().unwrap().is_empty());
        // Second drain is empty.
        assert_eq!(drain_pending_messages(&Some(q)), "");
    }

    // ── WO 35.2: worktree gating + temp-dir hygiene ──

    #[test]
    fn subagent_worktree_gated_on_flag_and_writer_persona() {
        let mut cfg = Config::default();
        cfg.session.artifact_policy = crate::shared::ArtifactPolicy::PatchOnly;
        assert!(subagent_worktree_wanted(&cfg, "coder", None));
        assert!(!subagent_worktree_wanted(&cfg, "explore", None));
        assert!(!subagent_worktree_wanted(&cfg, "plan", None));
        cfg.session.artifact_policy = crate::shared::ArtifactPolicy::DirectWrite;
        assert!(
            !subagent_worktree_wanted(&cfg, "coder", None),
            "flag off = shared sandbox"
        );
    }

    #[test]
    fn subagent_worktree_forced_by_agent_isolation_worktree() {
        // Subagent audit 2026-09-04: an agent with `isolation: worktree`
        // gets a worktree even when the global policy is off and the
        // persona is a writer under DirectWrite (which would otherwise
        // share the parent sandbox).
        let cfg = Config::default();
        let agent = crate::session::agents::AgentDef {
            name: "iso".into(),
            description: String::new(),
            system_prompt: String::new(),
            tools: vec![],
            model: None,
            max_turns: None,
            isolation: crate::session::agents::AgentIsolation::Worktree,
            background: false,
            permission_mode: None,
        };
        assert!(
            subagent_worktree_wanted(&cfg, "coder", Some(&agent)),
            "agent isolation: worktree must force a worktree even under DirectWrite"
        );
        // explore persona with isolation: worktree still wins (agent field
        // is checked first).
        assert!(
            subagent_worktree_wanted(&cfg, "explore", Some(&agent)),
            "agent isolation: worktree must force a worktree even for explore"
        );
        // No agent def → falls back to global policy (DirectWrite = off).
        assert!(!subagent_worktree_wanted(&cfg, "coder", None));
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
            owner: None,
            subagent_depth: 0,
            pending_messages: None,
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
            owner: None,
            subagent_depth: 0,
            pending_messages: None,
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

    // ── WO 38.4: collision-proof identities ──

    #[test]
    fn task_temp_tag_never_repeats_even_in_the_same_millisecond() {
        // The clock-based tag (pid+millis) returned identical strings for
        // same-ms spawns; the counter-minted tag cannot.
        let a = task_temp_tag();
        let b = task_temp_tag();
        assert_ne!(a, b, "same-ms spawns must never share a tag");
    }

    #[test]
    fn create_task_temp_dir_rejects_pre_existing_dir() {
        let tag = format!(
            "{}-preexist-{}",
            std::process::id(),
            crate::tools::task::next_unique_id()
        );
        let first = create_task_temp_dir(&tag).expect("fresh tag must create");
        assert!(first.exists());
        let err = create_task_temp_dir(&tag).expect_err("collision must be an error");
        assert!(
            err.contains("already exists"),
            "collision must be an error, not silent sharing: {err}"
        );
        let _ = std::fs::remove_dir_all(first);
    }

    #[cfg(unix)]
    fn live_dirs_with_prefix(prefix: &str) -> Vec<std::path::PathBuf> {
        std::fs::read_dir(std::env::temp_dir())
            .map(|rd| {
                rd.flatten()
                    .map(|e| e.path())
                    .filter(|p| {
                        p.file_name()
                            .is_some_and(|n| n.to_string_lossy().starts_with(prefix))
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    // WO 38.4 gap test: two run_task calls launched concurrently WITHOUT
    // artificial distinct ids must land in distinct temp dirs (both alive
    // simultaneously — no first-finisher deletion of a sibling's log) and
    // both error out cleanly against the dead host. multi_thread flavor:
    // this is a real-concurrency test and the production runtime is
    // multi-threaded; on current_thread the two long-lived spawned tasks
    // starve behind the main task's poll loop.
    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn same_ms_double_spawn_gets_distinct_temp_dirs() {
        let _tmp_lock = RUN_TASK_TMP_LOCK.lock().await;
        let mut cfg = Config::default();
        cfg.model.request_timeout_secs = 5;
        let config: SharedConfig = Arc::new(std::sync::RwLock::new(cfg));
        let spawner = Arc::new(InProcessTaskSpawner::new(
            config,
            "test-model".into(),
            "127.0.0.1:1".into(),
            None,
            false,
        ));
        let mk = |p: &'static str| TaskRequest {
            prompt: p.into(),
            persona: "coder".into(),
            model: None,
            max_turns: 1,
            cancel: None,
            owner: None,
            subagent_depth: 0,
            pending_messages: None,
        };
        // Real spawned tasks (an un-awaited future never runs — the poll
        // loop below must observe dirs while both tasks are in flight).
        let spawner_a = spawner.clone();
        let a = tokio::spawn(async move { spawner_a.run_task(mk("doomed a")).await });
        let spawner_b = spawner.clone();
        let b = tokio::spawn(async move { spawner_b.run_task(mk("doomed b")).await });

        // Both dirs must be alive at the same time (the adapter's retry
        // backoff keeps the tasks in flight for seconds).
        // Readiness deadline generous on purpose: under ~50 parallel test
        // processes the spawns starved at the old 10s deadline (WO 47.21
        // residual) — nextest ci-fast grants this test 90s.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        let dirs = loop {
            let dirs = task_temp_dirs_for_this_pid();
            if dirs.len() >= 2 {
                break dirs;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "two live temp dirs never appeared: {dirs:?}"
            );
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        };
        assert_ne!(dirs[0], dirs[1], "same-ms spawns must not share a temp dir");

        let ra = a.await.expect("task a panicked");
        let rb = b.await.expect("task b panicked");
        assert!(ra.is_err() && rb.is_err(), "dead host must fail both tasks");
        let leftover = task_temp_dirs_for_this_pid();
        assert!(leftover.is_empty(), "temp dirs leaked: {leftover:?}");
    }

    // WO 38.4 gap test (worktree half): the same double spawn under
    // worktree isolation must create two distinct worktrees — with the
    // clock tag they collided and stale recovery force-removed the LIVE
    // sibling. Both are cleaned up on error return. multi_thread flavor:
    // real-concurrency test (see the temp-dir variant above).
    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn same_ms_double_spawn_gets_distinct_worktrees() {
        let _tmp_lock = RUN_TASK_TMP_LOCK.lock().await;
        // A throwaway git repo as the worktree root so the test never
        // touches the checkout it runs in.
        let repo = tempfile::tempdir().expect("repo tempdir");
        for args in [
            vec!["init"],
            vec!["config", "user.email", "test@test"],
            vec!["config", "user.name", "test"],
        ] {
            let out = std::process::Command::new("git")
                .args(&args)
                .current_dir(repo.path())
                .output()
                .expect("git spawn");
            assert!(out.status.success(), "git {args:?} failed");
        }
        std::fs::write(repo.path().join("base.txt"), "base\n").unwrap();
        for args in [vec!["add", "."], vec!["commit", "-m", "init"]] {
            let out = std::process::Command::new("git")
                .args(&args)
                .current_dir(repo.path())
                .output()
                .expect("git spawn");
            assert!(out.status.success(), "git {args:?} failed");
        }

        let mut cfg = Config::default();
        cfg.model.request_timeout_secs = 5;
        cfg.session.artifact_policy = crate::shared::ArtifactPolicy::PatchOnly;
        cfg.security.sandbox_dir = Some(repo.path().to_string_lossy().to_string());
        let config: SharedConfig = Arc::new(std::sync::RwLock::new(cfg));
        let spawner = Arc::new(InProcessTaskSpawner::new(
            config,
            "test-model".into(),
            "127.0.0.1:1".into(),
            None,
            false,
        ));
        let mk = |p: &'static str| TaskRequest {
            prompt: p.into(),
            persona: "coder".into(),
            model: None,
            max_turns: 1,
            cancel: None,
            owner: None,
            subagent_depth: 0,
            pending_messages: None,
        };
        let spawner_a = spawner.clone();
        let a = tokio::spawn(async move { spawner_a.run_task(mk("doomed a")).await });
        let spawner_b = spawner.clone();
        let b = tokio::spawn(async move { spawner_b.run_task(mk("doomed b")).await });

        let prefix = format!("kf-code-session-task-{}", std::process::id());
        // Readiness deadline generous on purpose — see the temp-dir variant
        // above (WO 47.21 residual).
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        let dirs = loop {
            let dirs = live_dirs_with_prefix(&prefix);
            if dirs.len() >= 2 {
                break dirs;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "two live worktrees never appeared: {dirs:?}"
            );
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        };
        assert_ne!(dirs[0], dirs[1], "same-ms spawns must not share a worktree");

        let ra = a.await.expect("task a panicked");
        let rb = b.await.expect("task b panicked");
        assert!(ra.is_err() && rb.is_err(), "dead host must fail both tasks");
        for d in &dirs {
            assert!(
                !d.exists(),
                "worktree {d:?} must be removed on error return"
            );
        }
    }

    // ── WO 39.3: agent registry integration ──

    // The spawner's `_` arm consults the registry before falling back to
    // the full toolset. This test proves the agent_def lookup + alias
    // translation produces a correct allowlist WITHOUT a live model: we
    // verify the registry the spawner holds resolves the persona and the
    // translated allowlist filters as expected. The actual run_task path
    // needs a live adapter; the toolset filter is pure data over the
    // registry, so we test the pieces the spawner composes.
    #[test]
    fn spawner_agent_lookup_translates_tool_allowlist() {
        let mut reg = crate::session::agents::AgentRegistry::new();
        reg.register(crate::session::agents::AgentDef {
            name: "reviewer".into(),
            description: "Reviews code".into(),
            system_prompt: "You are a reviewer.".into(),
            // Claude names that map through the alias table.
            tools: vec!["Read".into(), "Grep".into(), "Task".into()],
            model: None,
            max_turns: None,
            isolation: crate::session::agents::AgentIsolation::None,
            background: false,
            permission_mode: None,
        });
        let reg = Arc::new(reg);

        // Simulate the spawner's `_` arm lookup.
        let agent = reg.get("reviewer").expect("agent must resolve");
        let allowlist = crate::session::agents::translate_tool_list(&agent.tools);
        assert_eq!(
            allowlist,
            vec!["read_file", "grep", "task"],
            "Claude tool names must translate to native names, Task→task passthrough"
        );
    }

    // A persona NOT in the registry yields no agent_def, so the `_` arm
    // falls back to the full toolset (empty allowlist = no filter).
    #[test]
    fn spawner_unknown_persona_yields_no_agent_def() {
        let reg = crate::session::agents::AgentRegistry::new();
        let reg = Arc::new(reg);
        assert!(reg.get("mystery-persona").is_none());
    }

    // The spawner's with_agent_registry constructor stores the handle.
    #[test]
    fn spawner_with_agent_registry_stores_handle() {
        let mut reg = crate::session::agents::AgentRegistry::new();
        reg.register(crate::session::agents::AgentDef {
            name: "x".into(),
            description: "x".into(),
            system_prompt: "x".into(),
            tools: vec![],
            model: None,
            max_turns: None,
            isolation: crate::session::agents::AgentIsolation::None,
            background: false,
            permission_mode: None,
        });
        let reg = Arc::new(reg);
        let config: SharedConfig = Arc::new(std::sync::RwLock::new(Config::default()));
        let spawner = InProcessTaskSpawner::with_agent_registry(
            config,
            "test".into(),
            "127.0.0.1:1".into(),
            None,
            false,
            reg.clone(),
        );
        assert!(spawner.agents.get("x").is_some(), "registry must be stored");
    }

    // ── WO 45.31: agent `model` frontmatter wiring ──

    // The spawner's model resolution chain is now:
    //   request.model → agent_def.model → subagent_provider.model → parent.
    // This test mirrors the exact chain the spawner composes (pure data
    // over the registry + request + config — no live adapter needed) and
    // asserts each precedence level. A live run_task needs a model host;
    // the resolution is the logic this WO wires, so we test it directly.
    fn resolve_effective_model(
        request_model: Option<&str>,
        agent_model: Option<&str>,
        subagent_model: Option<&str>,
        parent_model: &str,
    ) -> String {
        let agent_def = agent_model.map(|m| crate::session::agents::AgentDef {
            name: "a".into(),
            description: String::new(),
            system_prompt: String::new(),
            tools: vec![],
            model: Some(m.to_string()),
            max_turns: None,
            isolation: crate::session::agents::AgentIsolation::None,
            background: false,
            permission_mode: None,
        });
        let agent_def_model = agent_def
            .as_ref()
            .and_then(|a| a.model.as_deref().filter(|m| !m.is_empty()));
        request_model
            .or(agent_def_model)
            .or(subagent_model.filter(|m| !m.is_empty()))
            .unwrap_or(parent_model)
            .to_string()
    }

    #[test]
    fn agent_model_field_overrides_parent_when_no_per_call_model() {
        // WO 45.31 gate: agent with `model: claude-sonnet-4` and no
        // per-call override selects claude-sonnet-4 (not the parent).
        let m = resolve_effective_model(None, Some("claude-sonnet-4"), None, "parent-model");
        assert_eq!(
            m, "claude-sonnet-4",
            "agent frontmatter model must win over the parent when no per-call override"
        );
    }

    #[test]
    fn per_call_model_beats_agent_model() {
        let m = resolve_effective_model(
            Some("per-call"),
            Some("claude-sonnet-4"),
            None,
            "parent-model",
        );
        assert_eq!(m, "per-call", "per-call model is the highest priority");
    }

    #[test]
    fn agent_model_beats_subagent_provider_model() {
        let m = resolve_effective_model(
            None,
            Some("claude-sonnet-4"),
            Some("subagent-model"),
            "parent-model",
        );
        assert_eq!(
            m, "claude-sonnet-4",
            "agent frontmatter model beats subagent_provider.model"
        );
    }

    #[test]
    fn no_agent_model_falls_back_to_subagent_provider() {
        let m = resolve_effective_model(None, None, Some("subagent-model"), "parent-model");
        assert_eq!(
            m, "subagent-model",
            "subagent_provider.model is the next fallback"
        );
    }

    #[test]
    fn no_agent_no_subagent_falls_back_to_parent() {
        let m = resolve_effective_model(None, None, None, "parent-model");
        assert_eq!(m, "parent-model", "parent model is the final fallback");
    }

    #[test]
    fn empty_agent_model_is_skipped() {
        // An empty string model field must not block the chain (it is
        // filtered out, falling through to the next level).
        let m = resolve_effective_model(None, Some(""), Some("subagent-model"), "parent-model");
        assert_eq!(
            m, "subagent-model",
            "empty agent model is skipped, not used"
        );
    }

    // ── Model fallback: first-turn failure triggers fallback retry ──

    // When subagent_fallback_model is configured and the primary model
    // fails on turn 0, the fallback path retries with the fallback
    // model. Both models point at a dead host here, so both fail — but
    // the error message must mention BOTH model names, proving the
    // fallback path was taken (not just the primary error).
    #[tokio::test]
    async fn run_task_fallback_triggered_on_first_turn_failure() {
        let _tmp_lock = RUN_TASK_TMP_LOCK.lock().await;
        let mut cfg = Config::default();
        cfg.model.request_timeout_secs = 5;
        cfg.model.subagent_fallback_model = Some("fallback-dead".into());
        let config: SharedConfig = Arc::new(std::sync::RwLock::new(cfg));
        let spawner = InProcessTaskSpawner::new(
            config,
            "primary-dead".into(),
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
            owner: None,
            subagent_depth: 0,
            pending_messages: None,
        };
        let result = spawner.run_task(request).await;
        let err =
            result.expect_err("both primary and fallback models point at a dead host — must error");
        assert!(
            err.contains("primary-dead"),
            "error must mention the primary model; got: {err}"
        );
        assert!(
            err.contains("fallback-dead"),
            "error must mention the fallback model — fallback path was taken; got: {err}"
        );
        let leftover = task_temp_dirs_for_this_pid();
        assert!(
            leftover.is_empty(),
            "temp dir leaked on fallback-exhausted error return: {leftover:?}"
        );
    }

    // When NO fallback is configured, the first-turn failure propagates
    // directly (no fallback model name in the error).
    #[tokio::test]
    async fn run_task_no_fallback_propagates_primary_error() {
        let _tmp_lock = RUN_TASK_TMP_LOCK.lock().await;
        let mut cfg = Config::default();
        cfg.model.request_timeout_secs = 5;
        // subagent_fallback_model left as None (default)
        let config: SharedConfig = Arc::new(std::sync::RwLock::new(cfg));
        let spawner = InProcessTaskSpawner::new(
            config,
            "primary-dead".into(),
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
            owner: None,
            subagent_depth: 0,
            pending_messages: None,
        };
        let result = spawner.run_task(request).await;
        let err = result.expect_err("dead host with no fallback must error");
        assert!(
            err.contains("task turn 0 failed"),
            "error must be the direct turn-0 failure (no fallback attempted); got: {err}"
        );
        assert!(
            !err.contains("fallback"),
            "no fallback configured — error must not mention fallback; got: {err}"
        );
    }

    // The per-subagent-provider fallback_model wins over the top-level
    // subagent_fallback_model. Both point at dead hosts; the error must
    // mention the per-provider fallback name, not the top-level one.
    #[tokio::test]
    async fn run_task_per_provider_fallback_wins_over_top_level() {
        let _tmp_lock = RUN_TASK_TMP_LOCK.lock().await;
        let mut cfg = Config::default();
        cfg.model.request_timeout_secs = 5;
        cfg.model.subagent_fallback_model = Some("top-level-fallback".into());
        cfg.model.subagent_provider.fallback_model = Some("per-provider-fallback".into());
        let config: SharedConfig = Arc::new(std::sync::RwLock::new(cfg));
        let spawner = InProcessTaskSpawner::new(
            config,
            "primary-dead".into(),
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
            owner: None,
            subagent_depth: 0,
            pending_messages: None,
        };
        let result = spawner.run_task(request).await;
        let err = result.expect_err("both models dead — must error");
        assert!(
            err.contains("per-provider-fallback"),
            "per-provider fallback must win over top-level; got: {err}"
        );
        assert!(
            !err.contains("top-level-fallback"),
            "top-level fallback must NOT be used when per-provider is set; got: {err}"
        );
    }
}
