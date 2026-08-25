//! Session executor — runs model turns, dispatches tools, handles approvals.

use crate::adapters::ModelAdapter;
use crate::session::access::{
    access_from_config, refuse_if_production_unsandboxed, refuse_if_unsandboxed,
    warn_if_unsandboxed,
};
use crate::session::adapter_swap::AdapterSwap;
use crate::session::carryover::CarryoverProfile;
use crate::session::config::config_diff_summary;
use crate::session::conversation::ConversationLog;
use crate::session::hooks::HookRunner;
use crate::session::prompt::PromptBuilder;
use crate::session::verifier::types::BusEvent;
use crate::session::verifier::{
    CorrectionLoop, CorrectionResult, VerifierBus, VerifierHandler, VerifierSlots,
};
use crate::shared::audit::AuditLog;
use crate::shared::{read_shared_config, Config, Message, Role, SharedConfig, ToolInvocation};
use crate::tools::UndoStackRef;
use std::sync::Arc;
use tokio::sync::mpsc;

pub(crate) mod approval;
pub(crate) mod cost_tracking;
pub(crate) mod dispatch;
pub(crate) mod helpers;
pub(crate) mod loop_;
pub(crate) mod pre_run;
pub(crate) mod sandbox;
pub(crate) mod scout;
pub(crate) mod stream;
#[cfg(test)]
pub(crate) mod tests;
pub(crate) mod turn;
pub(crate) mod types;

pub use approval::{ApprovalRequest, ApprovalResponder, ApprovalResponse};
pub use cost_tracking::{DoomLoopAction, DoomLoopOutcome};
pub use loop_::{DoomHit, DoomLoopTracker};
pub use scout::{ScoutSubagent, SCOUT_TOOLS};
pub use types::{CompactHookStats, TurnEvent, VerificationOutcome};

pub struct Executor {
    adapter: Box<dyn ModelAdapter>,
    adapter_swap: AdapterSwap,
    hook_runner: HookRunner,
    conversation: ConversationLog,
    prompt_builder: PromptBuilder,
    tools: crate::session::toolset::CompositeToolset,
    config: SharedConfig,
    cost: cost_tracking::CostTracker,
    model_name: String,
    sandbox: sandbox::PathGuardTower,
    audit_log: Arc<AuditLog>,
    correction_loop: Option<CorrectionLoop>,

    /// Per-session budget for slicing tool results (WO 22.6-R2).
    #[cfg(feature = "budget")]
    budget: Option<crate::session::budget::SharedBudget>,

    /// Per-session budget offload store with LRU cap (WO 22.6-R2).
    #[cfg(feature = "budget")]
    budget_store: Option<std::sync::Arc<dyn kf_budget_core::OffloadStore>>,

    /// Per-session Stratum offload store with LRU cap (WO 22.6-R2).
    #[cfg(feature = "stratum")]
    stratum_store: Option<std::sync::Arc<kf_compress_core::store::InMemoryOffloadStore>>,

    /// Unified verifier bus — collects structured VerdictEntrys from all
    /// registered BusVerifiers after each file-modifying tool call.
    verifier_bus: Option<std::sync::Mutex<VerifierBus>>,

    /// Optional per-session undo stack. Held here so `/undo` can pop
    /// via a control channel without touching the tools directly.
    undo_stack: Option<UndoStackRef>,

    /// When true, the executor only permits read-only discovery tools
    /// (read_file, read_image, grep, glob, and read-only bash). All
    /// mutating tools are denied at the dispatch layer so the model
    /// cannot implement while it is still "thinking". Entered via
    /// `/plan` and exited via `/implement` or user approval.
    plan_mode: bool,

    /// True when the session runs unattended (`--non-interactive`). Plan
    /// mode is an interactive aid (exit via `/implement`); in a scripted
    /// run there is no one to type it, so enforcing it would brick the
    /// agent. When set, the doom-loop breaker downgrades `AutoPlan` to
    /// `WarnOnly` and `pre_run` skips the plan-mode block entirely. (WO 30.9)
    non_interactive: bool,

    /// If the conversation log was restored from a checkpoint on open,
    /// this holds the number of recovered messages. It is emitted once
    /// as a `TurnEvent::Recovered` at the start of the first turn so
    /// the TUI/line-mode output can show a status line.
    recovered_messages: Option<usize>,

    /// Unique identifier for this session, forwarded to lifecycle hooks as
    /// `KF_SESSION_ID`. Populated by the caller after construction.
    session_id: String,
    /// Optional spawner for the `task` tool. Built lazily from executor
    /// state so subagents inherit the same model, config, and sandboxing.
    /// Held concretely (not as `dyn TaskSpawner`) so the executor can set
    /// the per-session parent-approval forwarder on it (WO 30.6). The ctx
    /// the task tool receives still carries it as `Arc<dyn TaskSpawner>`.
    task_spawner: Option<Arc<crate::session::task_spawner::InProcessTaskSpawner>>,

    /// Optional turn-trace recorder. When present, each completed turn
    /// is serialized as a `TurnRecord` and appended to the trace file.
    trace: Option<std::sync::Mutex<crate::session::replay::TraceRecorder>>,

    /// Memory store for auto-populated facts from post-turn extraction.
    memory_store: Option<crate::session::memory::MemoryStore>,

    /// Turn counter for rate-limited memory extraction.
    turn_count: u64,

    /// Root cooperative-cancel token (WO 35.3). When attached (subagent
    /// executors), per-tool-call cancel tokens are live children of it, so
    /// an external cancel (`TaskManager::cancel`) kills in-flight tool
    /// work — bash process groups — instead of waiting out
    /// `tool_timeout_secs`. `None` (parent sessions) keeps the
    /// snapshot-at-dispatch semantics (WO 15.7).
    cancel_token: Option<tokio_util::sync::CancellationToken>,
    /// Owning subagent task id (WO 36.2). Set by the task spawner so
    /// background bash jobs spawned by this executor are attributed to
    /// the task — `TaskManager::cancel` then kills exactly those jobs.
    /// `None` (parent sessions) leaves jobs unowned (main session).
    task_owner: Option<String>,
}

impl Executor {
    pub fn with_log(
        adapter: Box<dyn ModelAdapter>,
        tools: crate::session::toolset::CompositeToolset,
        config: Config,
        conversation: ConversationLog,
        carryover_target: Option<std::sync::Arc<std::sync::Mutex<CarryoverProfile>>>,
    ) -> anyhow::Result<Self> {
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
        tools: crate::session::toolset::CompositeToolset,
        config: SharedConfig,
        conversation: ConversationLog,
        carryover_target: Option<std::sync::Arc<std::sync::Mutex<CarryoverProfile>>>,
        undo_stack: Option<UndoStackRef>,
    ) -> anyhow::Result<Self> {
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
        tools: crate::session::toolset::CompositeToolset,
        config: SharedConfig,
        conversation: ConversationLog,
        carryover_target: Option<std::sync::Arc<std::sync::Mutex<CarryoverProfile>>>,
        undo_stack: Option<UndoStackRef>,
        plugin_registry: Option<&kf_plugin_host::PluginRegistry>,
    ) -> anyhow::Result<Self> {
        let model_name = adapter.model_info().name.clone();
        let config_for_startup = config.clone();
        let cfg = read_shared_config(&config_for_startup);
        let (deny_list, path_guard, read_gate) = access_from_config(&cfg);
        if cfg.security.sandbox.harden {
            refuse_if_unsandboxed(&path_guard)?;
        } else if cfg!(not(debug_assertions)) && !cfg.security.sandbox.accept_unsandboxed {
            // WO 27.1: --i-accept-unsandboxed is the release escape hatch.
            // When set, fall through to the warning instead of refusing, so
            // an operator on a kernel where landlock restrict_self errors
            // (or who intentionally runs without a write-scope sandbox) can
            // still start the binary. PathGuard write-containment is off in
            // that case; landlock fail-closed is independently escaped via
            // the same flag in setup_rlimits.
            refuse_if_production_unsandboxed(&path_guard)?;
        } else {
            warn_if_unsandboxed(&path_guard);
        }
        let sandbox = sandbox::PathGuardTower {
            path_guard,
            deny_list,
            read_gate,
        };

        // WO 45.63: surface the unmapped-model pricing warning EAGERLY at
        // session startup, not after the first turn's cost calc. The
        // shared predicate dedups with `warn_unmapped_model`, so this
        // logs once per model per process and the lazy turn-time warn
        // becomes a no-op for the startup model. Operators on a model
        // with no pricing row learn immediately that cost tracking will
        // report $0.
        if !crate::shared::model_has_pricing_row(
            &model_name,
            if cfg.model.price_overrides.is_empty() {
                None
            } else {
                Some(&cfg.model.price_overrides)
            },
        ) {
            crate::shared::warn_unmapped_model(&model_name);
        }

        let audit_log_path = cfg
            .security
            .audit_log_path
            .clone()
            .filter(|p| !p.as_os_str().is_empty())
            .or_else(|| match crate::session::data_dir() {
                Ok(d) => Some(d.join("audit.ndjson")),
                Err(e) => {
                    tracing::warn!(error = %e, "audit log disabled: data_dir unavailable");
                    None
                }
            });
        let audit_log = Arc::new(AuditLog::new(audit_log_path));

        // Push the session-level JSON-mode flag down to the active
        // adapter. The trait method has a default no-op for adapters
        // that don't support it, so unknown models (and the test
        // mocks) silently ignore the flag.
        adapter.set_json_mode(cfg.model.json_mode);
        adapter.set_response_format(cfg.model.effective_response_format());

        // Push the deterministic-mode seed down to the active adapter.
        adapter.set_seed(cfg.model.seed);

        // Push max_tokens + extended-thinking config down to adapters.
        adapter.set_max_tokens(cfg.model.max_tokens);
        adapter.set_extended_thinking(cfg.model.extended_thinking);
        adapter.set_budget_tokens(cfg.model.budget_tokens);
        adapter.set_streaming_timeout(cfg.model.streaming_timeout_secs);

        // Activate the Anthropic hosted computer_use beta when configured.
        // `hosted` is inert without the `computer_use` Cargo feature (the
        // adapter's `set_computer_use_dims` is a no-op for non-Anthropic
        // adapters and the wire rewrite compiles out). See WO 32.17.
        if cfg.security.computer_use.hosted && cfg.security.computer_use.enabled {
            #[cfg(feature = "computer_use")]
            adapter.set_computer_use_dims(Some((
                cfg.security.computer_use.width.max(1),
                cfg.security.computer_use.height.max(1),
            )));
        }

        let adapter_swap = AdapterSwap::new(
            model_name.clone(),
            cfg.model.ollama_host.clone(),
            None, // model_type_override not available here; set via CLI
            cfg.model.request_timeout_secs,
        );

        let mut hook_runner = match &cfg.tools.hooks_dir {
            Some(dir) => HookRunner::new(dir.clone()),
            None => HookRunner::default(),
        };
        hook_runner.set_audit_log(Arc::clone(&audit_log));
        if let Some(registry) = plugin_registry {
            hook_runner.load_plugin_hooks(registry, &cfg.tools.disabled_plugins);
        }
        #[cfg(feature = "stratum")]
        {
            // Runtime `enabled_plugins` gate (WO 15.7 5.1): skip hooks when
            // the plugin is disabled at runtime, even if the compile-time
            // feature is on. Matches the tool-registration gate in main.rs.
            if cfg.tools.enabled_plugins.iter().any(|n| n == "stratum")
                && !cfg.tools.disabled_plugins.contains("stratum")
            {
                hook_runner.add_post_hook(Box::new(
                    crate::session::stratum::StratumSessionStartHook {
                        config: config.clone(),
                    },
                ));
                hook_runner
                    .add_in_process_hook(Box::new(crate::session::stratum::StratumPreToolBashHook));
                tracing::info!("stratum session-start and pre-tool-bash hooks registered");
            } else {
                tracing::info!(
                    "stratum hooks skipped (disabled via enabled_plugins or disabled_plugins)"
                );
            }
        }

        let carryover_enabled = cfg.session.carryover_enabled;
        let mut cost = cost_tracking::CostTracker::new(carryover_enabled);
        cost.carryover_target = carryover_target;

        let mut this = Self {
            adapter,
            adapter_swap,
            hook_runner,
            conversation,
            prompt_builder: PromptBuilder::new(),
            tools,
            config,
            cost,
            model_name,
            sandbox,
            audit_log,
            correction_loop: None,
            #[cfg(feature = "budget")]
            budget: None,
            #[cfg(feature = "budget")]
            budget_store: None,
            #[cfg(feature = "stratum")]
            stratum_store: None,
            verifier_bus: None,
            undo_stack,
            plan_mode: false,
            non_interactive: false,
            recovered_messages: None,
            session_id: String::new(),
            task_spawner: None,
            trace: None,
            memory_store: crate::session::memory::MemoryStore::default_store().ok(),
            turn_count: 0,
            cancel_token: None,
            task_owner: None,
        };

        this.init_default_verifiers(plugin_registry);
        this.build_task_spawner();
        Ok(this)
    }

    /// Record that the conversation log was restored from a checkpoint.
    /// The count is emitted once as `TurnEvent::Recovered` on the first
    /// turn. Call immediately after constructing the executor if the log
    /// opener reported `OpenOutcome::Restored`.
    pub fn set_recovered_messages(&mut self, count: usize) {
        self.recovered_messages = Some(count);
    }

    /// Set the per-session budget and offload store (WO 22.6-R2).
    #[cfg(feature = "budget")]
    pub fn set_budget_stores(
        &mut self,
        budget: crate::session::budget::SharedBudget,
        store: std::sync::Arc<dyn kf_budget_core::OffloadStore>,
    ) {
        self.budget = Some(budget);
        self.budget_store = Some(store);
    }

    /// Set the per-session Stratum offload store (WO 22.6-R2).
    #[cfg(feature = "stratum")]
    pub fn set_stratum_store(
        &mut self,
        store: std::sync::Arc<kf_compress_core::store::InMemoryOffloadStore>,
    ) {
        self.stratum_store = Some(store);
    }

    /// Attach per-session budget + stratum stores and wire the budget
    /// guard into production (WO 38.8). Runs `init_from_config`, registers
    /// the budget hooks (session-start, post-tool-bash, post-tool-write_file,
    /// pre-compact), and registers the Stratum slice-compression listener
    /// keyed by this executor's `session_id`. Must be called AFTER
    /// `set_session_id`. Idempotent: clears the session's stale listeners
    /// first so repeated attaches (e.g. on `/plugins` reload) don't
    /// accumulate dead entries.
    #[cfg(all(feature = "budget", feature = "stratum"))]
    pub fn attach_session_stores(&mut self, stores: crate::session::SessionStores) {
        let cfg = read_shared_config(&self.config);
        self.budget = Some(stores.budget);
        self.budget_store = Some(stores.budget_store);
        self.stratum_store = Some(stores.stratum_store);

        let sid = self.session_id.clone();
        crate::session::budget::clear_session_sliced_listeners(&sid);

        // Stratum slice listener (compression hook).
        if cfg.tools.enabled_plugins.iter().any(|n| n == "stratum")
            && !cfg.tools.disabled_plugins.contains("stratum")
        {
            // WO 43.16 no-throw: `stratum_store` is set at :369 above, so
            // this is a local invariant. Previously an `expect` panic; now
            // a guarded branch that logs + skips listener registration so
            // a dispatch bug becomes a missing slice hook, not an unwind.
            match self.stratum_store.clone() {
                Some(store) => {
                    crate::session::stratum::register_default_budget_listener(&sid, store);
                    tracing::info!("stratum->budget slice listener registered for session {sid}");
                }
                None => {
                    tracing::error!(
                        "stratum_store missing in attach_session_stores; \
                         skipping slice listener registration for session {sid}"
                    );
                }
            }
        }

        // Budget hooks.
        if cfg.tools.enabled_plugins.iter().any(|n| n == "kf-budget")
            && !cfg.tools.disabled_plugins.contains("kf-budget")
        {
            if let Some(ref budget) = self.budget {
                crate::session::budget::init_from_config(budget, &cfg);
                for hook in crate::session::budget::budget_hooks(budget) {
                    self.hook_runner.add_post_hook(hook);
                }
                tracing::info!(
                    "budget hooks registered for session {sid} (session-start, post-tool-bash, post-tool-write_file, pre-compact)"
                );
            }
        } else {
            tracing::info!(
                "budget hooks skipped for session {sid} (disabled via enabled_plugins or disabled_plugins)"
            );
        }
    }

    /// Attach per-session budget stores (budget-only, no stratum).
    /// See [`attach_session_stores`] for the full variant. WO 38.8.
    #[cfg(all(feature = "budget", not(feature = "stratum")))]
    pub fn attach_session_stores(&mut self, stores: crate::session::SessionStores) {
        let cfg = read_shared_config(&self.config);
        self.budget = Some(stores.budget);
        self.budget_store = Some(stores.budget_store);

        let sid = self.session_id.clone();
        crate::session::budget::clear_session_sliced_listeners(&sid);

        if cfg.tools.enabled_plugins.iter().any(|n| n == "kf-budget")
            && !cfg.tools.disabled_plugins.contains("kf-budget")
        {
            if let Some(ref budget) = self.budget {
                crate::session::budget::init_from_config(budget, &cfg);
                for hook in crate::session::budget::budget_hooks(budget) {
                    self.hook_runner.add_post_hook(hook);
                }
                tracing::info!("budget hooks registered for session {sid}");
            }
        }
    }

    /// Attach per-session stratum store (stratum-only, no budget). WO 38.8.
    #[cfg(all(not(feature = "budget"), feature = "stratum"))]
    pub fn attach_session_stores(&mut self, stores: crate::session::SessionStores) {
        self.stratum_store = Some(stores.stratum_store);
    }

    /// No-op attach when neither budget nor stratum is enabled. WO 38.8.
    #[cfg(all(not(feature = "budget"), not(feature = "stratum")))]
    pub fn attach_session_stores(&mut self, _stores: crate::session::SessionStores) {}

    /// Clear this session's sliced listeners. Call on executor teardown
    /// so a dropped session doesn't leak listeners into the process-global
    /// registry. WO 38.8.
    #[cfg(feature = "budget")]
    pub fn clear_session_listeners(&self) {
        crate::session::budget::clear_session_sliced_listeners(&self.session_id);
    }

    /// Set the session identifier forwarded to lifecycle hooks as
    /// `KF_SESSION_ID`.
    pub fn set_session_id(&mut self, id: String) {
        self.session_id = id;
    }

    /// The canonical run id for this executor (WO 45.1). The session id
    /// IS the root `RunId`; child tasks / bash jobs / scheduled jobs /
    /// workflow steps derive their `parent_run_id` from it. Returns
    /// `None` when `set_session_id` was never called (tests, bench).
    pub fn run_id(&self) -> Option<crate::shared::RunId> {
        if self.session_id.is_empty() {
            None
        } else {
            Some(crate::shared::RunId::new(self.session_id.clone()))
        }
    }

    /// Set the turn-trace recorder for this session. Each completed turn
    /// will be serialized and appended to the trace file.
    pub fn set_trace(&mut self, recorder: crate::session::replay::TraceRecorder) {
        self.trace = Some(std::sync::Mutex::new(recorder));
    }

    /// Build a per-tool-call context linked to the turn's cancellation
    /// state. The deadline is derived from the config's per-tool timeout
    /// (default 120 s) unless the tool itself specifies a longer timeout
    /// (e.g. bash) — the executor layer caps the outer wait, and the tool
    /// is responsible for honouring its own internal deadline.
    /// Per-tool-call hard timeout from the shared config. Clamped to
    /// [1, 3600] seconds.
    fn tool_call_timeout(&self) -> std::time::Duration {
        let cfg = read_shared_config(&self.config);
        let secs = cfg.tools.tool_timeout_secs.unwrap_or(120).clamp(1, 3600);
        std::time::Duration::from_secs(secs)
    }

    /// Whether deterministic mode is active. When true, the parallel
    /// tool batch runs sequentially (no `tokio::spawn`) to eliminate
    /// nondeterminism from task scheduling.
    fn is_deterministic(&self) -> bool {
        read_shared_config(&self.config).model.seed.is_some()
    }

    /// Attach a repo-graph context index to the prompt builder.
    /// Called once at session start after the index is built.
    pub fn set_context_index(&mut self, idx: kf_context_index::ContextIndex) {
        let mut pb = crate::session::prompt::PromptBuilder::new();
        pb = pb.with_context_index(idx);
        self.prompt_builder = pb;
    }

    /// Construct the in-process task spawner from the executor's model,
    /// config, and sandboxing state. Called once at construction.
    fn build_task_spawner(&mut self) {
        let cfg = read_shared_config(&self.config).clone();
        let shared_cfg: crate::shared::SharedConfig =
            std::sync::Arc::new(std::sync::RwLock::new(cfg));
        let model_name = self.model_name.clone();
        // WO 43.16 no-throw: the RwLock was just constructed two lines
        // above, so the only failure mode is poison (unreachable here;
        // no write happens between construction and this read). Use the
        // repo's established poison-recovery pattern (budget.rs:131) so
        // a future refactor that moves a write earlier can't panic.
        let ollama_host = shared_cfg
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .model
            .ollama_host
            .clone();
        let undo_stack = self.undo_stack.clone();
        let supports_images = self.adapter.model_info().supports_images;
        self.task_spawner = Some(Arc::new(
            crate::session::task_spawner::InProcessTaskSpawner::new(
                shared_cfg,
                model_name,
                ollama_host,
                undo_stack,
                supports_images,
            ),
        ));
    }

    /// Set the parent-approval forwarder on this executor's task spawner.
    /// Subagent destructive-tool approval requests are forwarded to `tx`
    /// so the user sees them in the TUI / line-mode and can decide
    /// interactively (WO 30.6). Called from `run_turn` with the current
    /// turn's approval channel, which is session-stable.
    pub fn set_spawner_parent_approval(&self, tx: mpsc::UnboundedSender<ApprovalRequest>) {
        if let Some(spawner) = &self.task_spawner {
            spawner.set_parent_approval(tx);
        }
    }

    /// Clear the task spawner's parent-approval forwarder. Drops the
    /// cloned sender so the approval channel closes when the caller
    /// releases its own sender. Called at turn end.
    pub fn clear_spawner_parent_approval(&self) {
        if let Some(spawner) = &self.task_spawner {
            spawner.clear_parent_approval();
        }
    }

    /// Replace the shared config with `new` and rebuild access-control
    /// structures from it. Returns a human-readable diff summary.
    fn reload_config(&mut self, new: Config) -> String {
        let old = read_shared_config(&self.config).clone();
        // Update the shared lock. If it is poisoned we still apply the
        // new config locally so this executor keeps running with the
        // fresh rules.
        let mut cfg = crate::shared::write_shared_config(&self.config);
        *cfg = new.clone();
        let fresh = new;
        let (deny_list, path_guard, read_gate) = access_from_config(&fresh);
        self.sandbox = sandbox::PathGuardTower {
            path_guard,
            deny_list,
            read_gate,
        };
        // JSON-mode changes are applied to the running adapter too.
        self.adapter.set_json_mode(fresh.model.json_mode);
        self.adapter
            .set_extended_thinking(fresh.model.extended_thinking);
        self.adapter.set_budget_tokens(fresh.model.budget_tokens);
        self.adapter
            .set_streaming_timeout(fresh.model.streaming_timeout_secs);
        if fresh.security.computer_use.hosted && fresh.security.computer_use.enabled {
            #[cfg(feature = "computer_use")]
            self.adapter.set_computer_use_dims(Some((
                fresh.security.computer_use.width.max(1),
                fresh.security.computer_use.height.max(1),
            )));
        } else {
            #[cfg(feature = "computer_use")]
            self.adapter.set_computer_use_dims(None);
        }
        config_diff_summary(&old, &fresh)
    }

    pub fn init_default_verifiers(
        &mut self,
        plugin_registry: Option<&kf_plugin_host::PluginRegistry>,
    ) -> usize {
        use crate::session::verifier::{Verdict, Verifier};

        // Default slots need room for security, lint, build, git, rustfmt,
        // test, plus any plugin verifiers registered below. Use a generous cap
        // so live plugin reload can add many plugin verifiers without running out.
        let slots = Arc::new(std::sync::RwLock::new(VerifierSlots::with_max_slots(64)));
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
                crate::session::verifier::lint::verify_lint(
                    event,
                    &crate::session::verifier::SystemCommandRunner,
                )
                .await
            }
        }
        {
            let mut s = slots.write().unwrap_or_else(|e| e.into_inner());
            if s.register(Arc::new(LintV)).is_ok() {
                count += 1;
            }
        }

        struct BuildV;
        #[async_trait::async_trait]
        impl Verifier for BuildV {
            fn name(&self) -> &str {
                "build"
            }
            fn priority(&self) -> u8 {
                3
            }
            async fn verify(&self, event: &BusEvent) -> Verdict {
                crate::session::verifier::build::verify_build(
                    event,
                    &crate::session::verifier::SystemCommandRunner,
                )
                .await
            }
        }
        {
            let mut s = slots.write().unwrap_or_else(|e| e.into_inner());
            if s.register(Arc::new(BuildV)).is_ok() {
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

        struct RustfmtV;
        #[async_trait::async_trait]
        impl Verifier for RustfmtV {
            fn name(&self) -> &str {
                "rustfmt"
            }
            fn priority(&self) -> u8 {
                4
            }
            async fn verify(&self, event: &BusEvent) -> Verdict {
                crate::session::verifier::rustfmt::verify_rustfmt(event).await
            }
        }
        {
            let mut s = slots.write().unwrap_or_else(|e| e.into_inner());
            if s.register(Arc::new(RustfmtV)).is_ok() {
                count += 1;
            }
        }

        struct TestV;
        #[async_trait::async_trait]
        impl Verifier for TestV {
            fn name(&self) -> &str {
                "test"
            }
            fn priority(&self) -> u8 {
                5
            }
            async fn verify(&self, event: &BusEvent) -> Verdict {
                crate::session::verifier::test::verify_test(
                    event,
                    &crate::session::verifier::SystemCommandRunner,
                )
                .await
            }
        }
        {
            let mut s = slots.write().unwrap_or_else(|e| e.into_inner());
            if s.register(Arc::new(TestV)).is_ok() {
                count += 1;
            }
        }

        // Python verifiers (WO 31.1). Each self-gates on language detection
        // inside its verify fn — they return Skipped unless a Python marker is
        // found at the edited file's project root, so registering them
        // alongside the Rust verifiers is safe for pure-Rust workspaces.
        struct PyTestV;
        #[async_trait::async_trait]
        impl Verifier for PyTestV {
            fn name(&self) -> &str {
                "python_test"
            }
            fn priority(&self) -> u8 {
                6
            }
            async fn verify(&self, event: &BusEvent) -> Verdict {
                crate::session::verifier::python_test::verify_python_test(event).await
            }
        }
        {
            let mut s = slots.write().unwrap_or_else(|e| e.into_inner());
            if s.register(Arc::new(PyTestV)).is_ok() {
                count += 1;
            }
        }

        struct PyLintV;
        #[async_trait::async_trait]
        impl Verifier for PyLintV {
            fn name(&self) -> &str {
                "python_lint"
            }
            fn priority(&self) -> u8 {
                7
            }
            async fn verify(&self, event: &BusEvent) -> Verdict {
                crate::session::verifier::python_lint::verify_python_lint(event).await
            }
        }
        {
            let mut s = slots.write().unwrap_or_else(|e| e.into_inner());
            if s.register(Arc::new(PyLintV)).is_ok() {
                count += 1;
            }
        }

        struct PyTypeV;
        #[async_trait::async_trait]
        impl Verifier for PyTypeV {
            fn name(&self) -> &str {
                "python_typecheck"
            }
            fn priority(&self) -> u8 {
                8
            }
            async fn verify(&self, event: &BusEvent) -> Verdict {
                crate::session::verifier::python_typecheck::verify_python_typecheck(event).await
            }
        }
        {
            let mut s = slots.write().unwrap_or_else(|e| e.into_inner());
            if s.register(Arc::new(PyTypeV)).is_ok() {
                count += 1;
            }
        }

        // Node/Go/generic verifiers (WO 32.20). Each self-gates on language
        // detection + toolchain presence inside its verify fn — they return
        // Skipped unless the relevant marker is found at the edited file's
        // project root, so registering them alongside the Rust/Python
        // verifiers is safe for pure-Rust workspaces.
        struct NodeTestV;
        #[async_trait::async_trait]
        impl Verifier for NodeTestV {
            fn name(&self) -> &str {
                "node_test"
            }
            fn priority(&self) -> u8 {
                9
            }
            async fn verify(&self, event: &BusEvent) -> Verdict {
                crate::session::verifier::node_test::verify_node_test(event).await
            }
        }
        {
            let mut s = slots.write().unwrap_or_else(|e| e.into_inner());
            if s.register(Arc::new(NodeTestV)).is_ok() {
                count += 1;
            }
        }

        struct NodeLintV;
        #[async_trait::async_trait]
        impl Verifier for NodeLintV {
            fn name(&self) -> &str {
                "node_lint"
            }
            fn priority(&self) -> u8 {
                10
            }
            async fn verify(&self, event: &BusEvent) -> Verdict {
                crate::session::verifier::node_lint::verify_node_lint(event).await
            }
        }
        {
            let mut s = slots.write().unwrap_or_else(|e| e.into_inner());
            if s.register(Arc::new(NodeLintV)).is_ok() {
                count += 1;
            }
        }

        struct GoTestV;
        #[async_trait::async_trait]
        impl Verifier for GoTestV {
            fn name(&self) -> &str {
                "go_test"
            }
            fn priority(&self) -> u8 {
                11
            }
            async fn verify(&self, event: &BusEvent) -> Verdict {
                crate::session::verifier::go_test::verify_go_test(event).await
            }
        }
        {
            let mut s = slots.write().unwrap_or_else(|e| e.into_inner());
            if s.register(Arc::new(GoTestV)).is_ok() {
                count += 1;
            }
        }

        struct GoVetV;
        #[async_trait::async_trait]
        impl Verifier for GoVetV {
            fn name(&self) -> &str {
                "go_vet"
            }
            fn priority(&self) -> u8 {
                12
            }
            async fn verify(&self, event: &BusEvent) -> Verdict {
                crate::session::verifier::go_vet::verify_go_vet(event).await
            }
        }
        {
            let mut s = slots.write().unwrap_or_else(|e| e.into_inner());
            if s.register(Arc::new(GoVetV)).is_ok() {
                count += 1;
            }
        }

        struct GenericTestV;
        #[async_trait::async_trait]
        impl Verifier for GenericTestV {
            fn name(&self) -> &str {
                "generic_test"
            }
            fn priority(&self) -> u8 {
                13
            }
            async fn verify(&self, event: &BusEvent) -> Verdict {
                crate::session::verifier::generic_test::verify_generic_test(event).await
            }
        }
        {
            let mut s = slots.write().unwrap_or_else(|e| e.into_inner());
            if s.register(Arc::new(GenericTestV)).is_ok() {
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

        let handler = Arc::new(VerifierHandler::new(slots, self.sandbox.path_guard.clone()));
        self.correction_loop = Some(CorrectionLoop::new(handler));
        match tokio::runtime::Handle::try_current() {
            Ok(_) => {
                let mut vbus = super::verifier::VerifierBus::new();
                if let Some(registry) = plugin_registry {
                    let n = crate::session::verifier::plugin::register_plugin_verifiers_into_bus(
                        registry, &mut vbus,
                    );
                    if n > 0 {
                        tracing::info!(
                            plugin_verifiers = n,
                            "registered plugin verifiers into verifier bus"
                        );
                    }
                }
                self.verifier_bus = Some(std::sync::Mutex::new(vbus));
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

    /// Re-register plugin verifiers from a fresh registry while keeping the
    /// built-in verifier slots intact.
    ///
    /// Returns the number of plugin verifiers now registered.
    fn rebuild_plugin_verifiers(&mut self, registry: &kf_plugin_host::PluginRegistry) -> usize {
        const BUILTIN_VERIFIERS: &[&str] = &[
            "security",
            "lint",
            "build",
            "git",
            "rustfmt",
            "test",
            "python_test",
            "python_lint",
            "python_typecheck",
            // WO 32.20 Node/Go/generic built-ins. Must stay in sync with
            // init_default_verifiers or reload drops them silently (WO 44.29).
            "node_test",
            "node_lint",
            "go_test",
            "go_vet",
            "generic_test",
        ];

        let Some(ref correction_loop) = self.correction_loop else {
            return 0;
        };
        let handler = correction_loop.verifier_handler();
        let slots = handler.slots();
        let plugin_verifiers = crate::session::verifier::plugin::verifiers_from_registry(registry);

        let mut new_count = 0;
        {
            let mut s = slots.write().unwrap_or_else(|e| e.into_inner());
            s.retain(|v| BUILTIN_VERIFIERS.contains(&v.name()));
            for v in plugin_verifiers {
                if s.register(v).is_ok() {
                    new_count += 1;
                }
            }
        }
        new_count
    }

    /// Reload the plugin layer: tools, hooks, and verifiers.
    ///
    /// Built-in and MCP toolsets are preserved; only the plugin source is
    /// replaced. Returns a short human-readable summary.
    pub fn reload_plugins(&mut self, registry: &kf_plugin_host::PluginRegistry) -> String {
        let cfg = read_shared_config(&self.config).clone();

        // 1. Replace the plugin toolset.
        let plugin_tools = crate::session::plugin_tools::all_plugin_tools(
            registry,
            self.config.clone(),
            Some(std::sync::Arc::clone(&self.audit_log)),
        );
        let plugin_tool_count = plugin_tools.len();
        let plugin_set = Box::new(crate::session::toolset::VecToolset::new(
            "plugin",
            plugin_tools,
        ));
        self.tools.replace("plugin", plugin_set);

        // 2. Rebuild hooks so built-in and plugin hooks are merged fresh.
        let mut hook_runner = match &cfg.tools.hooks_dir {
            Some(dir) => HookRunner::new(dir.clone()),
            None => HookRunner::default(),
        };
        hook_runner.load_plugin_hooks(registry, &cfg.tools.disabled_plugins);
        #[cfg(feature = "stratum")]
        {
            // Runtime `enabled_plugins` gate (WO 15.7 5.1).
            if cfg.tools.enabled_plugins.iter().any(|n| n == "stratum")
                && !cfg.tools.disabled_plugins.contains("stratum")
            {
                hook_runner.add_post_hook(Box::new(
                    crate::session::stratum::StratumSessionStartHook {
                        config: self.config.clone(),
                    },
                ));
                hook_runner
                    .add_in_process_hook(Box::new(crate::session::stratum::StratumPreToolBashHook));
            }
        }
        #[cfg(all(feature = "budget", feature = "stratum"))]
        {
            if cfg.tools.enabled_plugins.iter().any(|n| n == "stratum")
                && !cfg.tools.disabled_plugins.contains("stratum")
            {
                if let Some(ref stratum_store) = self.stratum_store {
                    // WO 38.8: clear stale listeners for this session before
                    // re-registering, so reloads don't accumulate dead entries.
                    crate::session::budget::clear_session_sliced_listeners(&self.session_id);
                    crate::session::stratum::register_default_budget_listener(
                        &self.session_id,
                        stratum_store.clone(),
                    );
                }
            }
        }
        #[cfg(feature = "budget")]
        {
            // Runtime `enabled_plugins` gate (WO 15.7 5.1): config key is
            // `"kf-budget"` for the folded budget plugin.
            if cfg.tools.enabled_plugins.iter().any(|n| n == "kf-budget")
                && !cfg.tools.disabled_plugins.contains("kf-budget")
            {
                if let (Some(ref budget), Some(ref _store)) = (&self.budget, &self.budget_store) {
                    crate::session::budget::init_from_config(budget, &cfg);
                    for hook in crate::session::budget::budget_hooks(budget) {
                        hook_runner.add_post_hook(hook);
                    }
                }
            }
        }
        self.hook_runner = hook_runner;

        // 3. Rebuild plugin verifiers while keeping built-in verifiers.
        let plugin_verifier_count = self.rebuild_plugin_verifiers(registry);

        // 4. Rebuild plugin verifiers on the unified bus (ADR-028): drop
        // old plugin verifiers, keep built-in stub verifiers, re-add from
        // the fresh registry.
        self.rebuild_bus_plugin_verifiers(registry);

        format!(
            "Reloaded plugins: {} active plugin(s), {} plugin tool(s), {} plugin verifier(s)",
            registry.active_count(),
            plugin_tool_count,
            plugin_verifier_count
        )
    }

    /// Re-register plugin verifiers on the `VerifierBus` while keeping the
    /// built-in bus verifiers (`security`, `git`) intact. Mirrors
    /// `rebuild_plugin_verifiers` for the event-driven path. ADR-028.
    fn rebuild_bus_plugin_verifiers(&mut self, registry: &kf_plugin_host::PluginRegistry) -> usize {
        const BUILTIN_BUS_VERIFIERS: &[&str] = &["security", "git"];
        let Some(ref bus_lock) = self.verifier_bus else {
            return 0;
        };
        let mut bus = bus_lock.lock().unwrap_or_else(|e| e.into_inner());
        bus.retain_verifiers(|v| BUILTIN_BUS_VERIFIERS.contains(&v));
        crate::session::verifier::plugin::register_plugin_verifiers_into_bus(registry, &mut bus)
    }

    pub fn conversation_log(&self) -> &ConversationLog {
        &self.conversation
    }

    pub fn replace_conversation(&mut self, new_log: ConversationLog) {
        self.conversation = new_log;
    }

    /// Feed a tool outcome to the doom-loop detector. If the
    /// threshold is crossed, also emit a `TurnEvent::DoomLoopDetected`
    /// on `event_tx` and a `MetricEvent::DoomLoop` to the metrics
    /// log. Returns `Some(DoomLoopOutcome)` with the hint and
    /// the configured remediation action when the circuit breaker fires.
    pub fn observe_tool_outcome(
        &mut self,
        tool: &str,
        outcome: &crate::shared::ToolOutcome,
        event_tx: &mpsc::Sender<TurnEvent>,
    ) -> Option<DoomLoopOutcome> {
        let cfg = crate::shared::read_shared_config(&self.config);
        let action = cfg
            .tools
            .doom_loop_action
            .parse::<DoomLoopAction>()
            .unwrap_or(DoomLoopAction::AutoPlan);
        // Non-interactive runs have no user to switch to plan mode for help,
        // and `/implement` (the only exit) is interactive-only — AutoPlan
        // would brick the run. Downgrade to WarnOnly so the doom-loop warning
        // still logs without trapping the agent. (WO 30.9)
        let action = if self.non_interactive && action == DoomLoopAction::AutoPlan {
            DoomLoopAction::WarnOnly
        } else {
            action
        };
        let max_hits = cfg.tools.doom_loop_max_hits;
        self.cost
            .observe_tool_outcome(tool, outcome, event_tx, max_hits, action)
    }

    /// Install a full system-prompt override (e.g. from `--system`).
    /// Pass `None` to revert to the base template. See
    /// `PromptBuilder::set_system_override` for the trade-off (full
    /// override, not append).
    pub fn set_system_override(&mut self, override_prompt: Option<String>) {
        self.prompt_builder.set_system_override(override_prompt);
    }

    /// Returns the current system override prompt, if any.
    pub fn system_override(&self) -> Option<&str> {
        self.prompt_builder.system_override()
    }

    /// Enable or disable plan mode. When enabled, only read-only
    /// discovery tools are allowed to execute; mutating tools are
    /// denied at the dispatch layer.
    /// Attach a root cooperative-cancel token (WO 35.3). Subagent
    /// executors get the token from their `TaskRequest` so external cancel
    /// reaches in-flight tool calls; parent sessions leave it unset.
    pub fn set_cancel_token(&mut self, token: Option<tokio_util::sync::CancellationToken>) {
        self.cancel_token = token;
    }

    /// Attach the owning subagent task id (WO 36.2). Threaded into every
    /// tool call's `ToolContext` so background bash jobs the subagent
    /// spawns are tagged and cancellable by owner; parent sessions leave
    /// it unset.
    pub fn set_task_owner(&mut self, owner: Option<String>) {
        self.task_owner = owner;
    }

    pub fn set_plan_mode(&mut self, enabled: bool) {
        self.plan_mode = enabled;
    }

    /// Mark this session as unattended (`--non-interactive`). When set,
    /// plan mode is never enforced (writes are never blocked) and the
    /// doom-loop circuit breaker downgrades `AutoPlan` to `WarnOnly`.
    /// (WO 30.9)
    pub fn set_non_interactive(&mut self, enabled: bool) {
        self.non_interactive = enabled;
    }

    /// Exit plan mode and inject a system message telling the model it
    /// may now implement the plan. Returns the message content so the
    /// caller can echo it to the user if desired.
    pub async fn exit_plan_mode(&mut self) -> anyhow::Result<String> {
        self.plan_mode = false;
        let msg = "Plan mode exited — you may now implement the plan.".to_string();
        self.conversation
            .append_async(Message {
                role: Role::System,
                content: msg.clone(),
                content_parts: None,
                thinking: None,
                tool_calls: None,
                tool_call_id: None,
                tool_name: None,
                token_count: None,
            })
            .await?;
        Ok(msg)
    }

    /// Run a lifecycle hook (fire-and-forget). Wraps HookRunner::run with
    /// common env vars derived from current session state.
    fn run_hook(&self, event: &str, tool_name: Option<&str>, args_json: Option<&str>) {
        let mut env_vars: Vec<(&str, &str)> = Vec::new();
        env_vars.push(("KF_EVENT", event));
        if !self.session_id.is_empty() {
            env_vars.push(("KF_SESSION_ID", &self.session_id));
        }
        if let Some(name) = tool_name {
            env_vars.push(("KF_TOOL_NAME", name));
        }
        if let Some(json) = args_json {
            env_vars.push(("KF_TOOL_ARGS_JSON", json));
        }
        let cfg = crate::shared::read_shared_config(&self.config);
        self.hook_runner.run(event, &env_vars, &cfg);
    }

    /// Run a lifecycle hook with tool result content (fire-and-forget).
    ///
    /// This is the in-process variant: folded-plugin hooks receive the tool's
    /// output via `HookContext.tool_result`, enabling the budget guard
    /// to inspect bash/write_file results and decide whether to slice/compact.
    fn run_hook_with_result(
        &self,
        event: &str,
        tool_name: Option<&str>,
        args_json: Option<&str>,
        tool_result: Option<&str>,
    ) {
        let ctx = crate::session::hooks::HookContext {
            event: event.to_string(),
            session_id: self.session_id.clone(),
            tool_name: tool_name.map(|s| s.to_string()),
            tool_args_json: args_json.map(|s| s.to_string()),
            tool_result: tool_result.map(|s| s.to_string()),
            compact_stats: None,
        };
        let cfg = crate::shared::read_shared_config(&self.config);
        self.hook_runner.run_with_context(event, &ctx, &cfg);
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

        let ctx = crate::session::hooks::HookContext {
            event: event.to_string(),
            session_id: self.session_id.clone(),
            tool_name: None,
            tool_args_json: Some(args_json.clone()),
            tool_result: None,
            compact_stats: Some(crate::session::hooks::CompactHookStatsData {
                message_count: stats.message_count,
                preserve_recent: stats.preserve_recent,
                original_count: stats.original_count,
                result_count: stats.result_count,
                dropped_tool_results: stats.dropped_tool_results,
                condensed_assistant_turns: stats.condensed_assistant_turns,
                summarised_messages: stats.summarised_messages,
                strategy: stats.strategy.to_string(),
            }),
        };
        let cfg = crate::shared::read_shared_config(&self.config);
        self.hook_runner.run_with_context(event, &ctx, &cfg);
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
        env_vars.push(("KF_EVENT", event));
        if !self.session_id.is_empty() {
            env_vars.push(("KF_SESSION_ID", &self.session_id));
        }
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
        self.cost.flush_carryover();
    }

    fn collect_carryover(&mut self, tc: &ToolInvocation, crs: &[CorrectionResult]) {
        self.cost.collect_carryover(tc, crs);
    }
}

// WO 38.8: clear this session's sliced listeners on teardown so a
// dropped session doesn't leak listeners into the process-global
// registry. No-op when the budget feature is off or the session never
// registered any.
#[cfg(feature = "budget")]
impl Drop for Executor {
    fn drop(&mut self) {
        crate::session::budget::clear_session_sliced_listeners(&self.session_id);
    }
}
