//! Session executor — runs model turns, dispatches tools, handles approvals.

use crate::adapters::ModelAdapter;
use crate::session::access::{
    access_from_config, warn_if_unsandboxed, DenyList, PathGuard, ReadGate,
};
use crate::session::adapter_swap::AdapterSwap;
use crate::session::carryover::CarryoverProfile;
use crate::session::config::config_diff_summary;
use crate::session::conversation::ConversationLog;
use crate::session::event_bus::BusEvent;
use crate::session::event_bus::EventBus;
use crate::session::hooks::HookRunner;
use crate::session::prompt::PromptBuilder;
use crate::session::verifier::{CorrectionLoop, CorrectionResult, VerifierHandler, VerifierSlots};
use crate::shared::audit::AuditLog;
use crate::shared::{read_shared_config, Config, Message, Role, SharedConfig, ToolInvocation};
use crate::tools::{ToolContext, UndoStackRef};
use std::sync::Arc;

use helpers::tool_cancel_token;

pub(crate) mod approval;
pub(crate) mod dispatch;
pub(crate) mod helpers;
pub(crate) mod loop_;
#[cfg(test)]
pub(crate) mod tests;
pub(crate) mod turn;
pub(crate) mod types;

pub use approval::{ApprovalRequest, ApprovalResponder, ApprovalResponse};
pub use types::{CompactHookStats, TurnEvent};

pub struct Executor {
    adapter: Box<dyn ModelAdapter>,
    adapter_swap: AdapterSwap,
    hook_runner: HookRunner,
    conversation: ConversationLog,
    prompt_builder: PromptBuilder,
    tools: crate::session::toolset::CompositeToolset,
    config: SharedConfig,
    cost_tracking: crate::shared::CostTracking,
    model_name: String,
    deny_list: DenyList,
    path_guard: PathGuard,
    read_gate: ReadGate,
    event_bus: EventBus,
    audit_log: Arc<AuditLog>,
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

    /// Unique identifier for this session, forwarded to lifecycle hooks as
    /// `KF_SESSION_ID`. Populated by the caller after construction.
    session_id: String,
    /// Optional spawner for the `task` tool. Built lazily from executor
    /// state so subagents inherit the same model, config, and sandboxing.
    task_spawner: Option<Arc<dyn crate::tools::task::TaskSpawner>>,
}

impl Executor {
    pub fn with_log(
        adapter: Box<dyn ModelAdapter>,
        tools: crate::session::toolset::CompositeToolset,
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
        tools: crate::session::toolset::CompositeToolset,
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
        tools: crate::session::toolset::CompositeToolset,
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

        let audit_log_path = cfg
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
        adapter.set_json_mode(cfg.json_mode);

        // Push the deterministic-mode seed down to the active adapter.
        adapter.set_seed(cfg.seed);

        let adapter_swap = AdapterSwap::new(
            model_name.clone(),
            cfg.ollama_host.clone(),
            None, // model_type_override not available here; set via CLI
            cfg.request_timeout_secs,
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
            tools,
            config,
            cost_tracking: crate::shared::CostTracking::default(),
            model_name,
            deny_list,
            path_guard,
            read_gate,
            event_bus,
            audit_log,
            correction_loop: None,
            carryover,
            carryover_enabled,
            carryover_target,
            undo_stack,
            plan_mode: false,
            recovered_messages: None,
            session_id: String::new(),
            task_spawner: None,
        };
        this.init_default_verifiers(plugin_registry);
        this.build_task_spawner();
        this
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
            task_spawner: self.task_spawner.clone(),
        }
    }

    /// Whether deterministic mode is active. When true, the parallel
    /// tool batch runs sequentially (no `tokio::spawn`) to eliminate
    /// nondeterminism from task scheduling.
    fn is_deterministic(&self) -> bool {
        read_shared_config(&self.config).seed.is_some()
    }

    /// Attach a repo-graph context index to the prompt builder.
    /// Called once at session start after the index is built.
    pub fn set_context_index(&mut self, idx: kirkforge_context_index::ContextIndex) {
        let mut pb = crate::session::prompt::PromptBuilder::new();
        pb = pb.with_context_index(idx);
        self.prompt_builder = pb;
    }

    /// Construct the in-process task spawner from the executor's model,
    /// config, and sandboxing state. Called once at construction.
    fn build_task_spawner(&mut self) {
        let cfg = read_shared_config(&self.config).clone();
        let model_name = self.model_name.clone();
        let ollama_host = cfg.ollama_host.clone();
        let undo_stack = self.undo_stack.clone();
        let supports_images = self.adapter.model_info().supports_images;
        self.task_spawner = Some(Arc::new(crate::tools::task::InProcessTaskSpawner::new(
            cfg,
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

    /// Re-register plugin verifiers from a fresh registry while keeping the
    /// built-in verifier slots intact.
    ///
    /// Returns the number of plugin verifiers now registered.
    fn rebuild_plugin_verifiers(
        &mut self,
        registry: &kirkforge_plugin_host::PluginRegistry,
    ) -> usize {
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
    pub fn reload_plugins(&mut self, registry: &kirkforge_plugin_host::PluginRegistry) -> String {
        let cfg = read_shared_config(&self.config).clone();

        // 1. Replace the plugin toolset.
        let plugin_tools =
            crate::session::plugin_tools::all_plugin_tools(registry, self.config.clone());
        let plugin_tool_count = plugin_tools.len();
        let plugin_set = Box::new(crate::session::toolset::VecToolset::new(
            "plugin",
            plugin_tools,
        ));
        self.tools.replace("plugin", plugin_set);

        // 2. Rebuild hooks so built-in and plugin hooks are merged fresh.
        let mut hook_runner = match &cfg.hooks_dir {
            Some(dir) => HookRunner::new(dir.clone()),
            None => HookRunner::default(),
        };
        hook_runner.load_plugin_hooks(registry);
        self.hook_runner = hook_runner;

        // 3. Rebuild plugin verifiers while keeping built-in verifiers.
        let plugin_verifier_count = self.rebuild_plugin_verifiers(registry);

        format!(
            "Reloaded plugins: {} active plugin(s), {} plugin tool(s), {} plugin verifier(s)",
            registry.active_count(),
            plugin_tool_count,
            plugin_verifier_count
        )
    }

    #[allow(clippy::too_many_arguments)]
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
}
