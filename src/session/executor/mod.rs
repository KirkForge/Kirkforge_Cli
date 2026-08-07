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
pub(crate) mod sandbox;
pub(crate) mod scout;
#[cfg(test)]
pub(crate) mod tests;
pub(crate) mod turn;
pub(crate) mod types;

pub use approval::{ApprovalRequest, ApprovalResponder, ApprovalResponse};
pub use loop_::{DoomHit, DoomLoopTracker};
pub use scout::{ScoutSubagent, SCOUT_TOOLS};
pub use types::{CompactHookStats, TurnEvent};

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
    task_spawner: Option<Arc<dyn crate::tools::task::TaskSpawner>>,

    /// Optional turn-trace recorder. When present, each completed turn
    /// is serialized as a `TurnRecord` and appended to the trace file.
    trace: Option<std::sync::Mutex<crate::session::replay::TraceRecorder>>,
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
            refuse_if_production_unsandboxed(&path_guard)?;
        } else {
            warn_if_unsandboxed(&path_guard);
        }
        let sandbox = sandbox::PathGuardTower {
            path_guard,
            deny_list,
            read_gate,
        };

        let audit_log_path = cfg
            .security
            .audit_log_path
            .clone()
            .filter(|p| !p.as_os_str().is_empty())
            .or_else(|| {
                crate::session::data_dir()
                    .ok()
                    .map(|d| d.join("audit.ndjson"))
            });
        let audit_log = Arc::new(AuditLog::new(audit_log_path));

        // Push the session-level JSON-mode flag down to the active
        // adapter. The trait method has a default no-op for adapters
        // that don't support it, so unknown models (and the test
        // mocks) silently ignore the flag.
        adapter.set_json_mode(cfg.model.json_mode);

        // Push the deterministic-mode seed down to the active adapter.
        adapter.set_seed(cfg.model.seed);

        // Push max_tokens + extended-thinking config down to adapters.
        adapter.set_max_tokens(cfg.model.max_tokens);
        adapter.set_extended_thinking(cfg.model.extended_thinking);
        adapter.set_budget_tokens(cfg.model.budget_tokens);

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
                hook_runner.add_in_process_hook(Box::new(
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
        #[cfg(all(feature = "budget", feature = "stratum"))]
        {
            // WO 8.6: register Stratum's default compression listener on
            // the budget's slice path so a slice triggers compression
            // and the post-tool hook records the post-compression size.
            // Runtime-gated on stratum being enabled (WO 15.7 5.1): the
            // listener is only useful when stratum hooks are live.
            if cfg.tools.enabled_plugins.iter().any(|n| n == "stratum")
                && !cfg.tools.disabled_plugins.contains("stratum")
            {
                crate::session::stratum::register_default_budget_listener();
                tracing::info!("stratum->budget slice listener registered");
            }
        }
        #[cfg(feature = "budget")]
        {
            // Runtime `enabled_plugins` gate (WO 15.7 5.1): the config key
            // for the folded budget plugin is `"kf-budget"`. Skip
            // hooks when disabled at runtime.
            if cfg.tools.enabled_plugins.iter().any(|n| n == "kf-budget")
                && !cfg.tools.disabled_plugins.contains("kf-budget")
            {
                crate::session::budget::init_from_config(&cfg);
                for hook in crate::session::budget::all_budget_hooks() {
                    hook_runner.add_in_process_hook(hook);
                }
                tracing::info!("budget session-start, post-tool-bash, post-tool-write_file, pre-compact hooks registered");
            } else {
                tracing::info!(
                    "budget hooks skipped (disabled via enabled_plugins or disabled_plugins)"
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
            verifier_bus: None,
            undo_stack,
            plan_mode: false,
            recovered_messages: None,
            session_id: String::new(),
            task_spawner: None,
            trace: None,
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

    /// Set the session identifier forwarded to lifecycle hooks as
    /// `KF_SESSION_ID`.
    pub fn set_session_id(&mut self, id: String) {
        self.session_id = id;
    }

    /// Set the turn-trace recorder for this session. Each completed turn
    /// will be serialized and appended to the trace file.
    pub fn set_trace(&mut self, recorder: crate::session::replay::TraceRecorder) {
        self.trace = Some(std::sync::Mutex::new(recorder));
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
        let secs = cfg.tools.tool_timeout_secs.unwrap_or(30).clamp(1, 3600);
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
        let ollama_host = shared_cfg.read().unwrap().model.ollama_host.clone();
        let undo_stack = self.undo_stack.clone();
        let supports_images = self.adapter.model_info().supports_images;
        self.task_spawner = Some(Arc::new(crate::tools::task::InProcessTaskSpawner::new(
            shared_cfg,
            model_name,
            ollama_host,
            undo_stack,
            supports_images,
        )));
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
                crate::session::verifier::lint::verify_lint(event).await
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
                crate::session::verifier::build::verify_build(event).await
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
                crate::session::verifier::test::verify_test(event).await
            }
        }
        {
            let mut s = slots.write().unwrap_or_else(|e| e.into_inner());
            if s.register(Arc::new(TestV)).is_ok() {
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
        const BUILTIN_VERIFIERS: &[&str] = &["security", "lint", "build", "git", "rustfmt", "test"];

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
                hook_runner.add_in_process_hook(Box::new(
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
                crate::session::stratum::register_default_budget_listener();
            }
        }
        #[cfg(feature = "budget")]
        {
            // Runtime `enabled_plugins` gate (WO 15.7 5.1): config key is
            // `"kf-budget"` for the folded budget plugin.
            if cfg.tools.enabled_plugins.iter().any(|n| n == "kf-budget")
                && !cfg.tools.disabled_plugins.contains("kf-budget")
            {
                crate::session::budget::init_from_config(&cfg);
                for hook in crate::session::budget::all_budget_hooks() {
                    hook_runner.add_in_process_hook(hook);
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
    /// log. Returns `Some(hint)` to inject into the conversation.
    pub fn observe_tool_outcome(
        &mut self,
        tool: &str,
        outcome: &crate::shared::ToolOutcome,
        event_tx: &mpsc::Sender<TurnEvent>,
    ) -> Option<String> {
        self.cost.observe_tool_outcome(tool, outcome, event_tx)
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
    pub fn set_plan_mode(&mut self, enabled: bool) {
        self.plan_mode = enabled;
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
