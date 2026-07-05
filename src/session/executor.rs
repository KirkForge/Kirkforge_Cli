use crate::adapters::{tool_call_markup::extract_dsml_tool_calls, ModelAdapter};
use crate::session::access::{
    access_from_config, warn_if_unsandboxed, DenyList, GuardVerdict, PathGuard, ReadGate,
};
use crate::session::adapter_swap::AdapterSwap;
use crate::session::bash_runner::check_bash_command;
use crate::session::carryover::CarryoverProfile;
use crate::session::config::config_diff_summary;
use crate::session::conversation::ConversationLog;
use crate::session::event_bus::{BusEvent, EventBus};
use crate::session::hooks::HookRunner;
use crate::session::prompt::PromptBuilder;
use crate::session::verifier::CorrectionResult;
use crate::session::verifier::{CorrectionLoop, VerifierHandler, VerifierSlots};
use crate::shared::permission::{evaluate, push_rule_unique, PermissionAction};
use crate::shared::{
    read_shared_config, Config, Message, Role, SharedConfig, StreamEvent, ToolDef, ToolInvocation,
    ToolOutcome,
};
use crate::tools::{Tool, ToolContext, UndoStackRef};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::mpsc;

enum IterationOutcome {
    ToolCalls(Vec<ToolInvocation>),

    Finished,

    ParseError,
}

enum ApprovalDecision {
    Approved,
    Denied { reason: String },
    AlwaysApproved,
}

/// Marker emitted by the model at the end of a plan-mode turn. The
/// executor detects this string in the assistant content and surfaces a
/// `TurnEvent::PlanComplete` so the TUI can ask the user to approve
/// exiting plan mode.
const PLAN_COMPLETE_MARKER: &str = "## Plan Complete — ready to implement";

/// Statistics passed to compaction lifecycle hooks (`pre-compact` / `post-compact`).
#[derive(Debug, Clone, Copy)]
struct CompactHookStats {
    message_count: usize,
    preserve_recent: usize,
    original_count: usize,
    result_count: usize,
    dropped_tool_results: usize,
    condensed_assistant_turns: usize,
    summarised_messages: usize,
    strategy: &'static str,
}

pub struct Executor {
    adapter: Box<dyn ModelAdapter>,
    adapter_swap: AdapterSwap,
    hook_runner: HookRunner,
    conversation: ConversationLog,
    prompt_builder: PromptBuilder,
    tools: Box<dyn crate::session::toolset::Toolset>,
    config: SharedConfig,
    cost_tracking: crate::shared::CostTracking,
    model_name: String,
    deny_list: DenyList,
    path_guard: PathGuard,
    read_gate: ReadGate,
    event_bus: EventBus,
    correction_loop: Option<CorrectionLoop>,

    carryover: CarryoverProfile,

    carryover_enabled: bool,

    carryover_target: Option<std::sync::Arc<std::sync::Mutex<CarryoverProfile>>>,

    /// Optional per-session undo stack. Held here so `/undo` can pop
    /// via a control channel without touching the tools directly.
    undo_stack: Option<UndoStackRef>,

    /// When true, the executor only permits read-only discovery tools
    /// (read_file, read_image, grep, glob, and read-only bash). All
    /// mutating tools are denied at the dispatch layer so the model
    /// cannot implement while it is still "thinking". Entered via
    /// `/plan` and exited via `/implement` or user approval.
    plan_mode: bool,

    /// If the conversation log was restored from a checkpoint on open,
    /// this holds the number of recovered messages. It is emitted once
    /// as a `TurnEvent::Recovered` at the start of the first turn so
    /// the TUI/line-mode output can show a status line.
    recovered_messages: Option<usize>,
}

/// Drop guard that fires the `post-turn` hook on every exit path of
/// `run_turn_inner` — Ok, Err, panic unwind, cancel, max-iterations,
/// parse-error second retry.
///
/// The previous design split `run_turn` into a public wrapper that
/// fired the hook manually after `run_turn_inner` returned. That was
/// correct but fragile: every early-return inside `run_turn_inner`
/// had to flow back to the outer wrapper for the hook to fire, and
/// the 17-line block-comment documenting the contract was a code
/// smell.
///
/// This guard owns a *cloned* `HookRunner` (the struct is `Clone` —
/// just a `PathBuf` + `HashSet<String>` + `Config`) and a copy of the
/// current config, so it can outlive the `&mut self` borrows inside
/// `run_turn_inner` and fire on Drop without aliasing.
///
/// Fire-and-forget: `HookRunner::run` wraps `tokio::spawn` internally
/// with a 5s timeout, so Drop never blocks on the hook script.
pub struct PostTurnHookGuard {
    runner: HookRunner,
    config: Config,
}

impl PostTurnHookGuard {
    pub fn new(runner: HookRunner, config: Config) -> Self {
        Self { runner, config }
    }
}

impl Drop for PostTurnHookGuard {
    fn drop(&mut self) {
        // No-op if the hook script doesn't exist; otherwise spawns
        // a tokio task that runs `bash <hooks_dir>/post-turn.sh`
        // with a 5s timeout. Drop completes in microseconds.
        self.runner.run("post-turn", &[], &self.config);
    }
}

impl Executor {
    pub fn with_log(
        adapter: Box<dyn ModelAdapter>,
        tools: Vec<Arc<dyn Tool>>,
        config: Config,
        conversation: ConversationLog,
        carryover_target: Option<std::sync::Arc<std::sync::Mutex<CarryoverProfile>>>,
    ) -> Self {
        Self::with_log_and_undo(
            adapter,
            tools,
            Arc::new(std::sync::RwLock::new(config)),
            conversation,
            carryover_target,
            None,
        )
    }

    /// Constructor that also accepts a shared undo stack and a shared config.
    ///
    /// Does not load plugin hooks or verifiers. Use
    /// [`Self::with_log_and_undo_and_plugins`] to enable plugins.
    pub fn with_log_and_undo(
        adapter: Box<dyn ModelAdapter>,
        tools: Vec<Arc<dyn Tool>>,
        config: SharedConfig,
        conversation: ConversationLog,
        carryover_target: Option<std::sync::Arc<std::sync::Mutex<CarryoverProfile>>>,
        undo_stack: Option<UndoStackRef>,
    ) -> Self {
        Self::with_log_and_undo_and_plugins(
            adapter,
            tools,
            config,
            conversation,
            carryover_target,
            undo_stack,
            None,
        )
    }

    /// Constructor that optionally loads plugin hooks and verifiers from a
    /// `PluginRegistry`.
    pub fn with_log_and_undo_and_plugins(
        mut adapter: Box<dyn ModelAdapter>,
        tools: Vec<Arc<dyn Tool>>,
        config: SharedConfig,
        conversation: ConversationLog,
        carryover_target: Option<std::sync::Arc<std::sync::Mutex<CarryoverProfile>>>,
        undo_stack: Option<UndoStackRef>,
        plugin_registry: Option<&kirkforge_plugin_host::PluginRegistry>,
    ) -> Self {
        let model_name = adapter.model_info().name.clone();
        let config_for_startup = config.clone();
        let cfg = read_shared_config(&config_for_startup);
        let (deny_list, path_guard, read_gate) = access_from_config(&cfg);
        warn_if_unsandboxed(&path_guard);

        // Push the session-level JSON-mode flag down to the active
        // adapter. The trait method has a default no-op for adapters
        // that don't support it, so unknown models (and the test
        // mocks) silently ignore the flag.
        adapter.set_json_mode(cfg.json_mode);

        let adapter_swap = AdapterSwap::new(
            model_name.clone(),
            cfg.ollama_host.clone(),
            None, // model_type_override not available here; set via CLI
        );

        let mut hook_runner = match &cfg.hooks_dir {
            Some(dir) => HookRunner::new(dir.clone()),
            None => HookRunner::default(),
        };
        if let Some(registry) = plugin_registry {
            hook_runner.load_plugin_hooks(registry);
        }

        let event_bus = EventBus::new();

        let carryover_enabled = cfg.carryover_enabled;
        let carryover = if carryover_enabled {
            crate::session::carryover::load_carryover()
        } else {
            CarryoverProfile::default()
        };

        let mut this = Self {
            adapter,
            adapter_swap,
            hook_runner,
            conversation,
            prompt_builder: PromptBuilder::new(),
            tools: Box::new(crate::session::toolset::VecToolset::new("legacy", tools)),
            config,
            cost_tracking: crate::shared::CostTracking::default(),
            model_name,
            deny_list,
            path_guard,
            read_gate,
            event_bus,
            correction_loop: None,
            carryover,
            carryover_enabled,
            carryover_target,
            undo_stack,
            plan_mode: false,
            recovered_messages: None,
        };
        this.init_default_verifiers(plugin_registry);
        this
    }

    /// Record that the conversation log was restored from a checkpoint.
    /// The count is emitted once as `TurnEvent::Recovered` on the first
    /// turn. Call immediately after constructing the executor if the log
    /// opener reported `OpenOutcome::Restored`.
    pub fn set_recovered_messages(&mut self, count: usize) {
        self.recovered_messages = Some(count);
    }

    /// Build a per-tool-call context linked to the turn's cancellation
    /// state. The deadline is derived from the config's per-tool timeout
    /// (default 30 s) unless the tool itself specifies a longer timeout
    /// (e.g. bash) — the executor layer caps the outer wait, and the tool
    /// is responsible for honouring its own internal deadline.
    /// Per-tool-call hard timeout from the shared config. Clamped to
    /// [1, 3600] seconds.
    fn tool_call_timeout(&self) -> std::time::Duration {
        let cfg = read_shared_config(&self.config);
        let secs = cfg.tool_timeout_secs.unwrap_or(30).clamp(1, 3600);
        std::time::Duration::from_secs(secs)
    }

    /// Build a per-tool-call context linked to the turn's cancellation
    /// state and the session's dry-run flag.
    fn tool_context_for_call(&self, cancelled: &std::sync::atomic::AtomicBool) -> ToolContext {
        let dry_run = read_shared_config(&self.config).dry_run;
        ToolContext {
            token: tool_cancel_token(cancelled),
            dry_run,
        }
    }

    /// Replace the shared config with `new` and rebuild access-control
    /// structures from it. Returns a human-readable diff summary.
    fn reload_config(&mut self, new: Config) -> String {
        let old = read_shared_config(&self.config).clone();
        // Update the shared lock. If it is poisoned we still apply the
        // new config locally so this executor keeps running with the
        // fresh rules.
        let fresh = if let Ok(mut cfg) = self.config.write() {
            *cfg = new.clone();
            new
        } else {
            new
        };
        let (deny_list, path_guard, read_gate) = access_from_config(&fresh);
        self.deny_list = deny_list;
        self.path_guard = path_guard;
        self.read_gate = read_gate;
        // JSON-mode changes are applied to the running adapter too.
        self.adapter.set_json_mode(fresh.json_mode);
        config_diff_summary(&old, &fresh)
    }

    pub fn init_default_verifiers(
        &mut self,
        plugin_registry: Option<&kirkforge_plugin_host::PluginRegistry>,
    ) -> usize {
        use crate::session::verifier::{Verdict, Verifier};

        let slots = Arc::new(std::sync::RwLock::new(VerifierSlots::new()));
        let mut count = 0;

        struct SecV;
        #[async_trait::async_trait]
        impl Verifier for SecV {
            fn name(&self) -> &str {
                "security"
            }
            fn priority(&self) -> u8 {
                1
            }
            async fn verify(&self, event: &BusEvent) -> Verdict {
                crate::session::verifier::security::verify_security(event).await
            }
        }
        {
            let mut s = slots.write().unwrap_or_else(|e| e.into_inner());
            if s.register(Arc::new(SecV)).is_ok() {
                count += 1;
            }
        }

        struct LintV;
        #[async_trait::async_trait]
        impl Verifier for LintV {
            fn name(&self) -> &str {
                "lint"
            }
            fn priority(&self) -> u8 {
                2
            }
            async fn verify(&self, event: &BusEvent) -> Verdict {
                crate::session::verifier::lint::verify_lint(event).await
            }
        }
        {
            let mut s = slots.write().unwrap_or_else(|e| e.into_inner());
            if s.register(Arc::new(LintV)).is_ok() {
                count += 1;
            }
        }

        struct GitV;
        #[async_trait::async_trait]
        impl Verifier for GitV {
            fn name(&self) -> &str {
                "git"
            }
            fn priority(&self) -> u8 {
                3
            }
            async fn verify(&self, event: &BusEvent) -> Verdict {
                crate::session::verifier::git::verify_git(event).await
            }
        }
        {
            let mut s = slots.write().unwrap_or_else(|e| e.into_inner());
            if s.register(Arc::new(GitV)).is_ok() {
                count += 1;
            }
        }

        // Register plugin verifiers (Phase 2.4).
        if let Some(registry) = plugin_registry {
            let plugin_verifiers =
                crate::session::verifier::plugin::verifiers_from_registry(registry);
            {
                let mut s = slots.write().unwrap_or_else(|e| e.into_inner());
                for v in plugin_verifiers {
                    if s.register(v).is_ok() {
                        count += 1;
                    }
                }
            }
        }

        let handler = Arc::new(VerifierHandler::new(slots, self.path_guard.clone()));
        let bus = self.event_bus.clone();
        let h = handler.clone();
        match tokio::runtime::Handle::try_current() {
            Ok(rt) => {
                rt.spawn(async move {
                    if let Err(e) = bus.register(h).await {
                        tracing::warn!(error = %e, "failed to register verifier handler on event bus");
                    }
                });
                self.correction_loop = Some(CorrectionLoop::new(handler));
                count
            }
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "no Tokio runtime available; default verifiers will not run"
                );
                0
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn run(
        &mut self,
        mut input_rx: mpsc::UnboundedReceiver<String>,
        event_tx: mpsc::UnboundedSender<TurnEvent>,
        approval_tx: mpsc::UnboundedSender<ApprovalRequest>,
        mut cancel_rx: mpsc::UnboundedReceiver<()>,
        mut resume_rx: mpsc::UnboundedReceiver<ConversationLog>,
        mut compact_rx: mpsc::UnboundedReceiver<()>,
        mut model_rx: mpsc::UnboundedReceiver<String>,
        mut undo_rx: mpsc::UnboundedReceiver<()>,
        mut config_rx: mpsc::UnboundedReceiver<Config>,
        mut plan_rx: mpsc::UnboundedReceiver<bool>,
    ) -> anyhow::Result<()> {
        let cancelled = Arc::new(AtomicBool::new(false));

        // Cancel watcher: drains the cancel channel and sets the
        // shared flag so that an in-flight turn can observe
        // cancellation without waiting for the outer `select!` to
        // poll `cancel_rx`. Previously `run_turn(...).await` was
        // awaited directly in the `input_rx` arm, so `cancel_rx` was
        // not polled while a turn streamed.
        let cancel_event_tx = event_tx.clone();
        let cancel_watcher_cancelled = cancelled.clone();
        tokio::spawn(async move {
            while cancel_rx.recv().await.is_some() {
                cancel_watcher_cancelled.store(true, Ordering::SeqCst);
                if cancel_event_tx
                    .send(TurnEvent::Token("\n⚠️ Generation cancelled\n".into()))
                    .is_err()
                {
                    tracing::warn!("TUI event receiver dropped; cancel watcher exiting");
                    break;
                }
            }
        });

        // Fire session-start hook (fire-and-forget, best-effort)
        self.run_hook("session-start", None, None);

        loop {
            tokio::select! {
                biased; // check control channels first, then input

                // Review.md gap #7 — in-app undo. The TUI sends a
                // signal over `undo_rx`; we pop the executor's undo
                // stack and emit the result as a system token.
                Some(()) = undo_rx.recv() => {
                    let msg = if let Some(ref stack) = self.undo_stack {
                        match stack.lock() {
                            Ok(mut s) => match s.pop() {
                                Ok(Some(r)) => format!(
                                    "↶ Undo: {} ({})",
                                    if r.prev_existed {
                                        format!("restored {}", r.path.display())
                                    } else {
                                        format!("removed newly-created {}", r.path.display())
                                    },
                                    r.kind.as_str()
                                ),
                                Ok(None) => "Nothing to undo.".to_string(),
                                Err(e) => format!("Undo failed: {e}"),
                            },
                            Err(e) => format!("Undo stack mutex poisoned: {e}"),
                        }
                    } else {
                        "Undo unavailable: no undo stack for this session.".to_string()
                    };
                    if event_tx.send(TurnEvent::Token(msg)).is_err() {
                        tracing::warn!("TUI event receiver dropped during /undo; exiting");
                        self.flush_carryover();
                        return Ok(());
                    }
                }
                // Review.md gap #5 — mid-session model swap. The TUI
                // forwards `/model <name>` here; we install the named
                // adapter via `AdapterSwap::force_swap` (which
                // bypasses the smart-router) and emit a confirmation
                // token so the user sees the swap land. The next turn
                // will use the new adapter.
                Some(model_name) = model_rx.recv() => {
                    let cfg_snapshot = read_shared_config(&self.config).clone();
                    let new_name = self
                        .adapter_swap
                        .force_swap(&model_name, &mut self.adapter, &cfg_snapshot);
                    self.model_name = new_name.clone();
                    if event_tx
                        .send(TurnEvent::Token(format!(
                            "🔀 Switched to {new_name}\n"
                        )))
                        .is_err()
                    {
                        tracing::warn!(
                            "TUI event receiver dropped while reporting model swap; exiting"
                        );
                        self.flush_carryover();
                        return Ok(());
                    }
                }
                Some(enable) = plan_rx.recv() => {
                    self.set_plan_mode(enable);
                    let msg = if enable {
                        "📐 Plan mode enabled — only read-only tools are permitted. Type /implement when ready.\n".to_string()
                    } else {
                        match self.exit_plan_mode() {
                            Ok(m) => format!("✅ {m}\n"),
                            Err(e) => {
                                tracing::warn!("exit_plan_mode failed: {}", e);
                                format!("⚠️ Could not exit plan mode: {e}\n")
                            }
                        }
                    };
                    if event_tx.send(TurnEvent::Token(msg)).is_err() {
                        tracing::warn!("TUI event receiver dropped during plan-mode toggle; exiting");
                        self.flush_carryover();
                        return Ok(());
                    }
                }
                Some(new_config) = config_rx.recv() => {
                    let diff_summary = self.reload_config(new_config);
                    let msg = if diff_summary.is_empty() {
                        "🔄 Reloaded config (no changes)\n".to_string()
                    } else {
                        format!("🔄 Reloaded config: {diff_summary}\n")
                    };
                    if event_tx.send(TurnEvent::Token(msg)).is_err() {
                        tracing::warn!("TUI event receiver dropped during config reload; exiting");
                        self.flush_carryover();
                        return Ok(());
                    }
                }
                Some(new_log) = resume_rx.recv() => {

                    self.replace_conversation(new_log);
                    if event_tx.send(TurnEvent::Token("✅ Resumed from fork\n".into())).is_err() {
                        tracing::warn!("TUI event receiver dropped during /resume; exiting");
                        self.flush_carryover();
                        return Ok(());
                    }
                }
                Some(()) = compact_rx.recv() => {
                    let history = self.conversation.all().to_vec();

                    // Snapshot the config fields we need; the guard must
                    // drop before we mutate `self.conversation` below.
                    let (summarize_enabled, summarize_model, ollama_host, preserve_recent) = {
                        let cfg = read_shared_config(&self.config);
                        (
                            cfg.summarize_enabled,
                            cfg.summarize_model.clone(),
                            cfg.ollama_host.clone(),
                            cfg.preserve_recent_messages,
                        )
                    };

                    // Notify lifecycle hooks that compaction is starting.
                    self.run_compact_hook(
                        "pre-compact",
                        CompactHookStats {
                            message_count: history.len(),
                            preserve_recent,
                            original_count: history.len(),
                            result_count: history.len(),
                            dropped_tool_results: 0,
                            condensed_assistant_turns: 0,
                            summarised_messages: 0,
                            strategy: "pending",
                        },
                    );

                    let mut did_summarize = false;
                    let mut compact_stats = None;

                    // Try LLM-based summarization if enabled
                    if summarize_enabled && history.len() > 2 {
                        // Preserve the system anchor and last 4 turns
                        let working_set_size = 8; // 4 user↔assistant pairs ≈ 8 messages
                        let anchor = if !history.is_empty()
                            && matches!(history[0].role, Role::System)
                        {
                            1
                        } else {
                            0
                        };

                        let summarize_from = anchor;
                        let summarize_to = history.len().saturating_sub(working_set_size);
                        if summarize_to > summarize_from + 6
                        {
                            let to_summarize: Vec<Message> = history[summarize_from..summarize_to]
                                .to_vec();
                            if !to_summarize.is_empty() {
                                let summarizer_config = crate::session::prompt::summarizer::SummarizerConfig {
                                    model: summarize_model.clone(),
                                    max_summary_tokens: 500,
                                    min_turns_for_summary: 4,
                                    min_compression_ratio: 0.4,
                                };

                                let result = crate::session::prompt::summarizer::summarize_conversation(
                                    &summarizer_config,
                                    &to_summarize,
                                    &ollama_host,
                                )
                                .await;

                                if let Some(ref summary) = result.summary {
                                    let mut new_msgs = Vec::new();
                                    // Keep the anchor
                                    if anchor > 0 {
                                        new_msgs.push(history[0].clone());
                                    }
                                    // Insert summary as system message
                                    new_msgs.push(Message {
                                        role: Role::System,
                                        content: format!(
                                            "[Context summary — {} messages compressed]\n{}",
                                            result.summarised_messages, summary
                                        ),
                                        ..Default::default()
                                    });
                                    // Append working set
                                    for msg in &history[summarize_to..] {
                                        new_msgs.push(msg.clone());
                                    }

                                    if let Err(e) = self.conversation.replace_all(new_msgs.clone())
                                    {
                                        if event_tx
                                            .send(TurnEvent::Error(format!(
                                                "Summarization failed: {e}"
                                            )))
                                            .is_err()
                                        {
                                            self.flush_carryover();
                                            return Ok(());
                                        }
                                    } else {
                                        did_summarize = true;
                                        compact_stats = Some(CompactHookStats {
                                            message_count: history.len(),
                                            preserve_recent,
                                            original_count: history.len(),
                                            result_count: new_msgs.len(),
                                            dropped_tool_results: 0,
                                            condensed_assistant_turns: 0,
                                            summarised_messages: result.summarised_messages,
                                            strategy: "summarize",
                                        });
                                        let report = TurnEvent::Token(format!(
                                            "🧠 Summarised {}→{} messages ({}→{} tokens, {:.0}% compression)\n",
                                            result.summarised_messages,
                                            if anchor > 0 { 1 + history.len() - summarize_to } else { history.len() - summarize_to },
                                            result.tokens_before,
                                            result.tokens_after,
                                            (1.0 - result.tokens_after as f64 / result.tokens_before.max(1) as f64) * 100.0,
                                        ));
                                        if event_tx.send(report).is_err() {
                                            self.flush_carryover();
                                            return Ok(());
                                        }
                                    }
                                } else if let Some(ref err) = result.error {
                                    // Summarization failed — log and fall through to truncation
                                    tracing::info!(
                                        "Summarization skipped: {} — falling back to truncation",
                                        err
                                    );
                                }
                            }
                        }
                    }

                    // Fall back to naive truncation if summarization didn't run or failed
                    if !did_summarize {
                        let history = self.conversation.all();
                        let result = crate::session::prompt::compact(history, preserve_recent);
                        compact_stats = Some(CompactHookStats {
                            message_count: history.len(),
                            preserve_recent,
                            original_count: result.original_count,
                            result_count: result.compacted_count,
                            dropped_tool_results: result.dropped_tool_results,
                            condensed_assistant_turns: result.condensed_assistant_turns,
                            summarised_messages: 0,
                            strategy: "naive",
                        });
                        let report = if let Err(e) = self.conversation.replace_all(result.new_messages.clone()) {
                            TurnEvent::Error(format!("Compaction failed: {e}"))
                        } else {
                            TurnEvent::CompactionReport {
                                new_messages: result.new_messages.clone(),
                                dropped_tool_results: result.dropped_tool_results,
                                condensed_assistant_turns: result.condensed_assistant_turns,
                                original_count: result.original_count,
                                compacted_count: result.compacted_count,
                            }
                        };
                        if event_tx.send(report).is_err() {
                            tracing::warn!("TUI event receiver dropped during /compact; exiting");
                            self.flush_carryover();
                            return Ok(());
                        }
                    }

                    // Notify lifecycle hooks that compaction finished.
                    if let Some(stats) = compact_stats {
                        self.run_compact_hook("post-compact", stats);
                    }
                }
                Some(input) = input_rx.recv() => {
                    cancelled.store(false, Ordering::SeqCst);
                    // Events stream live into `event_tx` during the turn;
                    // no batch to forward afterwards. A send failure inside
                    // the turn means the TUI dropped its receiver — flush
                    // and exit (the run loop's `input_rx.recv()` arm would
                    // otherwise spin on a closed channel anyway).
                    let result = self.run_turn(&input, &approval_tx, &cancelled, &event_tx).await;
                    if let Err(e) = result {
                        crate::send_or_warn!(event_tx.send(TurnEvent::Error(format!("Turn failed: {e}"))), "TurnEvent receiver dropped; discarding event");
                        tracing::warn!(
                            error = %e,
                            "TUI event receiver may be dropped while reporting turn-failure event"
                        );
                        self.flush_carryover();
                        return Ok(());
                    }
                }
                else => break,
            }
        }
        self.flush_carryover();
        Ok(())
    }

    pub fn conversation_log(&self) -> &ConversationLog {
        &self.conversation
    }

    pub fn replace_conversation(&mut self, new_log: ConversationLog) {
        self.conversation = new_log;
    }

    /// Install a full system-prompt override (e.g. from `--system`).
    /// Pass `None` to revert to the base template. See
    /// `PromptBuilder::set_system_override` for the trade-off (full
    /// override, not append).
    pub fn set_system_override(&mut self, override_prompt: Option<String>) {
        self.prompt_builder.set_system_override(override_prompt);
    }

    /// Enable or disable plan mode. When enabled, only read-only
    /// discovery tools are allowed to execute; mutating tools are
    /// denied at the dispatch layer.
    pub fn set_plan_mode(&mut self, enabled: bool) {
        self.plan_mode = enabled;
    }

    /// Exit plan mode and inject a system message telling the model it
    /// may now implement the plan. Returns the message content so the
    /// caller can echo it to the user if desired.
    pub fn exit_plan_mode(&mut self) -> anyhow::Result<String> {
        self.plan_mode = false;
        let msg = "Plan mode exited — you may now implement the plan.".to_string();
        self.conversation.append(Message {
            role: Role::System,
            content: msg.clone(),
            content_parts: None,
            thinking: None,
            tool_calls: None,
            tool_call_id: None,
            tool_name: None,
            token_count: None,
        })?;
        Ok(msg)
    }

    /// Run a lifecycle hook (fire-and-forget). Wraps HookRunner::run with
    /// common env vars derived from current session state.
    fn run_hook(&self, event: &str, tool_name: Option<&str>, args_json: Option<&str>) {
        let mut env_vars: Vec<(&str, &str)> = Vec::new();
        env_vars.push(("KF_EVENT", event));
        if let Some(name) = tool_name {
            env_vars.push(("KF_TOOL_NAME", name));
        }
        if let Some(json) = args_json {
            env_vars.push(("KF_TOOL_ARGS_JSON", json));
        }
        let cfg = crate::shared::read_shared_config(&self.config);
        self.hook_runner.run(event, &env_vars, &cfg);
    }

    /// Run a compaction lifecycle hook (`pre-compact` / `post-compact`).
    ///
    /// Exposes compact metadata in `KF_TOOL_ARGS_JSON` as a JSON object:
    /// - `message_count` — messages before compaction
    /// - `preserve_recent` — configured tail size
    /// - `original_count` — messages before compaction
    /// - `result_count` — messages after compaction
    /// - `dropped_tool_results` — number of tool results stubbed (naive path)
    /// - `condensed_assistant_turns` — number of assistant turns condensed (naive path)
    /// - `summarised_messages` — number of messages compressed into an LLM summary (summarize path)
    /// - `strategy` — `"summarize"`, `"naive"`, or `"pending"`
    fn run_compact_hook(&self, event: &str, stats: CompactHookStats) {
        let args_json = serde_json::json!({
            "message_count": stats.message_count,
            "preserve_recent": stats.preserve_recent,
            "original_count": stats.original_count,
            "result_count": stats.result_count,
            "dropped_tool_results": stats.dropped_tool_results,
            "condensed_assistant_turns": stats.condensed_assistant_turns,
            "summarised_messages": stats.summarised_messages,
            "strategy": stats.strategy,
        })
        .to_string();
        self.run_hook(event, None, Some(&args_json));
    }

    /// Run a pre-tool hook that is allowed to deny the tool call.
    /// Returns `Some(reason)` if the hook exits with code 2 and denies
    /// the call; returns `None` otherwise (missing hook, success, or any
    /// failure — hooks are fail-open so a broken hook cannot block the
    /// user).
    async fn run_pre_tool_hook(
        &self,
        event: &str,
        tool_name: Option<&str>,
        args_json: Option<&str>,
    ) -> Option<String> {
        let mut env_vars: Vec<(&str, &str)> = Vec::new();
        if let Some(name) = tool_name {
            env_vars.push(("KF_TOOL_NAME", name));
        }
        if let Some(json) = args_json {
            env_vars.push(("KF_TOOL_ARGS_JSON", json));
        }
        let cfg = crate::shared::read_shared_config(&self.config).clone();
        match self.hook_runner.run_decision(event, &env_vars, &cfg).await {
            crate::session::hooks::HookDecision::Allow => None,
            crate::session::hooks::HookDecision::Deny(reason) => Some(reason),
        }
    }

    fn flush_carryover(&mut self) {
        if self.carryover_enabled {
            self.carryover.session_count += 1;
            self.carryover.last_session_time =
                chrono::Local::now().format("%Y-%m-%d %H:%M").to_string();
            self.carryover.refresh_patterns();
            if let Some(ref target) = self.carryover_target {
                if let Ok(mut guard) = target.lock() {
                    *guard = self.carryover.clone();
                }
            }
        }
    }

    fn collect_carryover(&mut self, tc: &ToolInvocation, crs: &[CorrectionResult]) {
        if !self.carryover_enabled {
            return;
        }
        self.carryover.record_tool_call(&tc.name);

        if let Some(path) = tc.arguments.get("path").and_then(|v| v.as_str()) {
            if !path.is_empty() {
                self.carryover.record_path(path);
            }
        }

        if tc.name == "bash" {
            if let Some(cmd) = tc.arguments.get("command").and_then(|v| v.as_str()) {
                if cmd.contains("cargo test")
                    || cmd.contains("cargo check")
                    || cmd.contains("go test")
                    || cmd.contains("npm test")
                    || cmd.contains("pytest")
                    || cmd.contains("make test")
                {
                    self.carryover.record_test_after_change();
                }
            }
        }

        for cr in crs {
            self.carryover.record_verifier_warning(&cr.message);
        }
    }

    /// Run one turn, streaming `TurnEvent`s live to `event_tx` as they are
    /// produced (tokens, tool starts/results, errors) instead of batching
    /// them. The TUI consumes them incrementally — see `drain_turn_events` —
    /// so the chat updates during generation rather than freezing until the
    /// turn ends.
    ///
    /// `run_turn_collecting` is the batched wrapper used by tests and the
    /// non-interactive / persona paths that want a `Vec<TurnEvent>`.
    pub async fn run_turn(
        &mut self,
        user_input: &str,
        approval_sender: &mpsc::UnboundedSender<ApprovalRequest>,
        cancelled: &AtomicBool,
        event_tx: &mpsc::UnboundedSender<TurnEvent>,
    ) -> anyhow::Result<()> {
        // Post-turn hook: fires on every exit path (Ok / Err / panic /
        // cancel / max-iterations / parse-error second retry) via the
        // `PostTurnHookGuard` constructed on the stack below. The guard
        // owns a cloned `HookRunner`, so it can outlive the `&mut self`
        // borrows inside `run_turn_inner` and fire on Drop without
        // aliasing.
        let _hook_guard = PostTurnHookGuard::new(
            self.hook_runner.clone(),
            crate::shared::read_shared_config(&self.config).clone(),
        );
        let result = self
            .run_turn_inner(user_input, approval_sender, cancelled, event_tx)
            .await;
        if result.is_ok() {
            if let Err(e) = self.conversation.checkpoint() {
                tracing::warn!(error = %e, "post-turn checkpoint failed");
                crate::send_or_warn!(
                    event_tx.send(TurnEvent::Error(format!("Checkpoint failed: {e}"))),
                    "TurnEvent receiver dropped; discarding event"
                );
            }
        }
        result
    }

    /// Batched wrapper: run a turn into a private channel and return every
    /// event as a `Vec`. The channel is unbounded, so all events are
    // buffered by the time the turn completes and `try_recv` drains them
    // without blocking. Keeps the old `run_turn` return shape for callers
    // that want a slice (tests, non-interactive line mode, persona runner).
    pub async fn run_turn_collecting(
        &mut self,
        user_input: &str,
        approval_sender: &mpsc::UnboundedSender<ApprovalRequest>,
        cancelled: &AtomicBool,
    ) -> anyhow::Result<Vec<TurnEvent>> {
        let (tx, mut rx) = mpsc::unbounded_channel::<TurnEvent>();
        self.run_turn(user_input, approval_sender, cancelled, &tx)
            .await?;
        let mut events = Vec::new();
        while let Ok(ev) = rx.try_recv() {
            events.push(ev);
        }
        Ok(events)
    }

    async fn run_turn_inner(
        &mut self,
        user_input: &str,
        approval_sender: &mpsc::UnboundedSender<ApprovalRequest>,
        cancelled: &AtomicBool,
        event_tx: &mpsc::UnboundedSender<TurnEvent>,
    ) -> anyhow::Result<()> {
        // --- adapter hot-swap via smart routing ---
        let routing_enabled = read_shared_config(&self.config).routing_enabled;
        if routing_enabled {
            // Clone the config for the swap check so we don't hold the
            // read guard across the mutable adapter borrow.
            let cfg_snapshot = read_shared_config(&self.config).clone();
            let swapped =
                self.adapter_swap
                    .maybe_swap(&cfg_snapshot, &mut self.adapter, user_input);
            if let Some(new_model) = swapped {
                self.model_name = new_model.clone();
                crate::send_or_warn!(
                    event_tx.send(TurnEvent::Token(format!("🔀 Switched to {new_model}\n"))),
                    "TurnEvent receiver dropped; discarding event"
                );
            }
        }

        self.conversation.append(Message {
            role: Role::User,
            content: user_input.to_string(),
            content_parts: None,
            thinking: None,
            tool_calls: None,
            tool_call_id: None,
            tool_name: None,
            token_count: None,
        })?;

        if self.carryover_enabled {
            self.carryover.last_user_message = user_input.to_string();
        }

        // If this session was recovered from a checkpoint, tell the user
        // once before any model output appears.
        if let Some(count) = self.recovered_messages.take() {
            crate::send_or_warn!(
                event_tx.send(TurnEvent::Recovered { messages: count }),
                "TurnEvent receiver dropped; discarding event"
            );
        }

        let mut tool_calls: Vec<ToolInvocation> = Vec::new();
        let mut already_retried_parse = false;

        let max_iterations = read_shared_config(&self.config)
            .max_tool_calls_per_turn
            .max(1);

        for iteration in 0..max_iterations {
            if cancelled.load(Ordering::SeqCst) {
                // The cancel watcher already emitted "Generation
                // cancelled"; just return — events were already sent live.
                return Ok(());
            }

            let outcome = self
                .stream_iteration(approval_sender, cancelled, event_tx, &mut tool_calls)
                .await?;

            match outcome {
                IterationOutcome::Finished => return Ok(()),
                IterationOutcome::ToolCalls(mut tcs) => {
                    for tc in &mut tcs {
                        self.dispatch_tool_call(tc, approval_sender, cancelled, event_tx)
                            .await?;
                    }
                    // Checkpoint after a completed tool batch so a crash
                    // before the next assistant response loses less work.
                    if let Err(e) = self.conversation.checkpoint() {
                        tracing::warn!(error = %e, "post-tool-batch checkpoint failed");
                        crate::send_or_warn!(
                            event_tx.send(TurnEvent::Error(format!("Checkpoint failed: {e}"))),
                            "TurnEvent receiver dropped; discarding event"
                        );
                    }
                }
                IterationOutcome::ParseError => {
                    if !already_retried_parse {
                        already_retried_parse = true;

                        let retry_msg = "Your previous response contained a tool call with malformed JSON arguments. Re-emit ONLY the tool call with the corrected JSON — no additional text, no explanation.";
                        self.conversation.append(Message {
                            role: Role::User,
                            content: retry_msg.into(),
                            content_parts: None,
                            thinking: None,
                            tool_calls: None,
                            tool_call_id: None,
                            tool_name: None,
                            token_count: None,
                        })?;
                        crate::send_or_warn!(
                            event_tx
                                .send(TurnEvent::Token("(JSON parse error, retrying…)\n".into())),
                            "TurnEvent receiver dropped; discarding event"
                        );
                    } else {
                        return Ok(());
                    }
                }
            }

            if iteration + 1 >= max_iterations {
                crate::send_or_warn!(
                    event_tx.send(TurnEvent::Error("Tool call loop limit reached".into())),
                    "TurnEvent receiver dropped; discarding event"
                );
                return Ok(());
            }
        }

        // Post-turn hook fires from the public `run_turn` wrapper
        // after this inner function returns. Do NOT add an explicit
        // `self.run_hook("post-turn", ...)` here — that double-fires
        // the hook on the natural completion path.
        Ok(())
    }

    #[allow(unused_variables)]
    async fn stream_iteration(
        &mut self,
        approval_sender: &mpsc::UnboundedSender<ApprovalRequest>,
        cancelled: &AtomicBool,
        event_tx: &mpsc::UnboundedSender<TurnEvent>,
        tool_calls_out: &mut Vec<ToolInvocation>,
    ) -> anyhow::Result<IterationOutcome> {
        let model_info = self.adapter.model_info();
        let tool_defs: Vec<ToolDef> = self.tools.definitions();
        let tool_names: Vec<&str> = tool_defs.iter().map(|t| t.name).collect();

        let carryover_block = if self.carryover_enabled {
            let block = self.carryover.to_prompt_block();
            if block.is_empty() {
                None
            } else {
                Some(block)
            }
        } else {
            None
        };

        let system = self.prompt_builder.build(
            &model_info.name,
            model_info.supports_thinking,
            &tool_names,
            carryover_block.as_deref(),
        );

        let history = self.conversation.all();
        let tool_results: Vec<Message> = Vec::new(); // sent as part of history

        let messages = self.prompt_builder.build_messages(
            system,
            history,
            model_info.max_context_tokens,
            &tool_results,
        );

        let mut rx = self.adapter.stream(&messages, &tool_defs).await?;

        let mut assistant_content = String::new();
        let mut assistant_thinking = String::new();
        tool_calls_out.clear();

        let mut had_parse_error = false;

        while let Some(event) = rx.recv().await {
            if cancelled.load(Ordering::SeqCst) {
                // The cancel watcher already emitted "Generation
                // cancelled"; flush any partial assistant message
                // and finish the turn.
                if !assistant_content.is_empty()
                    || !tool_calls_out.is_empty()
                    || !assistant_thinking.is_empty()
                {
                    let msg = Message {
                        role: Role::Assistant,
                        content: assistant_content.clone(),
                        thinking: if assistant_thinking.is_empty() {
                            None
                        } else {
                            Some(assistant_thinking.clone())
                        },
                        tool_calls: if tool_calls_out.is_empty() {
                            None
                        } else {
                            Some(tool_calls_out.clone())
                        },
                        ..Default::default()
                    };
                    self.conversation.append(msg)?;
                }
                return Ok(IterationOutcome::Finished);
            }

            match event {
                StreamEvent::Text(t) => {
                    assistant_content.push_str(&t);
                    crate::send_or_warn!(
                        event_tx.send(TurnEvent::Token(t)),
                        "TurnEvent receiver dropped; discarding event"
                    );
                }
                StreamEvent::Thinking(t) => {
                    assistant_thinking.push_str(&t);
                    crate::send_or_warn!(
                        event_tx.send(TurnEvent::Thinking(t)),
                        "TurnEvent receiver dropped; discarding event"
                    );
                }
                StreamEvent::ToolCall(tc) => {
                    tool_calls_out.push(tc);
                }
                StreamEvent::Error(e) => {
                    if e.contains("parse") || e.contains("parseable") {
                        had_parse_error = true;
                    }
                    crate::send_or_warn!(
                        event_tx.send(TurnEvent::Error(e)),
                        "TurnEvent receiver dropped; discarding event"
                    );
                }
                StreamEvent::Done {
                    finish_reason: _,
                    usage,
                } => {
                    // Fallback: some models (notably DeepSeek cloud through
                    // Ollama's /api/chat proxy) emit native DSML markup in
                    // the content stream instead of a valid tool_calls JSON
                    // array. If the adapter delivered no tool calls but the
                    // assistant content contains DSML blocks, extract them,
                    // strip the markup from the persisted message, and treat
                    // the turn as a tool-call turn.
                    if tool_calls_out.is_empty() {
                        let (cleaned, dsml_calls) = extract_dsml_tool_calls(&assistant_content);
                        if !dsml_calls.is_empty() {
                            assistant_content = cleaned;
                            tool_calls_out.extend(dsml_calls);
                        }
                    }

                    let msg = Message {
                        role: Role::Assistant,
                        content: assistant_content.clone(),
                        content_parts: None,
                        thinking: if assistant_thinking.is_empty() {
                            None
                        } else {
                            Some(assistant_thinking.clone())
                        },
                        tool_calls: if tool_calls_out.is_empty() {
                            None
                        } else {
                            Some(tool_calls_out.clone())
                        },
                        tool_call_id: None,
                        tool_name: None,
                        token_count: usage.as_ref().and_then(|u| u.completion_tokens),
                    };
                    self.conversation.append(msg)?;

                    // If we're in plan mode and the assistant signalled
                    // completion, surface a PlanComplete event so the TUI
                    // can ask the user to approve implementation.
                    if self.plan_mode && assistant_content.contains(PLAN_COMPLETE_MARKER) {
                        crate::send_or_warn!(
                            event_tx.send(TurnEvent::PlanComplete),
                            "TurnEvent receiver dropped; discarding event"
                        );
                    }

                    if let Some(ref u) = usage {
                        let prompt = u.prompt_tokens.unwrap_or(0);
                        let completion = u.completion_tokens.unwrap_or(0);
                        let cost = crate::shared::calculate_cost(&self.model_name, u);
                        self.cost_tracking.record_turn(prompt, completion, cost);
                        crate::send_or_warn!(
                            event_tx.send(TurnEvent::CostStats {
                                prompt_tokens: prompt,
                                completion_tokens: completion,
                                turn_cost: cost,
                                cumulative_cost: self.cost_tracking.cumulative_cost,
                            }),
                            "TurnEvent receiver dropped; discarding event"
                        );
                    }

                    if !tool_calls_out.is_empty() {
                        return Ok(IterationOutcome::ToolCalls(tool_calls_out.clone()));
                    }

                    return Ok(if had_parse_error {
                        IterationOutcome::ParseError
                    } else {
                        IterationOutcome::Finished
                    });
                }
            }
        }

        if had_parse_error {
            Ok(IterationOutcome::ParseError)
        } else {
            Ok(IterationOutcome::Finished)
        }
    }

    async fn dispatch_tool_call(
        &mut self,
        tc: &mut ToolInvocation,
        approval_sender: &mpsc::UnboundedSender<ApprovalRequest>,
        cancelled: &std::sync::atomic::AtomicBool,
        event_tx: &mpsc::UnboundedSender<TurnEvent>,
    ) -> anyhow::Result<()> {
        let tool = match self.tools.resolve(&tc.name) {
            Some(t) => t,
            None => {
                let err = format!("Unknown tool: {}", tc.name);
                crate::send_or_warn!(
                    event_tx.send(TurnEvent::Error(err.clone())),
                    "TurnEvent receiver dropped; discarding event"
                );
                self.conversation.append(Message {
                    role: Role::Tool,
                    content: err,
                    tool_call_id: Some(tc.id.clone()),
                    tool_name: Some(tc.name.clone()),
                    ..Default::default()
                })?;
                return Ok(());
            }
        };

        // Plan-mode enforcement: only read-only discovery tools may run.
        // This is a hard guard independent of permission rules so the
        // model cannot mutate code while it is still "thinking".
        if self.plan_mode {
            let allowed = match tc.name.as_str() {
                "read_file" | "read_image" | "grep" | "glob" => true,
                // Job-status queries are read-only and useful while planning.
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
                let reason = format!(
                    "📐 Plan mode blocked {}: only read-only discovery tools are allowed until you type /implement.",
                    tc.name
                );
                crate::send_or_warn!(
                    event_tx.send(TurnEvent::ToolResult {
                        name: tc.name.clone(),
                        output: reason.clone(),
                        success: false,
                    }),
                    "TurnEvent receiver dropped; discarding event"
                );
                self.conversation.append(Message {
                    role: Role::Tool,
                    content: reason,
                    tool_call_id: Some(tc.id.clone()),
                    tool_name: Some(tc.name.clone()),
                    ..Default::default()
                })?;
                return Ok(());
            }
        }

        // Snapshot the permission config so we don't hold the read
        // guard across the mutable self borrows below.
        let (auto_approve, permission_rules) = {
            let cfg = read_shared_config(&self.config);
            (cfg.auto_approve, cfg.permission_rules.clone())
        };
        let is_destructive = matches!(tc.name.as_str(), "write_file" | "edit_file" | "bash");

        // Whether THIS specific bash call is read-only discovery
        // (ls/cat/grep/…). Only meaningful for bash; false otherwise.
        let is_read_only_bash_call = tc.name == "bash"
            && tc
                .arguments
                .get("command")
                .and_then(|v| v.as_str())
                .map(is_read_only_bash)
                .unwrap_or(false);

        // The DEFAULT action — used ONLY when no permission rule matches.
        // The read-only / auto_approve heuristics live HERE, on the
        // default, so they can never override an explicit user rule.
        let default_action = if !is_destructive || is_read_only_bash_call {
            // Non-destructive tools (read_file/grep/glob/read_image) and
            // read-only discovery bash are governed by the path guard and
            // deny-list, not the approval dialog. They don't prompt by
            // default. An explicit `deny`/`ask` rule (below) still applies.
            PermissionAction::Allow
        } else if auto_approve {
            // auto_approve clears writes/edits, but is NOT a blank cheque
            // for non-read-only bash — that still asks by default.
            if tc.name == "bash" {
                PermissionAction::Ask
            } else {
                PermissionAction::Allow
            }
        } else {
            PermissionAction::Ask
        };

        // First-match-wins rules override the default. An explicit `allow`
        // (e.g. one written by the `[A]lways` key) is honored as-is — it is
        // no longer silently downgraded back to Ask under auto_approve.
        let action = evaluate(&permission_rules, &tc.name, &tc.arguments, default_action);

        // Enforce the decision uniformly for EVERY tool. Previously the
        // checks below were gated on `is_destructive`, which meant `deny`
        // rules on read_file/grep/etc. were silently ignored and `ask`
        // rules never prompted. `default_action` already encodes the safe
        // per-tool defaults, so gate purely on `action`.
        let needs_approval = matches!(action, PermissionAction::Ask);

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
            crate::send_or_warn!(
                event_tx.send(TurnEvent::ToolResult {
                    name: tc.name.clone(),
                    output: reason.clone(),
                    success: false,
                }),
                "TurnEvent receiver dropped; discarding event"
            );
            self.conversation.append(Message {
                role: Role::Tool,
                content: reason,
                tool_call_id: Some(tc.id.clone()),
                tool_name: Some(tc.name.clone()),
                ..Default::default()
            })?;
            return Ok(());
        }

        if needs_approval {
            match self.run_approval_flow(tc, approval_sender).await? {
                ApprovalDecision::Approved | ApprovalDecision::AlwaysApproved => {}
                ApprovalDecision::Denied { reason } => {
                    let msg = format!("❌ Approval denied: {reason}");
                    crate::send_or_warn!(
                        event_tx.send(TurnEvent::ToolResult {
                            name: tc.name.clone(),
                            output: msg.clone(),
                            success: false,
                        }),
                        "TurnEvent receiver dropped; discarding event"
                    );
                    self.conversation.append(Message {
                        role: Role::Tool,
                        content: msg,
                        tool_call_id: Some(tc.id.clone()),
                        tool_name: Some(tc.name.clone()),
                        ..Default::default()
                    })?;
                    return Ok(());
                }
            }
        }

        if let Some(denied) = check_deny_list(&self.deny_list, &tc.name, &tc.arguments) {
            crate::send_or_warn!(
                event_tx.send(TurnEvent::ToolResult {
                    name: tc.name.clone(),
                    output: denied.clone(),
                    success: false,
                }),
                "TurnEvent receiver dropped; discarding event"
            );
            self.conversation.append(Message {
                role: Role::Tool,
                content: denied,
                tool_call_id: Some(tc.id.clone()),
                tool_name: Some(tc.name.clone()),
                ..Default::default()
            })?;
            return Ok(());
        }

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

            let verdict = match tc.name.as_str() {
                "read_file" | "read_image" => self.path_guard.check_read(path),
                "write_file" | "edit_file" => self.path_guard.check_write(path),
                _ => {
                    let denied = format!("🔒 Access denied: unsupported file tool '{}'", tc.name);
                    crate::send_or_warn!(
                        event_tx.send(TurnEvent::ToolResult {
                            name: tc.name.clone(),
                            output: denied.clone(),
                            success: false,
                        }),
                        "TurnEvent receiver dropped; discarding event"
                    );
                    self.conversation.append(Message {
                        role: Role::Tool,
                        content: denied,
                        tool_call_id: Some(tc.id.clone()),
                        tool_name: Some(tc.name.clone()),
                        ..Default::default()
                    })?;
                    return Ok(());
                }
            };

            match verdict {
                GuardVerdict::Allowed(resolved) => {
                    if tc.name == "edit_file" {
                        if let GuardVerdict::Denied(msg) =
                            self.read_gate.check_edit(path, &resolved)
                        {
                            let denied = format!("🔒 Access denied: {msg}");
                            crate::send_or_warn!(
                                event_tx.send(TurnEvent::ToolResult {
                                    name: tc.name.clone(),
                                    output: denied.clone(),
                                    success: false,
                                }),
                                "TurnEvent receiver dropped; discarding event"
                            );
                            self.conversation.append(Message {
                                role: Role::Tool,
                                content: denied,
                                tool_call_id: Some(tc.id.clone()),
                                tool_name: Some(tc.name.clone()),
                                ..Default::default()
                            })?;
                            return Ok(());
                        }
                    }

                    if matches!(tc.name.as_str(), "read_file" | "read_image") {
                        self.read_gate.mark_read(&resolved);
                    }

                    let mut run_args = tc.arguments.clone();
                    if let Ok(path_obj) = serde_json::to_value(resolved.to_string_lossy().as_ref())
                    {
                        if let Some(obj) = run_args.as_object_mut() {
                            obj.insert("path".into(), path_obj);
                        }
                    }

                    crate::send_or_warn!(
                        event_tx.send(TurnEvent::ToolStart {
                            name: tc.name.clone(),
                            args: run_args.clone(),
                        }),
                        "TurnEvent receiver dropped; discarding event"
                    );

                    // Pre-tool hook (may deny the operation).
                    let args_json = serde_json::to_string(&run_args).unwrap_or_default();
                    if let Some(reason) = self
                        .run_pre_tool_hook(
                            &format!("pre-tool-{}", tc.name),
                            Some(&tc.name),
                            Some(&args_json),
                        )
                        .await
                    {
                        let denied = format!("❌ Hook denied {}: {}", tc.name, reason);
                        crate::send_or_warn!(
                            event_tx.send(TurnEvent::ToolResult {
                                name: tc.name.clone(),
                                output: denied.clone(),
                                success: false,
                            }),
                            "TurnEvent receiver dropped; discarding event"
                        );
                        self.conversation.append(Message {
                            role: Role::Tool,
                            content: denied,
                            tool_call_id: Some(tc.id.clone()),
                            tool_name: Some(tc.name.clone()),
                            ..Default::default()
                        })?;
                        return Ok(());
                    }

                    let ctx = self.tool_context_for_call(cancelled);
                    let timeout = self.tool_call_timeout();
                    let outcome = tokio::time::timeout(timeout, tool.run(&ctx, run_args.clone()))
                        .await
                        .unwrap_or(ToolOutcome::Failure(crate::shared::ToolError::Timeout {
                            after_secs: timeout.as_secs(),
                        }));
                    let edit_diff =
                        handle_tool_outcome(outcome, tc, event_tx, &mut self.conversation)?;

                    // Post-tool hook
                    self.run_hook(
                        &format!("post-tool-{}", tc.name),
                        Some(&tc.name),
                        Some(&args_json),
                    );

                    let crs = self
                        .emit_tool_event_and_correct(
                            tc, &tc.name, &run_args, None, None, None, edit_diff,
                        )
                        .await;
                    self.collect_carryover(tc, &crs);
                    emit_correction_results(crs, tc, event_tx, &mut self.conversation)?;
                    return Ok(());
                }
                GuardVerdict::Denied(msg) => {
                    let denied = format!("🔒 Access denied: {msg}");
                    crate::send_or_warn!(
                        event_tx.send(TurnEvent::ToolResult {
                            name: tc.name.clone(),
                            output: denied.clone(),
                            success: false,
                        }),
                        "TurnEvent receiver dropped; discarding event"
                    );
                    self.conversation.append(Message {
                        role: Role::Tool,
                        content: denied,
                        tool_call_id: Some(tc.id.clone()),
                        tool_name: Some(tc.name.clone()),
                        ..Default::default()
                    })?;
                    return Ok(());
                }
            }
        }

        if tc.name == "bash" {
            // Pre-process: if `bash_sandbox_workdir` is enabled, force
            // the workdir to the sandbox when the model didn't pass one.
            // We mutate `tc.arguments` in place so the actual `tool.run`
            // call (and the pre/post tool hooks) see the override. The
            // check function below rejects an explicit workdir that
            // points outside the sandbox.
            let bash_sandbox_workdir = read_shared_config(&self.config).bash_sandbox_workdir;
            if bash_sandbox_workdir
                && self.path_guard.sandbox_dir.is_some()
                && tc
                    .arguments
                    .get("workdir")
                    .and_then(|w| w.as_str())
                    .map(|s| s.is_empty())
                    .unwrap_or(true)
            {
                if let Some(obj) = tc.arguments.as_object_mut() {
                    if let Some(ref sandbox) = self.path_guard.sandbox_dir {
                        obj.insert(
                            "workdir".into(),
                            serde_json::Value::String(sandbox.to_string_lossy().to_string()),
                        );
                    }
                }
            }

            if let Some(denied) = check_bash_command(
                &tc.arguments,
                &self.deny_list,
                &self.path_guard,
                bash_sandbox_workdir,
            ) {
                crate::send_or_warn!(
                    event_tx.send(TurnEvent::ToolResult {
                        name: tc.name.clone(),
                        output: denied.clone(),
                        success: false,
                    }),
                    "TurnEvent receiver dropped; discarding event"
                );
                self.conversation.append(Message {
                    role: Role::Tool,
                    content: denied,
                    tool_call_id: Some(tc.id.clone()),
                    tool_name: Some(tc.name.clone()),
                    ..Default::default()
                })?;
                return Ok(());
            }
        }

        // grep/glob: apply the same PathGuard containment that
        // read_file/write_file/edit_file get. Without this, the model
        // could enumerate or search outside the sandbox via grep/glob
        // even when file reads/writes are guarded. See `check_search_path`
        // for why we use a separate check rather than `check_read`.
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
            if let GuardVerdict::Denied(msg) = check_search_path(&self.path_guard, path) {
                let denied = format!("🔒 Access denied: {msg}");
                crate::send_or_warn!(
                    event_tx.send(TurnEvent::ToolResult {
                        name: tc.name.clone(),
                        output: denied.clone(),
                        success: false,
                    }),
                    "TurnEvent receiver dropped; discarding event"
                );
                self.conversation.append(Message {
                    role: Role::Tool,
                    content: denied,
                    tool_call_id: Some(tc.id.clone()),
                    tool_name: Some(tc.name.clone()),
                    ..Default::default()
                })?;
                return Ok(());
            }
        }

        crate::send_or_warn!(
            event_tx.send(TurnEvent::ToolStart {
                name: tc.name.clone(),
                args: tc.arguments.clone(),
            }),
            "TurnEvent receiver dropped; discarding event"
        );

        // Pre-tool hook: gating hooks may deny the call with exit code 2.
        let args_json = serde_json::to_string(&tc.arguments).unwrap_or_default();
        if let Some(reason) = self
            .run_pre_tool_hook(
                &format!("pre-tool-{}", tc.name),
                Some(&tc.name),
                Some(&args_json),
            )
            .await
        {
            crate::send_or_warn!(
                event_tx.send(TurnEvent::ToolResult {
                    name: tc.name.clone(),
                    output: reason.clone(),
                    success: false,
                }),
                "TurnEvent receiver dropped; discarding event"
            );
            self.conversation.append(Message {
                role: Role::Tool,
                content: reason,
                tool_call_id: Some(tc.id.clone()),
                tool_name: Some(tc.name.clone()),
                ..Default::default()
            })?;
            return Ok(());
        }

        let ctx = self.tool_context_for_call(cancelled);
        let timeout = self.tool_call_timeout();
        let outcome = tokio::time::timeout(timeout, tool.run(&ctx, tc.arguments.clone()))
            .await
            .unwrap_or(ToolOutcome::Failure(crate::shared::ToolError::Timeout {
                after_secs: timeout.as_secs(),
            }));

        let (real_exit_code, real_stdout_len, real_stderr_len) = if tc.name == "bash" {
            extract_bash_metrics(&outcome)
        } else {
            (None, None, None)
        };
        let max_tool_result_chars = read_shared_config(&self.config).max_tool_result_chars;
        let outcome = if tc.name == "bash" {
            truncate_tool_output(outcome, max_tool_result_chars)
        } else {
            outcome
        };
        let edit_diff = handle_tool_outcome(outcome, tc, event_tx, &mut self.conversation)?;

        // Post-tool hook
        self.run_hook(
            &format!("post-tool-{}", tc.name),
            Some(&tc.name),
            Some(&args_json),
        );

        let crs = self
            .emit_tool_event_and_correct(
                tc,
                &tc.name,
                &tc.arguments,
                real_exit_code,
                real_stdout_len,
                real_stderr_len,
                edit_diff,
            )
            .await;
        self.collect_carryover(tc, &crs);
        emit_correction_results(crs, tc, event_tx, &mut self.conversation)?;
        Ok(())
    }

    async fn run_approval_flow(
        &mut self,
        tc: &ToolInvocation,
        approval_sender: &mpsc::UnboundedSender<ApprovalRequest>,
    ) -> anyhow::Result<ApprovalDecision> {
        let (response_tx, response_rx) = tokio::sync::oneshot::channel::<ApprovalResponse>();
        approval_sender
            .send(ApprovalRequest {
                tool_name: tc.name.clone(),
                args: tc.arguments.clone(),
                response: ApprovalResponder::new(response_tx),
            })
            .map_err(|_| anyhow::anyhow!("approval channel closed"))?;

        match response_rx.await {
            Ok(ApprovalResponse::Approved) => Ok(ApprovalDecision::Approved),
            Ok(ApprovalResponse::Denied) => Ok(ApprovalDecision::Denied {
                reason: "User denied this operation".into(),
            }),
            Ok(ApprovalResponse::DeniedWithReason(reason)) => {
                Ok(ApprovalDecision::Denied { reason })
            }
            Ok(ApprovalResponse::AlwaysApprove) => {
                let rule = crate::shared::permission::suggest_rule(&tc.name, &tc.arguments);
                if let Ok(mut cfg) = self.config.write() {
                    push_rule_unique(&mut cfg.permission_rules, rule);
                }
                Ok(ApprovalDecision::AlwaysApproved)
            }
            Err(_) => Ok(ApprovalDecision::Denied {
                reason: "Approval channel closed".into(),
            }),
        }
    }

    // The seven-arg clippy limit was already at the boundary for
    // bash metrics (real_exit_code / real_stdout_len / real_stderr_len);
    // the GPT 5.5 #9 fix added the `edit_diff` parameter, pushing us
    // to 8. Suppress locally rather than refactor — the per-tool
    // metrics are only meaningful for `bash` and would be empty
    // Option fields for every other call site.
    #[allow(clippy::too_many_arguments)]
    async fn emit_tool_event_and_correct(
        &self,
        _tc: &ToolInvocation,
        tool_name: &str,
        args: &serde_json::Value,
        real_exit_code: Option<i32>,
        real_stdout_len: Option<usize>,
        real_stderr_len: Option<usize>,
        // The rendered diff from the edit_file tool, when the call
        // succeeded. Used as the `EditEvent.diff` payload so downstream
        // consumers (event-bus handlers, correction loop) see the
        // real unified diff rather than the user's `old_string`
        // (which was what the old code passed — see GPT 5.5
        // review finding #9). `None` for any other tool or for a
        // failed edit; the `args.old_string` fallback inside the
        // match keeps the event populated for the failure case.
        edit_diff: Option<String>,
    ) -> Vec<CorrectionResult> {
        let bus_event = match tool_name {
            "read_file" => {
                let path = args
                    .get("path")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                Some(BusEvent::FileRead(
                    crate::session::event_bus::FileReadEvent {
                        path: std::path::PathBuf::from(&path),
                        size_bytes: 0,
                        truncated: false,
                    },
                ))
            }
            "write_file" => {
                let path = args
                    .get("path")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let content = args.get("content").and_then(|v| v.as_str()).unwrap_or("");
                Some(BusEvent::FileWrite(
                    crate::session::event_bus::FileWriteEvent {
                        path: std::path::PathBuf::from(&path),
                        content_length: content.len(),
                    },
                ))
            }
            "edit_file" => {
                let path = args
                    .get("path")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                // Prefer the rendered diff returned by the tool (the
                // "happy path"); fall back to the user's old_string
                // when the edit failed (no real diff exists) so the
                // event still carries something useful for debugging.
                let diff = edit_diff.unwrap_or_else(|| {
                    args.get("old_string")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string()
                });
                Some(BusEvent::Edit(crate::session::event_bus::EditEvent {
                    path: std::path::PathBuf::from(&path),
                    diff,
                }))
            }
            "bash" => {
                let command = args
                    .get("command")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                Some(BusEvent::BashExec(
                    crate::session::event_bus::BashExecEvent {
                        command,
                        exit_code: real_exit_code.unwrap_or(0),
                        stdout_len: real_stdout_len.unwrap_or(0),
                        stderr_len: real_stderr_len.unwrap_or(0),
                    },
                ))
            }
            _ => None,
        };

        let Some(event) = bus_event else {
            return vec![];
        };

        let handler_results = self.event_bus.dispatch(&event).await;
        for r in handler_results {
            if !r.success {
                tracing::warn!(handler = %r.handler_id, message = %r.message, "event handler failed");
            }
        }

        let Some(ref correction_loop) = self.correction_loop else {
            return vec![];
        };
        correction_loop.run(&event).await
    }
}

pub struct ApprovalRequest {
    pub tool_name: String,
    pub args: serde_json::Value,
    pub response: ApprovalResponder,
}

/// Wrapper around the executor's oneshot approval response sender.
///
/// The oneshot channel is the boundary between the executor (which waits on
/// `response_rx.await`) and the TUI / line-mode handler (which waits for the
/// user). If the handler drops the `Sender` without calling `send`, the
/// executor sees `oneshot::error::RecvError` and surfaces the confusing
/// "Approval channel closed" message. This wrapper guarantees that the
/// sender is always consumed with a response: either the explicit one from
/// the user, or `ApprovalResponse::Denied` if the pending approval is dropped
/// (app shutdown, render error, superseded request, etc.).
#[derive(Debug)]
pub struct ApprovalResponder {
    inner: Option<tokio::sync::oneshot::Sender<ApprovalResponse>>,
}

impl ApprovalResponder {
    pub fn new(tx: tokio::sync::oneshot::Sender<ApprovalResponse>) -> Self {
        Self { inner: Some(tx) }
    }

    /// Consume this responder and send `resp` back to the executor.
    /// Returns `Ok(())` on success, or `Err` if the executor's receiver is
    /// already gone (cancelled / shut down).
    pub fn send(mut self, resp: ApprovalResponse) -> Result<(), ApprovalResponse> {
        if let Some(tx) = self.inner.take() {
            tx.send(resp)
        } else {
            // Already consumed or dropped once; shouldn't happen in normal
            // use, but treat it as a closed channel by returning the value.
            Err(resp)
        }
    }
}

impl Drop for ApprovalResponder {
    fn drop(&mut self) {
        if let Some(tx) = self.inner.take() {
            // Always answer so the executor never blocks forever and never
            // sees the generic "channel closed" error. Use a reasoned denial
            // so the model (and user) can tell this was not an explicit
            // user decision — it happened because the handler dropped the
            // responder without sending a response (e.g. app shutdown, render
            // panic, or a handler bug).
            if tx
                .send(ApprovalResponse::DeniedWithReason(
                    "approval responder dropped without a user decision".into(),
                ))
                .is_err()
            {
                tracing::debug!("approval fallback response receiver already dropped");
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum ApprovalResponse {
    Approved,
    Denied,
    /// Denied with an explanatory message surfaced to the model as the
    /// tool result. Used by non-interactive mode so the agent knows why
    /// a destructive operation was rejected.
    DeniedWithReason(String),
    AlwaysApprove,
}

#[derive(Debug, Clone)]
pub enum TurnEvent {
    Token(String),
    Thinking(String),
    ToolStart {
        name: String,
        args: serde_json::Value,
    },
    ToolResult {
        name: String,
        output: String,
        /// Whether the tool call actually succeeded. `false` covers all
        /// denial paths (path guard, deny list, read-before-edit gate,
        /// approval-deny, dangerous-command block) as well as the tool
        /// itself returning a `ToolOutcome::Error`. The non-interactive
        /// JSON summary uses this to populate the `success` field on
        /// `ToolCallRecord` truthfully (was hardcoded `vec![]` in the
        /// previous implementation — see GPT 5.5 review finding #13).
        success: bool,
    },
    Error(String),
    Verification {
        message: String,
        success: bool,
    },
    CostStats {
        prompt_tokens: usize,
        completion_tokens: usize,
        turn_cost: f64,
        cumulative_cost: f64,
    },

    CompactionReport {
        new_messages: Vec<crate::shared::Message>,
        dropped_tool_results: usize,
        condensed_assistant_turns: usize,
        original_count: usize,
        compacted_count: usize,
    },

    /// Emitted when the assistant's response contains the plan-complete
    /// marker while plan mode is active. The TUI should prompt the user
    /// to approve exiting plan mode (e.g. via `/implement`).
    PlanComplete,

    /// Emitted when the conversation log was corrupt on open and the
    /// executor recovered from the most recent intact checkpoint.
    /// Carries the number of restored messages so the TUI can show a
    /// concise status line.
    Recovered {
        /// Number of messages restored from checkpoint.
        messages: usize,
    },

    /// Progress of an asynchronous Ollama model pull triggered by
    /// `/model <name>` when the model is missing locally. Rendered
    /// in the TUI as a live progress bar.
    PullProgress {
        /// Human-readable status string from the Ollama `/api/pull`
        /// stream (e.g. "pulling manifest", "downloading").
        status: String,
        /// Completed bytes so far; `None` when the server has not
        /// reported a numeric value yet.
        completed: Option<u64>,
        /// Total bytes to download; `None` when the total is unknown.
        total: Option<u64>,
    },
}

/// Cancellation token linked to the turn's `cancelled` atomic. A
/// separate token is created per call so cancelling one tool does not
/// cancel unrelated background work.
fn tool_cancel_token(
    cancelled: &std::sync::atomic::AtomicBool,
) -> tokio_util::sync::CancellationToken {
    let token = tokio_util::sync::CancellationToken::new();
    if cancelled.load(Ordering::SeqCst) {
        token.cancel();
    }
    token
}

const READ_ONLY_COMMANDS: &[&str] = &[
    "ls", "cat", "head", "tail", "pwd", "echo", "printf", "which", "type", "file", "stat", "du",
    "df", "env", "printenv", "true", "false", "dirname", "basename", "realpath", "readlink",
    "grep", "rg", "sort", "wc", "cut", "tr", "uniq", "fold", "nl", "diff", "cmp", "comm", "jq",
    "date", "cal", "whoami", "id", "uname", "hostname", "uptime", "ps", "free", "lscpu", "lsblk",
    "lsof", "dmesg", "nproc", "arch", "tty", "jobs", "help", "find",
];

fn is_read_only_bash(cmd: &str) -> bool {
    let trimmed = cmd.trim();
    if trimmed.is_empty() {
        return true;
    }

    let trimmed_stripped = trimmed.trim_start();
    let first = match trimmed_stripped.find(|c: char| c.is_whitespace()) {
        Some(pos) => &trimmed_stripped[..pos],
        None => trimmed_stripped,
    };

    if !READ_ONLY_COMMANDS.contains(&first) {
        return false;
    }

    // `find` is read-only for discovery, but several flags mutate the
    // filesystem. Require approval for any find command that looks
    // destructive.
    if first == "find" {
        let lowered = trimmed.to_lowercase();
        for flag in [" -delete", " -exec", " -ok", " -fprint", " -fls"] {
            if lowered.contains(flag) {
                return false;
            }
        }
    }

    let rest = &trimmed[first.len()..];

    if rest.contains('>') {
        return false;
    }

    // Every pipe segment must itself be a read-only command. The first
    // segment's command is already validated above; this catches a
    // read-only producer piped into a writing consumer — e.g.
    // `cat list | xargs rm`, `… | tee /etc/file`, `… | sh`. Without this,
    // such a pipeline would be auto-approved despite mutating state.
    for segment in trimmed.split('|') {
        let seg = segment.trim();
        if let Some(word) = seg.split_whitespace().next() {
            if !READ_ONLY_COMMANDS.contains(&word) {
                return false;
            }
        }
    }

    if rest.contains(';') || rest.contains("&&") || rest.contains("||") {
        return false;
    }

    if rest.contains("$(") || rest.contains('`') {
        return false;
    }

    true
}

fn truncate_tool_output(outcome: ToolOutcome, max_chars: usize) -> ToolOutcome {
    if max_chars == 0 {
        return outcome;
    }
    match outcome {
        ToolOutcome::Success { content } => {
            if content.len() > max_chars {
                let mut boundary = max_chars;
                while !content.is_char_boundary(boundary) {
                    boundary -= 1;
                }
                let truncated = format!(
                    "{}...\n[output truncated to {} chars]",
                    &content[..boundary],
                    max_chars
                );
                ToolOutcome::Success { content: truncated }
            } else {
                ToolOutcome::Success { content }
            }
        }
        ToolOutcome::Error { message } => ToolOutcome::Error { message },
        ToolOutcome::Failure(err) => ToolOutcome::Failure(err.clone()),
        other => other,
    }
}

fn extract_bash_metrics(outcome: &ToolOutcome) -> (Option<i32>, Option<usize>, Option<usize>) {
    match outcome {
        ToolOutcome::Success { content } => (Some(0), Some(content.len()), Some(0)),
        ToolOutcome::Error { message } => {
            let exit_code = if message.contains("exited with code") {
                message
                    .split("exited with code ")
                    .nth(1)
                    .and_then(|s| s.split_whitespace().next())
                    .and_then(|s| s.parse::<i32>().ok())
            } else {
                Some(1)
            };
            let (stdout_len, stderr_len) = message
                .find("\nstderr:\n")
                .map(|pos| (pos, message[pos + 9..].len()))
                .unwrap_or((message.len(), 0));
            (exit_code, Some(stdout_len), Some(stderr_len))
        }
        ToolOutcome::Failure(crate::shared::ToolError::Execution {
            exit_code, stderr, ..
        }) => (*exit_code, Some(0), Some(stderr.len())),
        ToolOutcome::Failure(crate::shared::ToolError::Timeout { .. })
        | ToolOutcome::Failure(crate::shared::ToolError::Cancelled) => (Some(1), Some(0), Some(0)),
        ToolOutcome::Failure(_) => (Some(1), Some(0), Some(0)),
        _ => (None, None, None),
    }
}

/// PathGuard-style check for grep/glob search paths.
///
/// `PathGuard::check_read` requires the path to exist, but grep/glob
/// arguments are often glob patterns (`src/**/*.rs`) or directories
/// that may not exist yet. This helper does the deny-list and sandbox
/// containment checks without requiring existence, falling back to
/// the longest existing ancestor for containment.
///
/// This was the source of GPT 5.5's review finding #3 ("PathGuard
/// applied to grep/glob") — without this, a model could enumerate
/// files outside the sandbox via grep/glob even though
/// read/write/edit were guarded.
fn check_search_path(path_guard: &PathGuard, path: &std::path::Path) -> GuardVerdict {
    // 1. Deny list — same as check_read.
    if path_guard.deny_list.is_path_denied(path) {
        return GuardVerdict::Denied(format!("Path denied by deny list: {}", path.display()));
    }

    // 2. Resolve to the longest existing ancestor so glob patterns
    //    still get a containment check. If nothing in the path exists
    //    (e.g. the model is searching a freshly-deleted directory),
    //    fall back to the literal path; the sandbox check below will
    //    deny it because we can't prove containment.
    let check = if path.exists() {
        path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
    } else {
        let mut cur = path.to_path_buf();
        while !cur.exists() {
            if !cur.pop() {
                break;
            }
        }
        cur.canonicalize().unwrap_or_else(|_| path.to_path_buf())
    };

    // 3. Sandbox containment on the resolved ancestor.
    if let Some(ref sandbox) = path_guard.sandbox_dir {
        let sb = match sandbox.canonicalize() {
            Ok(s) => s,
            Err(e) => {
                return GuardVerdict::Denied(format!(
                    "Cannot resolve sandbox dir '{}': {e}",
                    sandbox.display()
                ));
            }
        };
        if !check.starts_with(&sb) {
            return GuardVerdict::Denied(format!(
                "Search path outside sandbox: {}",
                path.display()
            ));
        }
    }

    GuardVerdict::Allowed(path.to_path_buf())
}

fn check_deny_list(
    deny_list: &DenyList,
    tool_name: &str,
    args: &serde_json::Value,
) -> Option<String> {
    match tool_name {
        "read_file" | "read_image" | "write_file" | "edit_file" => {
            if let Some(path) = args.get("path").and_then(|v| v.as_str()) {
                let p = std::path::Path::new(path);
                if deny_list.is_path_denied(p) {
                    return Some(format!("🔒 Path denied by deny list: {path}"));
                }
            }
        }
        "bash" => {}
        "grep" | "glob" => {
            if let Some(path) = args.get("path").and_then(|v| v.as_str()) {
                let p = std::path::Path::new(path);
                if deny_list.is_path_denied(p) {
                    return Some(format!("🔒 Path denied by deny list: {path}"));
                }
            }
        }
        _ => {}
    }
    None
}

/// Process a tool outcome: append the conversation log entry, push a
/// `TurnEvent::ToolResult` for downstream consumers, and (on error) try
/// to surface a recovery hint.
///
/// Returns the rendered diff string when the outcome was a `FileEdit`.
/// This is propagated up to `emit_tool_event_and_correct` so the
/// `BusEvent::Edit` carries the *real* diff, not the user's `old_string`
/// (which is what the previous implementation used — see GPT 5.5
/// review finding #9).
fn handle_tool_outcome(
    outcome: ToolOutcome,
    tc: &ToolInvocation,
    event_tx: &mpsc::UnboundedSender<TurnEvent>,
    conversation: &mut ConversationLog,
) -> anyhow::Result<Option<String>> {
    match outcome {
        ToolOutcome::Success { content } => {
            crate::send_or_warn!(
                event_tx.send(TurnEvent::ToolResult {
                    name: tc.name.clone(),
                    output: content.clone(),
                    success: true,
                }),
                "TurnEvent receiver dropped; discarding event"
            );
            conversation.append(Message {
                role: Role::Tool,
                content,
                tool_call_id: Some(tc.id.clone()),
                tool_name: Some(tc.name.clone()),
                ..Default::default()
            })?;
        }
        ToolOutcome::FileContent { content, .. } => {
            crate::send_or_warn!(
                event_tx.send(TurnEvent::ToolResult {
                    name: tc.name.clone(),
                    output: content.clone(),
                    success: true,
                }),
                "TurnEvent receiver dropped; discarding event"
            );
            conversation.append(Message {
                role: Role::Tool,
                content,
                tool_call_id: Some(tc.id.clone()),
                tool_name: Some(tc.name.clone()),
                ..Default::default()
            })?;
        }
        ToolOutcome::FileEdit { diff, .. } => {
            // Hand the rendered diff to the caller so the
            // BusEvent::Edit event downstream carries the real
            // diff text — see the docstring on this fn.
            crate::send_or_warn!(
                event_tx.send(TurnEvent::ToolResult {
                    name: tc.name.clone(),
                    output: diff.clone(),
                    success: true,
                }),
                "TurnEvent receiver dropped; discarding event"
            );
            conversation.append(Message {
                role: Role::Tool,
                content: diff.clone(),
                tool_call_id: Some(tc.id.clone()),
                tool_name: Some(tc.name.clone()),
                ..Default::default()
            })?;
            return Ok(Some(diff));
        }
        ToolOutcome::GrepMatches {
            path,
            matches,
            total: _,
        } => {
            let output = format_grep_output(&path, &matches);
            crate::send_or_warn!(
                event_tx.send(TurnEvent::ToolResult {
                    name: tc.name.clone(),
                    output: output.clone(),
                    success: true,
                }),
                "TurnEvent receiver dropped; discarding event"
            );
            conversation.append(Message {
                role: Role::Tool,
                content: output,
                tool_call_id: Some(tc.id.clone()),
                tool_name: Some(tc.name.clone()),
                ..Default::default()
            })?;
        }
        ToolOutcome::Error { message } => {
            crate::send_or_warn!(
                event_tx.send(TurnEvent::ToolResult {
                    name: tc.name.clone(),
                    output: format!("Error: {message}"),
                    success: false,
                }),
                "TurnEvent receiver dropped; discarding event"
            );
            conversation.append(Message {
                role: Role::Tool,
                content: format!("Error: {message}"),
                tool_call_id: Some(tc.id.clone()),
                tool_name: Some(tc.name.clone()),
                ..Default::default()
            })?;

            // Attempt error recovery — analyze the error and inject a hint
            if let Some(hint) =
                crate::session::error_recovery::analyze_error(&tc.name, &message, &tc.arguments)
            {
                let recovery_msg = crate::session::error_recovery::build_recovery_message(&hint);
                conversation.append(recovery_msg)?;
            }
        }
        ToolOutcome::Failure(err) => {
            let message = err.to_user_message();
            crate::send_or_warn!(
                event_tx.send(TurnEvent::ToolResult {
                    name: tc.name.clone(),
                    output: format!("Error: {message}"),
                    success: false,
                }),
                "TurnEvent receiver dropped; discarding event"
            );
            conversation.append(Message {
                role: Role::Tool,
                content: format!("Error: {message}"),
                tool_call_id: Some(tc.id.clone()),
                tool_name: Some(tc.name.clone()),
                ..Default::default()
            })?;

            if let Some(hint) =
                crate::session::error_recovery::analyze_error(&tc.name, &message, &tc.arguments)
            {
                let recovery_msg = crate::session::error_recovery::build_recovery_message(&hint);
                conversation.append(recovery_msg)?;
            }
        }
        // `read_image` returns an Image outcome. We materialise it as
        // a `Role::Tool` message with `content_parts: [Image{…}]` set
        // and a short `content` projection that keeps the conversation
        // log human-readable. The PromptBuilder's image-attach step
        // (see `src/session/prompt/mod.rs`) splices the image onto the
        // next user turn so the model actually sees it inline.
        ToolOutcome::Image {
            path,
            mime,
            data_base64,
        } => {
            let projection = format!(
                "[image: {} ({}, {} bytes)]",
                path.display(),
                mime,
                data_base64.len()
            );
            crate::send_or_warn!(
                event_tx.send(TurnEvent::ToolResult {
                    name: tc.name.clone(),
                    output: projection.clone(),
                    success: true,
                }),
                "TurnEvent receiver dropped; discarding event"
            );
            conversation.append(Message {
                role: Role::Tool,
                content: projection,
                content_parts: Some(vec![crate::shared::ContentPart::Image {
                    data_base64,
                    mime,
                }]),
                tool_call_id: Some(tc.id.clone()),
                tool_name: Some(tc.name.clone()),
                ..Default::default()
            })?;
        }
    }
    Ok(None)
}

fn format_grep_output(path: &std::path::Path, matches: &[crate::shared::Match]) -> String {
    let mut out = format!("Matches in {}:\n", path.display());
    for m in matches {
        for ctx in &m.context_before {
            out.push_str(&format!("  {ctx}\n"));
        }
        out.push_str(&format!(">{}: {}\n", m.line_number, m.line));
        for ctx in &m.context_after {
            out.push_str(&format!("  {ctx}\n"));
        }
        out.push('\n');
    }
    out
}

fn emit_correction_results(
    results: Vec<CorrectionResult>,
    tc: &ToolInvocation,
    event_tx: &mpsc::UnboundedSender<TurnEvent>,
    conversation: &mut ConversationLog,
) -> anyhow::Result<()> {
    for cr in &results {
        crate::send_or_warn!(
            event_tx.send(TurnEvent::Verification {
                message: cr.message.clone(),
                success: cr.success,
            }),
            "TurnEvent receiver dropped; discarding event"
        );
        conversation.append(Message {
            role: Role::Tool,
            content: cr.message.clone(),
            tool_call_id: Some(tc.id.clone()),
            tool_name: Some(format!("verifier:{}", cr.verifier)),
            ..Default::default()
        })?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapters::ModelAdapter;
    use crate::shared::test_util::{remove_test_dir, remove_test_file};
    use crate::shared::{
        FinishReason, Message, ModelInfo, Role, StreamEvent, TokenUsage, ToolCallStyle, ToolDef,
        ToolInvocation, ToolOutcome,
    };
    use std::sync::atomic::AtomicBool;
    use std::sync::{Arc, Mutex};
    use tokio::sync::mpsc;

    /// RAII guard that removes a temp file when dropped. Used by plan-mode
    /// tests that need a real, readable file on disk.
    struct CleanupFile(std::path::PathBuf);

    impl Drop for CleanupFile {
        fn drop(&mut self) {
            remove_test_file(&self.0);
        }
    }

    fn never_cancelled() -> &'static AtomicBool {
        static NC: std::sync::LazyLock<AtomicBool> =
            std::sync::LazyLock::new(|| AtomicBool::new(false));
        &NC
    }

    fn cfg(exe: &Executor) -> std::sync::RwLockReadGuard<'_, Config> {
        crate::shared::read_shared_config(&exe.config)
    }

    struct MockAdapter {
        first_events: Vec<StreamEvent>,

        followup_events: Vec<StreamEvent>,
        info: ModelInfo,
        call_count: Arc<Mutex<usize>>,
    }

    impl MockAdapter {
        fn new(events: Vec<StreamEvent>, info: ModelInfo) -> Self {
            Self {
                first_events: events,
                followup_events: vec![
                    StreamEvent::Text("Done.".to_string()),
                    StreamEvent::Done {
                        finish_reason: FinishReason::Stop,
                        usage: None,
                    },
                ],
                info,
                call_count: Arc::new(Mutex::new(0)),
            }
        }

        fn with_followup_events(mut self, events: Vec<StreamEvent>) -> Self {
            self.followup_events = events;
            self
        }
    }

    #[async_trait::async_trait]
    impl ModelAdapter for MockAdapter {
        fn model_info(&self) -> ModelInfo {
            self.info.clone()
        }

        async fn stream(
            &self,
            _messages: &[Message],
            _tools: &[ToolDef],
        ) -> anyhow::Result<mpsc::Receiver<StreamEvent>> {
            let mut count = self.call_count.lock().unwrap();
            let is_first = *count == 0;
            *count += 1;
            drop(count);

            let (tx, rx) = mpsc::channel(64);
            let events = if is_first {
                self.first_events.clone()
            } else {
                self.followup_events.clone()
            };
            tokio::spawn(async move {
                for ev in events {
                    let _ = tx.send(ev).await;
                }
            });
            Ok(rx)
        }
    }

    #[derive(Clone)]
    struct MockTool {
        def: ToolDef,
        captured_args: Arc<Mutex<Option<serde_json::Value>>>,
        outcome: ToolOutcome,
    }

    #[async_trait::async_trait]
    impl Tool for MockTool {
        fn def(&self) -> ToolDef {
            self.def.clone()
        }

        async fn run(&self, _ctx: &ToolContext, args: serde_json::Value) -> ToolOutcome {
            *self.captured_args.lock().unwrap() = Some(args);
            self.outcome.clone()
        }
    }

    fn make_info() -> ModelInfo {
        ModelInfo {
            name: "test-model".into(),
            supports_thinking: false,
            tool_call_format: ToolCallStyle::Native,
            max_context_tokens: 8192,
            recommended_temperature: 0.7,
            supports_images: false,
            supports_cache: false,
        }
    }

    fn make_config(auto_approve: bool) -> Config {
        Config {
            default_model: "test".into(),
            ollama_host: "http://localhost:11434".into(),
            auto_approve,
            truncation_strategy: crate::shared::TruncationStrategy::KeepToolOnly,
            max_tool_result_chars: 4000,
            deny_paths: vec![],
            deny_urls: vec![],
            deny_extensions: vec![],
            allowed_write_dirs: vec![],
            sandbox_dir: None,
            block_dotfiles: false,
            max_file_read_size: 1024 * 1024,
            max_overwrite_size: 1024 * 1024,
            follow_symlinks: false,
            block_binary_reads: false,
            bash_sandbox_workdir: false,
            carryover_enabled: false,
            permission_rules: vec![],
            summarize_model: String::new(),
            summarize_enabled: false,
            routing_enabled: false,
            router_model: String::new(),
            routing_model_map: std::collections::HashMap::new(),
            mcp_servers: vec![],
            bang_requires_approval: false,
            json_mode: false,
            preserve_recent_messages: 2,
            max_plugin_trust: kirkforge_plugin::TrustTier::Shell,
            max_tool_calls_per_turn: 10,
            max_persona_turns: 10,
            hooks_dir: None,
            commit_max_file_size: 5 * 1024 * 1024,
            tool_timeout_secs: Some(30),
            dry_run: false,
            cache_enabled: false,
            cache_dir: None,
        }
    }

    fn make_executor(
        adapter: Box<dyn ModelAdapter>,
        tools: Vec<Arc<dyn Tool>>,
        config: Config,
    ) -> Executor {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let temp_dir = std::env::temp_dir();
        let log_path = temp_dir.join(format!(
            "kirkforge-test-{}-{}.ndjson",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::SeqCst)
        ));
        remove_test_file(&log_path);
        let (conversation, _outcome) = ConversationLog::open(log_path).unwrap();
        Executor::with_log(adapter, tools, config, conversation, None)
    }

    #[tokio::test]
    async fn test_basic_text_response() {
        let adapter = MockAdapter::new(
            vec![
                StreamEvent::Text("Hello ".to_string()),
                StreamEvent::Text("world!".to_string()),
                StreamEvent::Done {
                    finish_reason: FinishReason::Stop,
                    usage: Some(TokenUsage {
                        prompt_tokens: Some(10),
                        completion_tokens: Some(5),
                        cached_tokens: None,
                    }),
                },
            ],
            make_info(),
        );

        let (approval_tx, _approval_rx) = mpsc::unbounded_channel();
        let mut exe = make_executor(Box::new(adapter), vec![], make_config(false));
        let events = exe
            .run_turn_collecting("hello", &approval_tx, never_cancelled())
            .await
            .unwrap();

        let tokens: Vec<_> = events
            .iter()
            .filter_map(|e| match e {
                TurnEvent::Token(t) => Some(t.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(tokens, vec!["Hello ", "world!"]);

        let msgs = exe.conversation.all();
        assert_eq!(msgs.len(), 2); // user + assistant
        assert_eq!(msgs[0].role, Role::User);
        assert_eq!(msgs[0].content, "hello");
        assert_eq!(msgs[1].role, Role::Assistant);
        assert_eq!(msgs[1].content, "Hello world!");
        assert_eq!(msgs[1].token_count, Some(5));
    }

    #[tokio::test]
    async fn test_tool_call_dispatch() {
        let captured = Arc::new(Mutex::new(None));
        let tool = MockTool {
            def: ToolDef {
                name: "echo",
                description: "echo a value",
                parameters: serde_json::json!({"type": "object", "properties": {"val": {"type": "string"}}}),
            },
            captured_args: captured.clone(),
            outcome: ToolOutcome::Success {
                content: "echoed!".into(),
            },
        };

        let adapter = MockAdapter::new(
            vec![
                StreamEvent::Text("Calling tool...".to_string()),
                StreamEvent::ToolCall(ToolInvocation {
                    id: "call-1".into(),
                    name: "echo".into(),
                    arguments: serde_json::json!({"val": "test"}),
                }),
                StreamEvent::Done {
                    finish_reason: FinishReason::ToolCalls,
                    usage: None,
                },
            ],
            make_info(),
        );

        let (approval_tx, _approval_rx) = mpsc::unbounded_channel();
        let mut exe = make_executor(Box::new(adapter), vec![Arc::new(tool)], make_config(true));
        let events = exe
            .run_turn_collecting("use echo", &approval_tx, never_cancelled())
            .await
            .unwrap();

        let has_token = events.iter().any(|e| matches!(e, TurnEvent::Token(_)));
        let has_start = events
            .iter()
            .any(|e| matches!(e, TurnEvent::ToolStart { name, .. } if name == "echo"));
        let has_result = events.iter().any(|e| matches!(e, TurnEvent::ToolResult { name, output, .. } if name == "echo" && output == "echoed!"));

        assert!(has_token, "Should stream text before tool call");
        assert!(has_start, "Should emit ToolStart");
        assert!(has_result, "Should emit ToolResult");

        let called_with = captured.lock().unwrap().take();
        assert!(called_with.is_some(), "Tool should have been called");
        assert_eq!(
            called_with.unwrap().get("val").and_then(|v| v.as_str()),
            Some("test")
        );

        let msgs = exe.conversation.all();
        let tool_msgs: Vec<_> = msgs.iter().filter(|m| m.role == Role::Tool).collect();
        assert_eq!(tool_msgs.len(), 1);
        assert_eq!(tool_msgs[0].content, "echoed!");
    }

    #[tokio::test]
    async fn test_approval_required_for_destructive_bash() {
        // Non-read-only bash (a redirect here) requires approval even
        // when auto_approve is false.
        let captured = Arc::new(Mutex::new(None));
        let tool = MockTool {
            def: ToolDef {
                name: "bash",
                description: "run a command",
                parameters: serde_json::json!({"type": "object", "properties": {"command": {"type": "string"}}}),
            },
            captured_args: captured.clone(),
            outcome: ToolOutcome::Success {
                content: "ran!".into(),
            },
        };

        let adapter = MockAdapter::new(
            vec![
                StreamEvent::ToolCall(ToolInvocation {
                    id: "call-1".into(),
                    name: "bash".into(),
                    arguments: serde_json::json!({"command": "echo x > file.txt"}),
                }),
                StreamEvent::Done {
                    finish_reason: FinishReason::ToolCalls,
                    usage: None,
                },
            ],
            make_info(),
        );

        let (approval_tx, mut approval_rx) = mpsc::unbounded_channel();

        let approval_handle = tokio::spawn(async move {
            let req: ApprovalRequest = approval_rx.recv().await.unwrap();
            assert_eq!(req.tool_name, "bash");
            let _ = req.response.send(ApprovalResponse::Approved);
        });

        let mut exe = make_executor(Box::new(adapter), vec![Arc::new(tool)], make_config(false));
        let events = exe
            .run_turn_collecting("run command", &approval_tx, never_cancelled())
            .await
            .unwrap();

        approval_handle.await.unwrap();

        let result = events.iter().find_map(|e| match e {
            TurnEvent::ToolResult { name, output, .. } => Some((name.as_str(), output.as_str())),
            _ => None,
        });
        assert_eq!(result, Some(("bash", "ran!")));
    }

    #[tokio::test]
    async fn test_read_only_bash_auto_approved() {
        // Read-only bash commands like `ls -la` should run without
        // requiring approval when auto_approve is false.
        let captured = Arc::new(Mutex::new(None));
        let tool = MockTool {
            def: ToolDef {
                name: "bash",
                description: "run a command",
                parameters: serde_json::json!({"type": "object", "properties": {"command": {"type": "string"}}}),
            },
            captured_args: captured.clone(),
            outcome: ToolOutcome::Success {
                content: "ran!".into(),
            },
        };

        let adapter = MockAdapter::new(
            vec![
                StreamEvent::ToolCall(ToolInvocation {
                    id: "call-1".into(),
                    name: "bash".into(),
                    arguments: serde_json::json!({"command": "ls -la"}),
                }),
                StreamEvent::Done {
                    finish_reason: FinishReason::ToolCalls,
                    usage: None,
                },
            ],
            make_info(),
        );

        let (approval_tx, mut approval_rx) = mpsc::unbounded_channel();

        // No approval request should be sent, so the channel stays empty.
        let approval_handle = tokio::spawn(async move {
            let res =
                tokio::time::timeout(std::time::Duration::from_millis(100), approval_rx.recv())
                    .await;
            assert!(
                res.is_err() || res.unwrap().is_none(),
                "read-only bash should not ask for approval"
            );
        });

        let mut exe = make_executor(Box::new(adapter), vec![Arc::new(tool)], make_config(false));
        let events = exe
            .run_turn_collecting("run command", &approval_tx, never_cancelled())
            .await
            .unwrap();

        approval_handle.await.unwrap();

        let result = events.iter().find_map(|e| match e {
            TurnEvent::ToolResult { name, output, .. } => Some((name.as_str(), output.as_str())),
            _ => None,
        });
        assert_eq!(result, Some(("bash", "ran!")));
    }

    #[tokio::test]
    async fn test_approval_denied_for_destructive_tool() {
        let captured = Arc::new(Mutex::new(None));
        let tool = MockTool {
            def: ToolDef {
                name: "bash",
                description: "run a command",
                parameters: serde_json::json!({"type": "object", "properties": {"command": {"type": "string"}}}),
            },
            captured_args: captured.clone(),
            outcome: ToolOutcome::Success {
                content: "ran!".into(),
            },
        };

        let adapter = MockAdapter::new(
            vec![
                StreamEvent::ToolCall(ToolInvocation {
                    id: "call-1".into(),
                    name: "bash".into(),
                    arguments: serde_json::json!({"command": "rm -rf /"}),
                }),
                StreamEvent::Done {
                    finish_reason: FinishReason::ToolCalls,
                    usage: None,
                },
            ],
            make_info(),
        );

        let (approval_tx, mut approval_rx) = mpsc::unbounded_channel();

        let approval_handle = tokio::spawn(async move {
            let req: ApprovalRequest = approval_rx.recv().await.unwrap();
            let _ = req.response.send(ApprovalResponse::Denied);
        });

        let mut exe = make_executor(Box::new(adapter), vec![Arc::new(tool)], make_config(false));
        let events = exe
            .run_turn_collecting("run command", &approval_tx, never_cancelled())
            .await
            .unwrap();

        approval_handle.await.unwrap();

        assert!(
            captured.lock().unwrap().is_none(),
            "Tool should not have been called when denied"
        );

        let denied = events.iter().any(|e| matches!(e, TurnEvent::ToolResult { name, output, .. } if name == "bash" && output.contains("denied")));
        assert!(denied, "Should report that operation was denied");
    }

    #[tokio::test]
    async fn test_always_approve_pushes_permission_rule_not_auto_approve() {
        let captured = Arc::new(Mutex::new(None));
        let tool = MockTool {
            def: ToolDef {
                name: "bash",
                description: "run a command",
                parameters: serde_json::json!({"type": "object", "properties": {"command": {"type": "string"}}}),
            },
            captured_args: captured.clone(),
            outcome: ToolOutcome::Success {
                content: "ran!".into(),
            },
        };

        let adapter = MockAdapter::new(
            vec![
                StreamEvent::ToolCall(ToolInvocation {
                    id: "call-1".into(),
                    name: "bash".into(),
                    arguments: serde_json::json!({"command": "cargo test --release"}),
                }),
                StreamEvent::Done {
                    finish_reason: FinishReason::ToolCalls,
                    usage: None,
                },
            ],
            make_info(),
        );

        let (approval_tx, mut approval_rx) = mpsc::unbounded_channel();
        let approval_handle = tokio::spawn(async move {
            let req: ApprovalRequest = approval_rx.recv().await.unwrap();

            let _ = req.response.send(ApprovalResponse::AlwaysApprove);
        });

        let config = make_config(false);
        assert!(config.permission_rules.is_empty());
        assert!(!config.auto_approve);

        let mut exe = make_executor(Box::new(adapter), vec![Arc::new(tool)], config);
        let _events = exe
            .run_turn_collecting("run tests", &approval_tx, never_cancelled())
            .await
            .unwrap();
        approval_handle.await.unwrap();

        {
            let cfg = cfg(&exe);
            assert_eq!(
                cfg.permission_rules.len(),
                1,
                "AlwaysApprove should have appended exactly one rule, got {:?}",
                cfg.permission_rules
            );
            let r = &cfg.permission_rules[0];
            assert_eq!(r.tool, "bash");
            assert_eq!(r.key, "command");
            assert_eq!(r.pattern, "cargo test --release");
            assert_eq!(r.action, PermissionAction::Allow);
        }

        assert!(
            !cfg(&exe).auto_approve,
            "AlwaysApprove should NOT flip auto_approve — the new rule is the user's intent"
        );
    }

    #[tokio::test]
    async fn test_always_approve_dedups_repeated_calls() {
        let captured = Arc::new(Mutex::new(None));
        let tool = MockTool {
            def: ToolDef {
                name: "bash",
                description: "run a command",
                parameters: serde_json::json!({"type": "object", "properties": {"command": {"type": "string"}}}),
            },
            captured_args: captured.clone(),
            outcome: ToolOutcome::Success {
                content: "ran!".into(),
            },
        };

        let adapter = MockAdapter::new(
            vec![
                StreamEvent::ToolCall(ToolInvocation {
                    id: "call-1".into(),
                    name: "bash".into(),
                    arguments: serde_json::json!({"command": "ls"}),
                }),
                StreamEvent::Done {
                    finish_reason: FinishReason::ToolCalls,
                    usage: None,
                },
            ],
            make_info(),
        );

        let (approval_tx, mut approval_rx) = mpsc::unbounded_channel::<ApprovalRequest>();

        let approval_handle = tokio::spawn(async move {
            while let Some(req) = approval_rx.recv().await {
                let _ = req.response.send(ApprovalResponse::AlwaysApprove);
            }
        });

        let config = make_config(false);

        let mut config = config;
        config
            .permission_rules
            .push(crate::shared::permission::PermissionRule {
                tool: "bash".into(),
                key: "command".into(),
                pattern: "ls".into(),
                action: PermissionAction::Allow,
            });
        assert_eq!(config.permission_rules.len(), 1);

        let mut exe = make_executor(Box::new(adapter), vec![Arc::new(tool)], config);
        let _events = exe
            .run_turn_collecting("list", &approval_tx, never_cancelled())
            .await
            .unwrap();
        drop(approval_tx);
        approval_handle.await.unwrap();

        assert_eq!(
            cfg(&exe).permission_rules.len(),
            1,
            "AlwaysApprove should dedup against an existing identical rule"
        );
    }

    #[tokio::test]
    async fn test_always_approve_does_not_overwrite_existing_deny() {
        let captured = Arc::new(Mutex::new(None));
        let tool = MockTool {
            def: ToolDef {
                name: "bash",
                description: "run a command",
                parameters: serde_json::json!({"type": "object", "properties": {"command": {"type": "string"}}}),
            },
            captured_args: captured.clone(),
            outcome: ToolOutcome::Success {
                content: "ran!".into(),
            },
        };

        let adapter = MockAdapter::new(
            vec![
                StreamEvent::ToolCall(ToolInvocation {
                    id: "call-1".into(),
                    name: "bash".into(),
                    arguments: serde_json::json!({"command": "rm -rf build"}),
                }),
                StreamEvent::Done {
                    finish_reason: FinishReason::ToolCalls,
                    usage: None,
                },
            ],
            make_info(),
        );

        let (approval_tx, mut approval_rx) = mpsc::unbounded_channel::<ApprovalRequest>();

        let approval_handle = tokio::spawn(async move {
            while let Some(req) = approval_rx.recv().await {
                let _ = req.response.send(ApprovalResponse::AlwaysApprove);
            }
        });

        let mut config = make_config(false);

        config
            .permission_rules
            .push(crate::shared::permission::PermissionRule {
                tool: "bash".into(),
                key: "command".into(),
                pattern: "rm -rf build".into(),
                action: PermissionAction::Deny,
            });

        let mut exe = make_executor(Box::new(adapter), vec![Arc::new(tool)], config);
        let _events = exe
            .run_turn_collecting("clean", &approval_tx, never_cancelled())
            .await
            .unwrap();
        drop(approval_tx);
        approval_handle.await.unwrap();

        {
            let cfg = cfg(&exe);
            assert_eq!(cfg.permission_rules.len(), 1);
            assert_eq!(
                cfg.permission_rules[0].action,
                PermissionAction::Deny,
                "Existing Deny should not be overwritten by a new Allow on the same pattern"
            );
        }
    }

    #[tokio::test]
    async fn test_deny_rule_blocks_bash_even_with_auto_approve() {
        let captured = Arc::new(Mutex::new(None));
        let tool = MockTool {
            def: ToolDef {
                name: "bash",
                description: "run a command",
                parameters: serde_json::json!({"type": "object", "properties": {"command": {"type": "string"}}}),
            },
            captured_args: captured.clone(),
            outcome: ToolOutcome::Success {
                content: "ran!".into(),
            },
        };

        let adapter = MockAdapter::new(
            vec![
                StreamEvent::ToolCall(ToolInvocation {
                    id: "call-1".into(),
                    name: "bash".into(),

                    arguments: serde_json::json!({"command": "rm -rf /home/user/build"}),
                }),
                StreamEvent::Done {
                    finish_reason: FinishReason::ToolCalls,
                    usage: None,
                },
            ],
            make_info(),
        );

        let (approval_tx, mut approval_rx) = mpsc::unbounded_channel::<ApprovalRequest>();
        let approval_handle = tokio::spawn(async move {
            while let Some(req) = approval_rx.recv().await {
                let _ = req.response.send(ApprovalResponse::Approved);
            }
        });

        let mut config = make_config(true);
        config
            .permission_rules
            .push(crate::shared::permission::PermissionRule {
                tool: "bash".into(),
                key: "command".into(),
                pattern: "rm -rf **".into(),
                action: PermissionAction::Deny,
            });

        let mut exe = make_executor(Box::new(adapter), vec![Arc::new(tool)], config);
        let events = exe
            .run_turn_collecting("clean build", &approval_tx, never_cancelled())
            .await
            .unwrap();
        drop(approval_tx);
        approval_handle.await.unwrap();

        assert!(
            captured.lock().unwrap().is_none(),
            "Deny rule should prevent the tool from being called even with auto_approve"
        );

        let denied_msg = events.iter().find_map(|e| match e {
            TurnEvent::ToolResult { name, output, .. } if name == "bash" => Some(output.as_str()),
            _ => None,
        });
        assert!(
            denied_msg.is_some_and(|m| m.contains("Permission rule denied")),
            "Expected a permission-rule denial message, got events: {events:?}"
        );
    }

    #[tokio::test]
    async fn test_deny_paths_blocks_write_file_even_with_auto_approve() {
        let captured = Arc::new(Mutex::new(None));
        let tool = MockTool {
            def: ToolDef {
                name: "write_file",
                description: "write to a file",
                parameters: serde_json::json!({"type": "object", "properties": {"path": {"type": "string"}, "content": {"type": "string"}}}),
            },
            captured_args: captured.clone(),
            outcome: ToolOutcome::Success {
                content: "wrote".into(),
            },
        };

        let adapter = MockAdapter::new(
            vec![
                StreamEvent::ToolCall(ToolInvocation {
                    id: "call-1".into(),
                    name: "write_file".into(),
                    arguments: serde_json::json!({
                        "path": "secret/credentials.json",
                        "content": "{\"leaked\": true}"
                    }),
                }),
                StreamEvent::Done {
                    finish_reason: FinishReason::ToolCalls,
                    usage: None,
                },
            ],
            make_info(),
        );

        let (approval_tx, _approval_rx) = mpsc::unbounded_channel();

        let mut config = make_config(true);
        config.deny_paths = vec!["secret/**".into()];

        let mut exe = make_executor(Box::new(adapter), vec![Arc::new(tool)], config);
        let events = exe
            .run_turn_collecting("save creds", &approval_tx, never_cancelled())
            .await
            .unwrap();

        assert!(
            captured.lock().unwrap().is_none(),
            "write_file must be blocked by the path deny-list before the tool runs"
        );

        let denied = events.iter().any(|e| matches!(
            e,
            TurnEvent::ToolResult { name, output, .. } if name == "write_file" && output.contains("denied")
        ));
        assert!(
            denied,
            "Expected a deny-list refusal ToolResult, got events: {events:?}"
        );
    }

    #[tokio::test]
    async fn test_dangerous_shell_blocked_even_with_allow_all_rule() {
        let captured = Arc::new(Mutex::new(None));
        let tool = MockTool {
            def: ToolDef {
                name: "bash",
                description: "run a command",
                parameters: serde_json::json!({"type": "object", "properties": {"command": {"type": "string"}}}),
            },
            captured_args: captured.clone(),
            outcome: ToolOutcome::Success {
                content: "ran!".into(),
            },
        };

        let adapter = MockAdapter::new(
            vec![
                StreamEvent::ToolCall(ToolInvocation {
                    id: "call-1".into(),
                    name: "bash".into(),
                    arguments: serde_json::json!({"command": "rm -rf /"}),
                }),
                StreamEvent::Done {
                    finish_reason: FinishReason::ToolCalls,
                    usage: None,
                },
            ],
            make_info(),
        );

        // No approval request should be sent: the allow-all rule permits the
        // call, but the dangerous-pattern guard blocks it before the tool runs.
        let (approval_tx, mut approval_rx) = mpsc::unbounded_channel::<ApprovalRequest>();
        let approval_handle = tokio::spawn(async move {
            let res =
                tokio::time::timeout(std::time::Duration::from_millis(100), approval_rx.recv())
                    .await;
            assert!(
                res.is_err() || res.unwrap().is_none(),
                "dangerous command should be blocked by the safety gate, not by an approval prompt"
            );
        });

        let mut config = make_config(true);
        config
            .permission_rules
            .push(crate::shared::permission::PermissionRule {
                tool: "*".into(),
                key: "*".into(),
                pattern: String::new(),
                action: PermissionAction::Allow,
            });

        let mut exe = make_executor(Box::new(adapter), vec![Arc::new(tool)], config);
        let events = exe
            .run_turn_collecting("wipe disk", &approval_tx, never_cancelled())
            .await
            .unwrap();
        drop(approval_tx);
        approval_handle.await.unwrap();

        assert!(
            captured.lock().unwrap().is_none(),
            "dangerous shell command must be blocked even when all permission rules allow it"
        );

        let blocked = events.iter().any(|e| matches!(
            e,
            TurnEvent::ToolResult { name, output, .. } if name == "bash" && output.contains("dangerous")
        ));
        assert!(
            blocked,
            "Expected a dangerous-pattern refusal, got events: {events:?}"
        );
    }

    #[tokio::test]
    async fn test_auto_approve_does_not_skip_approval_for_non_read_only_bash() {
        let captured = Arc::new(Mutex::new(None));
        let tool = MockTool {
            def: ToolDef {
                name: "bash",
                description: "run a command",
                parameters: serde_json::json!({"type": "object", "properties": {"command": {"type": "string"}}}),
            },
            captured_args: captured.clone(),
            outcome: ToolOutcome::Success {
                content: "compiled".into(),
            },
        };

        let adapter = MockAdapter::new(
            vec![
                StreamEvent::ToolCall(ToolInvocation {
                    id: "call-1".into(),
                    name: "bash".into(),
                    arguments: serde_json::json!({"command": "cargo build"}),
                }),
                StreamEvent::Done {
                    finish_reason: FinishReason::ToolCalls,
                    usage: None,
                },
            ],
            make_info(),
        );

        let (approval_tx, mut approval_rx) = mpsc::unbounded_channel::<ApprovalRequest>();
        let approval_handle = tokio::spawn(async move {
            let req: ApprovalRequest = approval_rx.recv().await.expect("approval request");
            assert_eq!(req.tool_name, "bash");
            let _ = req.response.send(ApprovalResponse::Approved);
        });

        let mut exe = make_executor(Box::new(adapter), vec![Arc::new(tool)], make_config(true));
        let _events = exe
            .run_turn_collecting("build", &approval_tx, never_cancelled())
            .await
            .unwrap();
        approval_handle.await.unwrap();

        assert!(
            captured.lock().unwrap().is_some(),
            "Tool should have run after the user approved the non-read-only command"
        );
    }

    #[tokio::test]
    async fn test_error_event_forwarded() {
        let adapter = MockAdapter::new(
            vec![
                StreamEvent::Text("Starting...".to_string()),
                StreamEvent::Error("connection lost".to_string()),
            ],
            make_info(),
        );

        let (approval_tx, _approval_rx) = mpsc::unbounded_channel();
        let mut exe = make_executor(Box::new(adapter), vec![], make_config(false));
        let events = exe
            .run_turn_collecting("do it", &approval_tx, never_cancelled())
            .await
            .unwrap();

        let has_error = events
            .iter()
            .any(|e| matches!(e, TurnEvent::Error(msg) if msg == "connection lost"));
        assert!(has_error, "Error events should be forwarded");
    }

    #[tokio::test]
    async fn test_unknown_tool_reported_as_error() {
        let adapter = MockAdapter::new(
            vec![
                StreamEvent::ToolCall(ToolInvocation {
                    id: "call-1".into(),
                    name: "nonexistent_tool".into(),
                    arguments: serde_json::json!({}),
                }),
                StreamEvent::Done {
                    finish_reason: FinishReason::ToolCalls,
                    usage: None,
                },
            ],
            make_info(),
        );

        let (approval_tx, _approval_rx) = mpsc::unbounded_channel();
        let mut exe = make_executor(Box::new(adapter), vec![], make_config(false));
        let events = exe
            .run_turn_collecting("use unknown tool", &approval_tx, never_cancelled())
            .await
            .unwrap();

        let has_error = events
            .iter()
            .any(|e| matches!(e, TurnEvent::Error(msg) if msg.contains("Unknown tool")));
        assert!(has_error, "Unknown tools should produce error events");
    }

    #[tokio::test]
    async fn test_tool_call_loop_capped() {
        let captured = Arc::new(Mutex::new(None));
        let tool = MockTool {
            def: ToolDef {
                name: "looper",
                description: "keeps being called",
                parameters: serde_json::json!({"type": "object", "properties": {"x": {"type": "string"}}}),
            },
            captured_args: captured.clone(),
            outcome: ToolOutcome::Success {
                content: "loop again".into(),
            },
        };

        struct LoopAdapter {
            info: ModelInfo,
            call_count: Arc<Mutex<usize>>,
        }

        #[async_trait::async_trait]
        impl ModelAdapter for LoopAdapter {
            fn model_info(&self) -> ModelInfo {
                self.info.clone()
            }

            async fn stream(
                &self,
                _messages: &[Message],
                _tools: &[ToolDef],
            ) -> anyhow::Result<mpsc::Receiver<StreamEvent>> {
                let (tx, rx) = mpsc::channel(64);
                let count = *self.call_count.lock().unwrap();
                *self.call_count.lock().unwrap() = count + 1;
                tokio::spawn(async move {
                    let _ = tx
                        .send(StreamEvent::ToolCall(ToolInvocation {
                            id: format!("call-{count}"),
                            name: "looper".into(),
                            arguments: serde_json::json!({"x": format!("round-{}", count)}),
                        }))
                        .await;
                    let _ = tx
                        .send(StreamEvent::Done {
                            finish_reason: FinishReason::ToolCalls,
                            usage: None,
                        })
                        .await;
                });
                Ok(rx)
            }
        }

        let call_count = Arc::new(Mutex::new(0usize));
        let adapter = LoopAdapter {
            info: make_info(),
            call_count: call_count.clone(),
        };

        let (approval_tx, _approval_rx) = mpsc::unbounded_channel();
        let mut config = make_config(true);
        config.max_tool_calls_per_turn = 5;
        let mut exe = make_executor(Box::new(adapter), vec![Arc::new(tool)], config);
        let _events = exe
            .run_turn_collecting("loop", &approval_tx, never_cancelled())
            .await
            .unwrap();

        let tool_calls = *call_count.lock().unwrap();
        assert!(
            tool_calls <= 5,
            "Should not exceed configured max_tool_calls_per_turn (was {tool_calls})"
        );
    }

    #[tokio::test]
    async fn test_explicit_allow_rule_honored_under_auto_approve_bash() {
        // Regression: with auto_approve=true, an explicit allow rule for a
        // non-read-only bash command must be honored, not downgraded back to Ask.
        let captured = Arc::new(Mutex::new(None));
        let tool = MockTool {
            def: ToolDef {
                name: "bash",
                description: "run a command",
                parameters: serde_json::json!({"type": "object", "properties": {"command": {"type": "string"}}}),
            },
            captured_args: captured.clone(),
            outcome: ToolOutcome::Success {
                content: "built!".into(),
            },
        };

        let adapter = MockAdapter::new(
            vec![
                StreamEvent::ToolCall(ToolInvocation {
                    id: "call-1".into(),
                    name: "bash".into(),
                    arguments: serde_json::json!({"command": "cargo build"}),
                }),
                StreamEvent::Done {
                    finish_reason: FinishReason::ToolCalls,
                    usage: None,
                },
            ],
            make_info(),
        );

        let (approval_tx, mut approval_rx) = mpsc::unbounded_channel::<ApprovalRequest>();
        let approval_handle = tokio::spawn(async move {
            let res =
                tokio::time::timeout(std::time::Duration::from_millis(100), approval_rx.recv())
                    .await;
            assert!(
                res.is_err() || res.unwrap().is_none(),
                "Explicit allow rule should be honored under auto_approve; no approval prompt expected"
            );
        });

        let mut config = make_config(true);
        config
            .permission_rules
            .push(crate::shared::permission::PermissionRule {
                tool: "bash".into(),
                key: "command".into(),
                pattern: "cargo build".into(),
                action: PermissionAction::Allow,
            });

        let mut exe = make_executor(Box::new(adapter), vec![Arc::new(tool)], config);
        let events = exe
            .run_turn_collecting("build", &approval_tx, never_cancelled())
            .await
            .unwrap();
        drop(approval_tx);
        approval_handle.await.unwrap();

        let result = events.iter().find_map(|e| match e {
            TurnEvent::ToolResult { name, output, .. } => Some((name.as_str(), output.as_str())),
            _ => None,
        });
        assert_eq!(result, Some(("bash", "built!")));
    }

    #[tokio::test]
    async fn test_deny_rule_blocks_read_file() {
        // Regression: deny rules must fire for non-destructive tools too.
        let captured = Arc::new(Mutex::new(None));
        let tool = MockTool {
            def: ToolDef {
                name: "read_file",
                description: "read a file",
                parameters: serde_json::json!({"type": "object", "properties": {"path": {"type": "string"}}}),
            },
            captured_args: captured.clone(),
            outcome: ToolOutcome::Success {
                content: "secret".into(),
            },
        };

        let adapter = MockAdapter::new(
            vec![
                StreamEvent::ToolCall(ToolInvocation {
                    id: "call-1".into(),
                    name: "read_file".into(),
                    arguments: serde_json::json!({"path": "/etc/passwd"}),
                }),
                StreamEvent::Done {
                    finish_reason: FinishReason::ToolCalls,
                    usage: None,
                },
            ],
            make_info(),
        );

        let (approval_tx, _approval_rx) = mpsc::unbounded_channel::<ApprovalRequest>();

        let mut config = make_config(false);
        config
            .permission_rules
            .push(crate::shared::permission::PermissionRule {
                tool: "read_file".into(),
                key: "path".into(),
                pattern: "/etc/**".into(),
                action: PermissionAction::Deny,
            });

        let mut exe = make_executor(Box::new(adapter), vec![Arc::new(tool)], config);
        let events = exe
            .run_turn_collecting("read secrets", &approval_tx, never_cancelled())
            .await
            .unwrap();

        assert!(
            captured.lock().unwrap().is_none(),
            "Deny rule on read_file should prevent the tool from running"
        );

        let denied = events.iter().any(|e| matches!(
            e,
            TurnEvent::ToolResult { name, output, .. } if name == "read_file" && output.contains("Permission rule denied")
        ));
        assert!(denied, "Expected a permission-rule denial for read_file");
    }

    #[test]
    fn test_is_read_only_bash_simple_ls() {
        assert!(is_read_only_bash("ls -la"));
    }

    #[test]
    fn test_is_read_only_bash_pwd() {
        assert!(is_read_only_bash("pwd"));
    }

    #[test]
    fn test_is_read_only_bash_cat() {
        assert!(is_read_only_bash("cat src/main.rs"));
    }

    #[test]
    fn test_is_read_only_bash_grep() {
        assert!(is_read_only_bash("grep -r foo ."));
    }

    #[test]
    fn test_is_read_only_bash_echo() {
        assert!(is_read_only_bash("echo hello world"));
    }

    #[test]
    fn test_is_read_only_bash_find() {
        // Plain find invocations are read-only discovery.
        assert!(is_read_only_bash("find . -name '*.rs'"));
        assert!(is_read_only_bash("find . -type f"));
        assert!(is_read_only_bash("find ."));
    }

    #[test]
    fn test_is_read_only_bash_find_destructive_flags_blocked() {
        // Destructive find flags must still require approval.
        assert!(!is_read_only_bash("find . -delete"));
        assert!(!is_read_only_bash("find . -type f -delete"));
        assert!(!is_read_only_bash("find . -exec rm {} \\;"));
        assert!(!is_read_only_bash("find . -exec sh {} \\;"));
        assert!(!is_read_only_bash("find . -ok rm {} \\;"));
        assert!(!is_read_only_bash("find . -fprint out.txt"));
        assert!(!is_read_only_bash("find . -fls out.txt"));
    }

    #[test]
    fn test_is_read_only_bash_curl_is_not_read_only() {
        assert!(!is_read_only_bash("curl https://example.com"));
    }

    #[test]
    fn test_is_read_only_bash_wget_is_not_read_only() {
        assert!(!is_read_only_bash("wget http://example.com"));
    }

    #[test]
    fn test_is_read_only_bash_pipe_to_sh_blocked() {
        assert!(!is_read_only_bash("cat script | sh"));
        assert!(!is_read_only_bash("cat script | bash"));
    }

    #[test]
    fn test_is_read_only_bash_pipe_to_writer_blocked() {
        // A read-only producer piped into a writing consumer must NOT be
        // auto-approved.
        assert!(!is_read_only_bash("cat list.txt | xargs rm"));
        assert!(!is_read_only_bash("cat data | tee /etc/important"));
        assert!(!is_read_only_bash("cat in | dd of=/dev/sda"));
        assert!(!is_read_only_bash("grep -rl foo . | xargs sed -i 's/a/b/'"));
    }

    #[test]
    fn test_is_read_only_bash_read_only_pipe_allowed() {
        // Pipelines where every stage is read-only stay auto-approved.
        assert!(is_read_only_bash("cat x | grep foo | sort | uniq -c"));
        assert!(is_read_only_bash("ps aux | grep ssh | wc -l"));
    }

    #[test]
    fn test_is_read_only_bash_redirect_blocked() {
        assert!(!is_read_only_bash("ls > out.txt"));
        assert!(!is_read_only_bash("grep foo file >> log.txt"));
    }

    #[test]
    fn test_is_read_only_bash_chaining_blocked() {
        assert!(!is_read_only_bash("ls && rm -rf /"));
        assert!(!is_read_only_bash("cat file; rm file"));
        assert!(!is_read_only_bash("ls || true"));
    }

    #[test]
    fn test_is_read_only_bash_substitution_blocked() {
        assert!(!is_read_only_bash("echo $(rm -rf /)"));
        assert!(!is_read_only_bash("echo `ls`"));
    }

    #[test]
    fn test_is_read_only_bash_unknown_command_not_readonly() {
        assert!(!is_read_only_bash("rm -rf /home/user/temp"));
        assert!(!is_read_only_bash("cargo build"));
        assert!(!is_read_only_bash("python -c 'print(1)'"));
        assert!(!is_read_only_bash("npm install"));
    }

    #[test]
    fn test_is_read_only_bash_word_boundary_no_false_positive() {
        assert!(!is_read_only_bash("scurling is not curl"));

        assert!(is_read_only_bash("cat /etc/hostname"));

        assert!(!is_read_only_bash("cattitude"));
    }

    #[test]
    fn test_is_read_only_bash_empty_is_readonly() {
        assert!(is_read_only_bash(""));
        assert!(is_read_only_bash("   "));
    }

    #[test]
    fn test_is_read_only_bash_ps_and_jobs() {
        assert!(is_read_only_bash("ps aux"));
        assert!(is_read_only_bash("jobs"));
        assert!(is_read_only_bash("help"));
    }

    /// Regression test for GPT 5.5 review finding #9: the
    /// `BusEvent::Edit` used to carry the user's `old_string` as the
    /// `diff` field, which made the event useless to downstream
    /// consumers (verifiers, correction loop, log replay). After the
    /// fix, it should carry the rendered diff that the tool returned
    /// in `ToolOutcome::FileEdit { diff, .. }`. This test wires up a
    /// real `edit_file` tool call, returns a `FileEdit` outcome with a
    /// distinctive diff string, and asserts the dispatched event
    /// matches.
    #[tokio::test]
    async fn test_edit_event_diff_carries_real_diff_not_old_string() {
        use crate::session::event_bus::{EditEvent, EventHandler, EventKind, HandlerResult};

        struct Capture {
            last: Mutex<Option<String>>,
        }
        #[async_trait::async_trait]
        impl EventHandler for Capture {
            fn id(&self) -> &str {
                "capture"
            }
            fn subscribed_kinds(&self) -> Vec<EventKind> {
                vec![EventKind::Edit]
            }
            async fn handle(&self, event: &BusEvent) -> HandlerResult {
                if let BusEvent::Edit(EditEvent { diff, .. }) = event {
                    *self.last.lock().unwrap() = Some(diff.clone());
                }
                HandlerResult {
                    handler_id: "capture".into(),
                    success: true,
                    message: String::new(),
                }
            }
        }

        let captured: Arc<Capture> = Arc::new(Capture {
            last: Mutex::new(None),
        });

        let tool = MockTool {
            def: ToolDef {
                name: "edit_file",
                description: "fake edit",
                parameters: serde_json::json!({"type": "object"}),
            },
            captured_args: Arc::new(Mutex::new(None)),
            outcome: ToolOutcome::FileEdit {
                path: std::path::PathBuf::from("/tmp/edit_event_diff_test.txt"),
                diff: "--- a\n+++ b\n-old line\n+new line".to_string(),
            },
        };

        let adapter = MockAdapter::new(
            vec![
                StreamEvent::ToolCall(ToolInvocation {
                    id: "call-edit".into(),
                    name: "edit_file".into(),
                    arguments: serde_json::json!({
                        "path": "/tmp/edit_event_diff_test.txt",
                        "old_string": "old line",
                        "new_string": "new line",
                    }),
                }),
                StreamEvent::Done {
                    finish_reason: FinishReason::ToolCalls,
                    usage: None,
                },
            ],
            make_info(),
        );

        let (approval_tx, _approval_rx) = mpsc::unbounded_channel();
        let mut exe = make_executor(Box::new(adapter), vec![Arc::new(tool)], make_config(true));
        exe.event_bus
            .register(captured.clone() as Arc<dyn EventHandler>)
            .await
            .unwrap();
        // The read-before-edit gate would otherwise deny the edit
        // before the tool runs (and before the EditEvent is emitted).
        // Mark the path as already read so we exercise the diff path.
        exe.read_gate
            .mark_read(&std::path::PathBuf::from("/tmp/edit_event_diff_test.txt"));

        let _events = exe
            .run_turn_collecting("edit it", &approval_tx, never_cancelled())
            .await
            .unwrap();

        let last = captured.last.lock().unwrap().clone();
        let got = last.expect("EditEvent should have been dispatched");
        assert!(
            got.contains("--- a")
                && got.contains("+++ b")
                && got.contains("-old line")
                && got.contains("+new line"),
            "EditEvent.diff should be the rendered diff, got: {got:?}"
        );
        assert!(
            got.starts_with("---") || got.contains("\n---"),
            "diff should start with --- header, got: {got:?}"
        );
    }

    /// Smoke test for `PostTurnHookGuard`. Constructs a guard with the
    /// default `HookRunner` and lets it fall out of scope. The
    /// `HookRunner::run` call inside `Drop` is fire-and-forget and
    /// (in the absence of a real `~/.local/share/kirkforge/hooks/
    /// post-turn.sh`) is a no-op, so this test exercises construction
    /// and Drop without making any external assumptions.
    ///
    /// The real value is at compile time: if `PostTurnHookGuard` ever
    /// stops being `pub`, or `HookRunner` stops being `Clone`, this
    /// test fails to build — catching the regression before it
    /// silently breaks the post-turn hook fire path.
    #[test]
    fn post_turn_guard_constructs_and_drops() {
        let _guard = PostTurnHookGuard::new(HookRunner::default(), Config::default());
    }

    /// `reload_config` rebuilds access control from a new config and
    /// reports the changed fields. This exercises the hot-reload path
    /// without needing a live TUI or SIGHUP signal.
    #[test]
    fn reload_config_rebuilds_and_reports_changes() {
        let adapter = MockAdapter::new(vec![], make_info());
        let mut exe = make_executor(Box::new(adapter), vec![], make_config(false));

        let mut new_config = make_config(false);
        new_config.default_model = "qwen2.5:14b".into();
        new_config.json_mode = true;
        new_config.carryover_enabled = true;

        let summary = exe.reload_config(new_config.clone());

        assert!(
            summary.contains("default_model")
                || summary.contains("json_mode")
                || summary.contains("carryover_enabled"),
            "reload_config should report changed high-impact fields, got: {summary}"
        );

        // The shared lock should hold the new values.
        let cfg = cfg(&exe);
        assert_eq!(cfg.default_model, "qwen2.5:14b");
        assert!(cfg.json_mode);
        assert!(cfg.carryover_enabled);
    }

    #[tokio::test]
    async fn test_plan_mode_blocks_write_file() {
        let captured = Arc::new(Mutex::new(None));
        let tool = MockTool {
            def: ToolDef {
                name: "write_file",
                description: "write a file",
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "path": {"type": "string"},
                        "content": {"type": "string"}
                    }
                }),
            },
            captured_args: captured.clone(),
            outcome: ToolOutcome::Success {
                content: "wrote".into(),
            },
        };

        let adapter = MockAdapter::new(
            vec![
                StreamEvent::ToolCall(ToolInvocation {
                    id: "call-1".into(),
                    name: "write_file".into(),
                    arguments: serde_json::json!({
                        "path": "/tmp/plan_mode_test.txt",
                        "content": "hello"
                    }),
                }),
                StreamEvent::Done {
                    finish_reason: FinishReason::ToolCalls,
                    usage: None,
                },
            ],
            make_info(),
        );

        let (approval_tx, _approval_rx) = mpsc::unbounded_channel();
        let mut exe = make_executor(Box::new(adapter), vec![Arc::new(tool)], make_config(true));
        exe.set_plan_mode(true);

        let events = exe
            .run_turn_collecting("write something", &approval_tx, never_cancelled())
            .await
            .unwrap();

        assert!(
            captured.lock().unwrap().is_none(),
            "write_file must not run while plan mode is active"
        );
        let blocked = events.iter().any(|e| {
            matches!(
                e,
                TurnEvent::ToolResult { name, output, .. }
                    if name == "write_file" && output.contains("Plan mode blocked")
            )
        });
        assert!(blocked, "Expected plan-mode denial, got events: {events:?}");
    }

    #[tokio::test]
    async fn test_plan_mode_blocks_non_read_only_bash() {
        let captured = Arc::new(Mutex::new(None));
        let tool = MockTool {
            def: ToolDef {
                name: "bash",
                description: "run a command",
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {"command": {"type": "string"}}
                }),
            },
            captured_args: captured.clone(),
            outcome: ToolOutcome::Success {
                content: "ran".into(),
            },
        };

        let adapter = MockAdapter::new(
            vec![
                StreamEvent::ToolCall(ToolInvocation {
                    id: "call-1".into(),
                    name: "bash".into(),
                    arguments: serde_json::json!({"command": "cargo build"}),
                }),
                StreamEvent::Done {
                    finish_reason: FinishReason::ToolCalls,
                    usage: None,
                },
            ],
            make_info(),
        );

        let (approval_tx, _approval_rx) = mpsc::unbounded_channel();
        let mut exe = make_executor(Box::new(adapter), vec![Arc::new(tool)], make_config(true));
        exe.set_plan_mode(true);

        let events = exe
            .run_turn_collecting("build", &approval_tx, never_cancelled())
            .await
            .unwrap();

        assert!(
            captured.lock().unwrap().is_none(),
            "non-read-only bash must not run while plan mode is active"
        );
        let blocked = events.iter().any(|e| {
            matches!(
                e,
                TurnEvent::ToolResult { name, output, .. }
                    if name == "bash" && output.contains("Plan mode blocked")
            )
        });
        assert!(blocked, "Expected plan-mode denial, got events: {events:?}");
    }

    #[tokio::test]
    async fn test_plan_mode_allows_read_file() {
        let tmp = std::env::temp_dir().join(format!(
            "kirkforge_plan_read_test_{}.txt",
            std::process::id()
        ));
        std::fs::write(&tmp, "file contents").expect("write temp file");
        let _cleanup = CleanupFile(tmp.clone());

        let captured = Arc::new(Mutex::new(None));
        let tool = MockTool {
            def: ToolDef {
                name: "read_file",
                description: "read a file",
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {"path": {"type": "string"}}
                }),
            },
            captured_args: captured.clone(),
            outcome: ToolOutcome::Success {
                content: "file contents".into(),
            },
        };

        let adapter = MockAdapter::new(
            vec![
                StreamEvent::ToolCall(ToolInvocation {
                    id: "call-1".into(),
                    name: "read_file".into(),
                    arguments: serde_json::json!({"path": tmp.to_string_lossy()}),
                }),
                StreamEvent::Done {
                    finish_reason: FinishReason::ToolCalls,
                    usage: None,
                },
            ],
            make_info(),
        );

        let (approval_tx, _approval_rx) = mpsc::unbounded_channel();
        let mut exe = make_executor(Box::new(adapter), vec![Arc::new(tool)], make_config(true));
        exe.set_plan_mode(true);

        let events = exe
            .run_turn_collecting("read something", &approval_tx, never_cancelled())
            .await
            .unwrap();

        assert!(
            captured.lock().unwrap().is_some(),
            "read_file should run in plan mode"
        );
        let allowed = events.iter().any(|e| {
            matches!(
                e,
                TurnEvent::ToolResult { name, output, .. }
                    if name == "read_file" && output == "file contents"
            )
        });
        assert!(allowed, "Expected read_file result, got events: {events:?}");
    }

    #[tokio::test]
    async fn test_read_image_honours_path_guard_size_limit() {
        let tmp = std::env::temp_dir().join(format!(
            "kirkforge_oversized_image_test_{}.png",
            std::process::id()
        ));
        // Write one byte over the default 1 MiB max_file_read_size.
        let oversized = vec![0xFF; 1024 * 1024 + 1];
        std::fs::write(&tmp, oversized).expect("write oversized image");
        let _cleanup = CleanupFile(tmp.clone());

        let captured = Arc::new(Mutex::new(None));
        let tool = MockTool {
            def: ToolDef {
                name: "read_image",
                description: "read image",
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {"path": {"type": "string"}}
                }),
            },
            captured_args: captured.clone(),
            outcome: ToolOutcome::Image {
                path: tmp.clone(),
                mime: "image/png".into(),
                data_base64: String::new(),
            },
        };

        let adapter = MockAdapter::new(
            vec![
                StreamEvent::ToolCall(ToolInvocation {
                    id: "call-1".into(),
                    name: "read_image".into(),
                    arguments: serde_json::json!({"path": tmp.to_string_lossy()}),
                }),
                StreamEvent::Done {
                    finish_reason: FinishReason::ToolCalls,
                    usage: None,
                },
            ],
            make_info(),
        );

        let (approval_tx, _approval_rx) = mpsc::unbounded_channel();
        let mut exe = make_executor(Box::new(adapter), vec![Arc::new(tool)], make_config(true));
        let events = exe
            .run_turn_collecting("read image", &approval_tx, never_cancelled())
            .await
            .unwrap();

        assert!(
            captured.lock().unwrap().is_none(),
            "oversized read_image must be blocked before reaching the tool"
        );
        let denied = events.iter().any(|e| {
            matches!(
                e,
                TurnEvent::ToolResult { name, output, .. }
                    if name == "read_image" && output.contains("too large")
            )
        });
        assert!(
            denied,
            "Expected read_image size-denial, got events: {events:?}"
        );
    }

    #[tokio::test]
    async fn test_plan_mode_allows_read_only_bash() {
        let captured = Arc::new(Mutex::new(None));
        let tool = MockTool {
            def: ToolDef {
                name: "bash",
                description: "run a command",
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {"command": {"type": "string"}}
                }),
            },
            captured_args: captured.clone(),
            outcome: ToolOutcome::Success {
                content: "listing".into(),
            },
        };

        let adapter = MockAdapter::new(
            vec![
                StreamEvent::ToolCall(ToolInvocation {
                    id: "call-1".into(),
                    name: "bash".into(),
                    arguments: serde_json::json!({"command": "ls -la"}),
                }),
                StreamEvent::Done {
                    finish_reason: FinishReason::ToolCalls,
                    usage: None,
                },
            ],
            make_info(),
        );

        let (approval_tx, _approval_rx) = mpsc::unbounded_channel();
        let mut exe = make_executor(Box::new(adapter), vec![Arc::new(tool)], make_config(true));
        exe.set_plan_mode(true);

        let events = exe
            .run_turn_collecting("list files", &approval_tx, never_cancelled())
            .await
            .unwrap();

        assert!(
            captured.lock().unwrap().is_some(),
            "read-only bash should run in plan mode"
        );
        let allowed = events.iter().any(|e| {
            matches!(
                e,
                TurnEvent::ToolResult { name, output, .. }
                    if name == "bash" && output == "listing"
            )
        });
        assert!(allowed, "Expected bash result, got events: {events:?}");
    }

    #[tokio::test]
    async fn test_plan_mode_allows_bash_status() {
        let captured = Arc::new(Mutex::new(None));
        let tool = MockTool {
            def: ToolDef {
                name: "bash_status",
                description: "check job status",
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {"id": {"type": "string"}}
                }),
            },
            captured_args: captured.clone(),
            outcome: ToolOutcome::Success {
                content: "running".into(),
            },
        };

        let adapter = MockAdapter::new(
            vec![
                StreamEvent::ToolCall(ToolInvocation {
                    id: "call-1".into(),
                    name: "bash_status".into(),
                    arguments: serde_json::json!({"id": "job-1"}),
                }),
                StreamEvent::Done {
                    finish_reason: FinishReason::ToolCalls,
                    usage: None,
                },
            ],
            make_info(),
        );

        let (approval_tx, _approval_rx) = mpsc::unbounded_channel();
        let mut exe = make_executor(Box::new(adapter), vec![Arc::new(tool)], make_config(true));
        exe.set_plan_mode(true);

        let events = exe
            .run_turn_collecting("check job", &approval_tx, never_cancelled())
            .await
            .unwrap();

        assert!(
            captured.lock().unwrap().is_some(),
            "bash_status should run in plan mode"
        );
        let allowed = events.iter().any(|e| {
            matches!(
                e,
                TurnEvent::ToolResult { name, output, .. }
                    if name == "bash_status" && output == "running"
            )
        });
        assert!(
            allowed,
            "Expected bash_status result, got events: {events:?}"
        );
    }

    #[tokio::test]
    async fn test_plan_mode_allows_bash_cancel_for_read_only_query() {
        // bash_cancel is a read-only status query in plan mode (it only
        // asks to cancel a job; we treat it as allowed because it does not
        // mutate the worktree or read new files).
        let captured = Arc::new(Mutex::new(None));
        let tool = MockTool {
            def: ToolDef {
                name: "bash_cancel",
                description: "cancel a job",
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {"id": {"type": "string"}}
                }),
            },
            captured_args: captured.clone(),
            outcome: ToolOutcome::Success {
                content: "cancelled".into(),
            },
        };

        let adapter = MockAdapter::new(
            vec![
                StreamEvent::ToolCall(ToolInvocation {
                    id: "call-1".into(),
                    name: "bash_cancel".into(),
                    arguments: serde_json::json!({"id": "job-1"}),
                }),
                StreamEvent::Done {
                    finish_reason: FinishReason::ToolCalls,
                    usage: None,
                },
            ],
            make_info(),
        );

        let (approval_tx, _approval_rx) = mpsc::unbounded_channel();
        let mut exe = make_executor(Box::new(adapter), vec![Arc::new(tool)], make_config(true));
        exe.set_plan_mode(true);

        let events = exe
            .run_turn_collecting("cancel job", &approval_tx, never_cancelled())
            .await
            .unwrap();

        assert!(
            captured.lock().unwrap().is_some(),
            "bash_cancel should run in plan mode"
        );
        let allowed = events.iter().any(|e| {
            matches!(
                e,
                TurnEvent::ToolResult { name, output, .. }
                    if name == "bash_cancel" && output == "cancelled"
            )
        });
        assert!(
            allowed,
            "Expected bash_cancel result, got events: {events:?}"
        );
    }

    #[tokio::test]
    async fn test_plan_complete_marker_emits_event() {
        let adapter = MockAdapter::new(
            vec![
                StreamEvent::Text("Here is the plan.".to_string()),
                StreamEvent::Text(format!("\n{PLAN_COMPLETE_MARKER}\n")),
                StreamEvent::Done {
                    finish_reason: FinishReason::Stop,
                    usage: None,
                },
            ],
            make_info(),
        );

        let (approval_tx, _approval_rx) = mpsc::unbounded_channel();
        let mut exe = make_executor(Box::new(adapter), vec![], make_config(false));
        exe.set_plan_mode(true);

        let events = exe
            .run_turn_collecting("plan this", &approval_tx, never_cancelled())
            .await
            .unwrap();

        assert!(
            events.iter().any(|e| matches!(e, TurnEvent::PlanComplete)),
            "Expected PlanComplete event, got events: {events:?}"
        );
    }

    fn temp_hooks_dir() -> (tempfile::TempDir, std::path::PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let hooks_dir = tmp.path().join("hooks");
        std::fs::create_dir_all(&hooks_dir).unwrap();
        (tmp, hooks_dir)
    }

    #[tokio::test]
    async fn test_pre_tool_hook_exit_two_blocks_bash() {
        let captured = Arc::new(Mutex::new(None));
        let tool = MockTool {
            def: ToolDef {
                name: "bash",
                description: "run a command",
                parameters: serde_json::json!(
                    {"type": "object", "properties": {"command": {"type": "string"}}}
                ),
            },
            captured_args: captured.clone(),
            outcome: ToolOutcome::Success {
                content: "ran".into(),
            },
        };

        let adapter = MockAdapter::new(
            vec![
                StreamEvent::ToolCall(ToolInvocation {
                    id: "call-1".into(),
                    name: "bash".into(),
                    arguments: serde_json::json!({"command": "echo hi"}),
                }),
                StreamEvent::Done {
                    finish_reason: FinishReason::ToolCalls,
                    usage: None,
                },
            ],
            make_info(),
        );

        let (_tmp, hooks_dir) = temp_hooks_dir();
        std::fs::write(hooks_dir.join("pre-tool-bash.sh"), "#!/bin/bash\nexit 2").unwrap();

        let mut config = make_config(true);
        config.hooks_dir = Some(hooks_dir);
        let mut exe = make_executor(Box::new(adapter), vec![Arc::new(tool)], config);

        let (approval_tx, _approval_rx) = mpsc::unbounded_channel();
        let events = exe
            .run_turn_collecting("run command", &approval_tx, never_cancelled())
            .await
            .unwrap();

        assert!(
            captured.lock().unwrap().is_none(),
            "pre-tool hook exit 2 must prevent the bash tool from running"
        );
        let denied = events.iter().any(|e| {
            matches!(
                e,
                TurnEvent::ToolResult { name, output, .. }
                    if name == "bash" && output.contains("denied")
            )
        });
        assert!(
            denied,
            "Expected a hook-denial ToolResult, got events: {events:?}"
        );
    }

    #[tokio::test]
    async fn test_pre_tool_hook_exit_one_allows_and_warns() {
        let captured = Arc::new(Mutex::new(None));
        let tool = MockTool {
            def: ToolDef {
                name: "bash",
                description: "run a command",
                parameters: serde_json::json!(
                    {"type": "object", "properties": {"command": {"type": "string"}}}
                ),
            },
            captured_args: captured.clone(),
            outcome: ToolOutcome::Success {
                content: "ran".into(),
            },
        };

        let adapter = MockAdapter::new(
            vec![
                StreamEvent::ToolCall(ToolInvocation {
                    id: "call-1".into(),
                    name: "bash".into(),
                    arguments: serde_json::json!({"command": "echo hi"}),
                }),
                StreamEvent::Done {
                    finish_reason: FinishReason::ToolCalls,
                    usage: None,
                },
            ],
            make_info(),
        );

        let (_tmp, hooks_dir) = temp_hooks_dir();
        std::fs::write(
            hooks_dir.join("pre-tool-bash.sh"),
            "#!/bin/bash\necho warning >&2\nexit 1",
        )
        .unwrap();

        let mut config = make_config(true);
        config.hooks_dir = Some(hooks_dir);
        let mut exe = make_executor(Box::new(adapter), vec![Arc::new(tool)], config);

        let (approval_tx, _approval_rx) = mpsc::unbounded_channel();
        let _events = exe
            .run_turn_collecting("run command", &approval_tx, never_cancelled())
            .await
            .unwrap();

        assert!(
            captured.lock().unwrap().is_some(),
            "pre-tool hook exit 1 must be fail-open and allow the bash tool to run"
        );
    }

    #[tokio::test]
    async fn test_pre_tool_hook_timeout_allows_and_warns() {
        let captured = Arc::new(Mutex::new(None));
        let tool = MockTool {
            def: ToolDef {
                name: "bash",
                description: "run a command",
                parameters: serde_json::json!(
                    {"type": "object", "properties": {"command": {"type": "string"}}}
                ),
            },
            captured_args: captured.clone(),
            outcome: ToolOutcome::Success {
                content: "ran".into(),
            },
        };

        let adapter = MockAdapter::new(
            vec![
                StreamEvent::ToolCall(ToolInvocation {
                    id: "call-1".into(),
                    name: "bash".into(),
                    arguments: serde_json::json!({"command": "echo hi"}),
                }),
                StreamEvent::Done {
                    finish_reason: FinishReason::ToolCalls,
                    usage: None,
                },
            ],
            make_info(),
        );

        let (_tmp, hooks_dir) = temp_hooks_dir();
        std::fs::write(hooks_dir.join("pre-tool-bash.sh"), "#!/bin/bash\nsleep 10").unwrap();

        let mut config = make_config(true);
        config.hooks_dir = Some(hooks_dir);
        let mut exe = make_executor(Box::new(adapter), vec![Arc::new(tool)], config);

        let (approval_tx, _approval_rx) = mpsc::unbounded_channel();
        let _events = exe
            .run_turn_collecting("run command", &approval_tx, never_cancelled())
            .await
            .unwrap();

        assert!(
            captured.lock().unwrap().is_some(),
            "pre-tool hook timeout must be fail-open and allow the bash tool to run"
        );
    }

    #[tokio::test]
    async fn test_compact_hooks_fire_pre_and_post() {
        let (_tmp, hooks_dir) = temp_hooks_dir();
        let pre_marker = hooks_dir.join("pre-compact-marker.txt");
        let post_marker = hooks_dir.join("post-compact-marker.txt");

        std::fs::write(
            hooks_dir.join("pre-compact.sh"),
            format!(
                "#!/bin/bash\necho \"$KF_TOOL_ARGS_JSON\" > {}",
                pre_marker.to_string_lossy()
            ),
        )
        .unwrap();
        std::fs::write(
            hooks_dir.join("post-compact.sh"),
            format!(
                "#!/bin/bash\necho \"$KF_TOOL_ARGS_JSON\" > {}",
                post_marker.to_string_lossy()
            ),
        )
        .unwrap();

        let mut config = make_config(false);
        config.hooks_dir = Some(hooks_dir);
        let exe = make_executor(
            Box::new(MockAdapter::new(vec![], make_info())),
            vec![],
            config,
        );

        exe.run_compact_hook(
            "pre-compact",
            CompactHookStats {
                message_count: 20,
                preserve_recent: 2,
                original_count: 20,
                result_count: 20,
                dropped_tool_results: 0,
                condensed_assistant_turns: 0,
                summarised_messages: 0,
                strategy: "pending",
            },
        );
        exe.run_compact_hook(
            "post-compact",
            CompactHookStats {
                message_count: 20,
                preserve_recent: 2,
                original_count: 20,
                result_count: 8,
                dropped_tool_results: 5,
                condensed_assistant_turns: 3,
                summarised_messages: 0,
                strategy: "naive",
            },
        );

        let mut pre_content = String::new();
        let mut post_content = String::new();
        for _ in 0..40 {
            if let Ok(c) = std::fs::read_to_string(&pre_marker) {
                pre_content = c;
            }
            if let Ok(c) = std::fs::read_to_string(&post_marker) {
                post_content = c;
            }
            if !pre_content.is_empty() && !post_content.is_empty() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }

        assert!(
            !pre_content.is_empty(),
            "pre-compact hook should have written its marker"
        );
        assert!(
            !post_content.is_empty(),
            "post-compact hook should have written its marker"
        );

        let pre_json: serde_json::Value =
            serde_json::from_str(&pre_content).expect("pre-compact hook wrote valid JSON");
        let post_json: serde_json::Value =
            serde_json::from_str(&post_content).expect("post-compact hook wrote valid JSON");

        assert_eq!(pre_json["strategy"], "pending");
        assert_eq!(pre_json["message_count"], 20);

        assert_eq!(post_json["strategy"], "naive");
        assert_eq!(post_json["original_count"], 20);
        assert_eq!(post_json["result_count"], 8);
        assert_eq!(post_json["dropped_tool_results"], 5);
        assert_eq!(post_json["condensed_assistant_turns"], 3);
    }

    #[tokio::test]
    async fn test_find_without_destructive_flags_auto_approved() {
        let captured = Arc::new(Mutex::new(None));
        let tool = MockTool {
            def: ToolDef {
                name: "bash",
                description: "run a command",
                parameters: serde_json::json!({"type": "object", "properties": {"command": {"type": "string"}}}),
            },
            captured_args: captured.clone(),
            outcome: ToolOutcome::Success {
                content: "found!".into(),
            },
        };

        let adapter = MockAdapter::new(
            vec![
                StreamEvent::ToolCall(ToolInvocation {
                    id: "call-1".into(),
                    name: "bash".into(),
                    arguments: serde_json::json!({"command": "find . -name '*.rs' -type f"}),
                }),
                StreamEvent::Done {
                    finish_reason: FinishReason::ToolCalls,
                    usage: None,
                },
            ],
            make_info(),
        );

        let (approval_tx, mut approval_rx) = mpsc::unbounded_channel();
        let approval_handle = tokio::spawn(async move {
            let res =
                tokio::time::timeout(std::time::Duration::from_millis(100), approval_rx.recv())
                    .await;
            assert!(
                res.is_err() || res.unwrap().is_none(),
                "non-destructive find should not ask for approval"
            );
        });

        let mut exe = make_executor(Box::new(adapter), vec![Arc::new(tool)], make_config(false));
        let events = exe
            .run_turn_collecting("search files", &approval_tx, never_cancelled())
            .await
            .unwrap();

        approval_handle.await.unwrap();

        let result = events.iter().find_map(|e| match e {
            TurnEvent::ToolResult { name, output, .. } => Some((name.as_str(), output.as_str())),
            _ => None,
        });
        assert_eq!(result, Some(("bash", "found!")));
    }

    #[tokio::test]
    async fn test_find_delete_requires_approval() {
        let captured = Arc::new(Mutex::new(None));
        let tool = MockTool {
            def: ToolDef {
                name: "bash",
                description: "run a command",
                parameters: serde_json::json!({"type": "object", "properties": {"command": {"type": "string"}}}),
            },
            captured_args: captured.clone(),
            outcome: ToolOutcome::Success {
                content: "deleted!".into(),
            },
        };

        let adapter = MockAdapter::new(
            vec![
                StreamEvent::ToolCall(ToolInvocation {
                    id: "call-1".into(),
                    name: "bash".into(),
                    arguments: serde_json::json!({"command": "find . -name '*.tmp' -delete"}),
                }),
                StreamEvent::Done {
                    finish_reason: FinishReason::ToolCalls,
                    usage: None,
                },
            ],
            make_info(),
        );

        let (approval_tx, mut approval_rx) = mpsc::unbounded_channel();
        let approval_handle = tokio::spawn(async move {
            let req: ApprovalRequest = approval_rx.recv().await.unwrap();
            assert_eq!(req.tool_name, "bash");
            let _ = req.response.send(ApprovalResponse::Approved);
        });

        let mut exe = make_executor(Box::new(adapter), vec![Arc::new(tool)], make_config(false));
        let events = exe
            .run_turn_collecting("delete temp files", &approval_tx, never_cancelled())
            .await
            .unwrap();

        approval_handle.await.unwrap();

        let result = events.iter().find_map(|e| match e {
            TurnEvent::ToolResult { name, output, .. } => Some((name.as_str(), output.as_str())),
            _ => None,
        });
        assert_eq!(result, Some(("bash", "deleted!")));
    }

    #[tokio::test]
    async fn test_glob_base_dir_outside_sandbox_denied() {
        let temp = std::env::temp_dir();
        let sandbox = temp.join(format!("kf-sandbox-{}", std::process::id()));
        std::fs::create_dir_all(&sandbox).unwrap();
        let outside = temp.join(format!("kf-outside-{}", std::process::id()));

        let tool = MockTool {
            def: ToolDef {
                name: "glob",
                description: "list files",
                parameters: serde_json::json!({"type": "object", "properties": {"base_dir": {"type": "string"}, "pattern": {"type": "string"}}}),
            },
            captured_args: Arc::new(Mutex::new(None)),
            outcome: ToolOutcome::Success {
                content: "listed!".into(),
            },
        };

        let adapter = MockAdapter::new(
            vec![
                StreamEvent::ToolCall(ToolInvocation {
                    id: "call-1".into(),
                    name: "glob".into(),
                    arguments: serde_json::json!({"base_dir": outside.to_string_lossy(), "pattern": "*.rs"}),
                }),
                StreamEvent::Done {
                    finish_reason: FinishReason::ToolCalls,
                    usage: None,
                },
            ],
            make_info(),
        );

        let (approval_tx, _approval_rx) = mpsc::unbounded_channel();
        let mut config = make_config(false);
        config.sandbox_dir = Some(sandbox.to_string_lossy().to_string());
        let mut exe = make_executor(Box::new(adapter), vec![Arc::new(tool)], config);
        let events = exe
            .run_turn_collecting("list outside sandbox", &approval_tx, never_cancelled())
            .await
            .unwrap();

        let denied = events.iter().any(|e| matches!(e, TurnEvent::ToolResult { name, output, .. } if name == "glob" && output.contains("Access denied")));
        assert!(denied, "glob outside sandbox should be denied");

        remove_test_dir(&sandbox);
    }

    #[tokio::test]
    async fn test_max_tool_calls_per_turn_respected() {
        let captured = Arc::new(Mutex::new(None));
        let tool = MockTool {
            def: ToolDef {
                name: "echo",
                description: "echo a value",
                parameters: serde_json::json!({"type": "object", "properties": {"val": {"type": "string"}}}),
            },
            captured_args: captured.clone(),
            outcome: ToolOutcome::Success {
                content: "echoed!".into(),
            },
        };

        // The adapter always returns the same tool call, so the executor
        // will loop until it hits the configured cap.
        let tool_call_events = vec![
            StreamEvent::ToolCall(ToolInvocation {
                id: "call-1".into(),
                name: "echo".into(),
                arguments: serde_json::json!({"val": "loop"}),
            }),
            StreamEvent::Done {
                finish_reason: FinishReason::ToolCalls,
                usage: None,
            },
        ];
        let adapter = MockAdapter::new(tool_call_events.clone(), make_info())
            .with_followup_events(tool_call_events);

        let (approval_tx, _approval_rx) = mpsc::unbounded_channel();
        let mut config = make_config(true);
        config.max_tool_calls_per_turn = 3;
        let mut exe = make_executor(Box::new(adapter), vec![Arc::new(tool)], config);
        let events = exe
            .run_turn_collecting("loop", &approval_tx, never_cancelled())
            .await
            .unwrap();

        let tool_results = events
            .iter()
            .filter(|e| matches!(e, TurnEvent::ToolResult { name, .. } if name == "echo"))
            .count();
        assert_eq!(tool_results, 3, "should stop at max_tool_calls_per_turn");

        let hit_limit = events.iter().any(
            |e| matches!(e, TurnEvent::Error(e) if e.contains("Tool call loop limit reached")),
        );
        assert!(
            hit_limit,
            "should emit loop-limit error when cap is reached"
        );
    }

    #[tokio::test]
    async fn test_always_approve_rule_round_trips_to_next_turn() {
        // A rule created by the TUI's `[A]lways` key in one turn should
        // auto-approve the same command in a later turn without prompting.
        let captured = Arc::new(Mutex::new(None));
        let tool = MockTool {
            def: ToolDef {
                name: "bash",
                description: "run a command",
                parameters: serde_json::json!({"type": "object", "properties": {"command": {"type": "string"}}}),
            },
            captured_args: captured.clone(),
            outcome: ToolOutcome::Success {
                content: "ran!".into(),
            },
        };

        let command = "cargo test --release";
        let first_events = vec![
            StreamEvent::ToolCall(ToolInvocation {
                id: "call-1".into(),
                name: "bash".into(),
                arguments: serde_json::json!({"command": command}),
            }),
            StreamEvent::Done {
                finish_reason: FinishReason::ToolCalls,
                usage: None,
            },
        ];
        let followup_events = vec![
            StreamEvent::ToolCall(ToolInvocation {
                id: "call-2".into(),
                name: "bash".into(),
                arguments: serde_json::json!({"command": command}),
            }),
            StreamEvent::Done {
                finish_reason: FinishReason::ToolCalls,
                usage: None,
            },
        ];
        let adapter =
            MockAdapter::new(first_events, make_info()).with_followup_events(followup_events);

        let (approval_tx, mut approval_rx) = mpsc::unbounded_channel();
        let approval_handle = tokio::spawn(async move {
            let req: ApprovalRequest = approval_rx.recv().await.unwrap();
            assert_eq!(req.tool_name, "bash");
            assert_eq!(
                req.args.get("command").and_then(|v| v.as_str()),
                Some(command)
            );
            let _ = req.response.send(ApprovalResponse::AlwaysApprove);
        });

        let mut exe = make_executor(Box::new(adapter), vec![Arc::new(tool)], make_config(false));
        let _events = exe
            .run_turn_collecting("run tests", &approval_tx, never_cancelled())
            .await
            .unwrap();
        approval_handle.await.unwrap();

        {
            let cfg = cfg(&exe);
            assert_eq!(cfg.permission_rules.len(), 1);
            assert_eq!(cfg.permission_rules[0].action, PermissionAction::Allow);
        }

        // Second turn: same command should now match the rule and run
        // without sending an approval request.
        let (approval_tx2, mut approval_rx2) = mpsc::unbounded_channel();
        let requested = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let requested_flag = requested.clone();
        let no_approval_handle = tokio::spawn(async move {
            if approval_rx2.recv().await.is_some() {
                requested_flag.store(true, std::sync::atomic::Ordering::SeqCst);
            }
        });

        // Second turn: same command should now match the rule and run
        // without sending an approval request. The timeout is generous
        // because this test suite is heavily parallel and a 300 ms wall
        // clock would flake under load; the goal is only to detect an
        // infinite hang caused by a misplaced approval prompt.
        let second_events = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            exe.run_turn_collecting("run tests again", &approval_tx2, never_cancelled()),
        )
        .await
        .expect("second turn should complete without approval prompt");

        no_approval_handle.abort();
        assert!(
            !requested.load(std::sync::atomic::Ordering::SeqCst),
            "rule should prevent second approval request"
        );

        let second_events = second_events.unwrap();
        let has_result = second_events
            .iter()
            .any(|e| matches!(e, TurnEvent::ToolResult { name, output, .. } if name == "bash" && output == "ran!"));
        assert!(has_result, "second turn should execute the allowed command");
    }
}
