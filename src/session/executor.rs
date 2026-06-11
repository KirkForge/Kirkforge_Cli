use crate::adapters::ModelAdapter;
use crate::session::adapter_swap::AdapterSwap;
use crate::session::hooks::HookRunner;
use crate::session::access::{
    access_from_config, warn_if_unsandboxed, DenyList, GuardVerdict, PathGuard, ReadGate,
};
use crate::session::carryover::CarryoverProfile;
use crate::session::conversation::ConversationLog;
use crate::session::event_bus::{BusEvent, EventBus};
use crate::session::prompt::PromptBuilder;
use crate::session::verifier::CorrectionResult;
use crate::session::verifier::{CorrectionLoop, VerifierHandler, VerifierSlots};
use crate::shared::permission::{evaluate, PermissionAction};
use crate::shared::{Config, Message, Role, StreamEvent, ToolDef, ToolInvocation, ToolOutcome};
use crate::tools::Tool;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::mpsc;

fn push_rule_unique(
    rules: &mut Vec<crate::shared::permission::PermissionRule>,
    new_rule: crate::shared::permission::PermissionRule,
) {
    let duplicate = rules
        .iter()
        .any(|r| r.tool == new_rule.tool && r.key == new_rule.key && r.pattern == new_rule.pattern);
    if !duplicate {
        rules.push(new_rule);
    }
}

enum IterationOutcome {

    ToolCalls(Vec<ToolInvocation>),

    Finished,

    ParseError,
}

enum ApprovalDecision {
    Approved,
    Denied,
    AlwaysApproved,
}

pub struct Executor {
    adapter: Box<dyn ModelAdapter>,
    adapter_swap: AdapterSwap,
    hook_runner: HookRunner,
    conversation: ConversationLog,
    prompt_builder: PromptBuilder,
    tools: Vec<Arc<dyn Tool>>,
    config: Config,
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
/// just a `PathBuf` + `HashSet<String>`), so it can outlive the
/// `&mut self` borrows inside `run_turn_inner` and fire on Drop
/// without aliasing.
///
/// Fire-and-forget: `HookRunner::run` wraps `tokio::spawn` internally
/// with a 5s timeout, so Drop never blocks on the hook script.
pub struct PostTurnHookGuard {
    runner: HookRunner,
}

impl PostTurnHookGuard {
    pub fn new(runner: HookRunner) -> Self {
        Self { runner }
    }
}

impl Drop for PostTurnHookGuard {
    fn drop(&mut self) {
        // No-op if the hook script doesn't exist; otherwise spawns
        // a tokio task that runs `bash <hooks_dir>/post-turn.sh`
        // with a 5s timeout. Drop completes in microseconds.
        self.runner.run("post-turn", &[]);
    }
}

impl Executor {
    pub fn new(adapter: Box<dyn ModelAdapter>, tools: Vec<Arc<dyn Tool>>, config: Config) -> Self {

        let temp_dir = std::env::temp_dir().join("kirkforge-session");
        let log_path = temp_dir.join(format!(
            "session-{}.ndjson",
            chrono::Local::now().format("%Y%m%d-%H%M%S")
        ));
        let conversation = ConversationLog::open(log_path).unwrap();
        Self::with_log(adapter, tools, config, conversation, None)
    }

    pub fn with_log(
        mut adapter: Box<dyn ModelAdapter>,
        tools: Vec<Arc<dyn Tool>>,
        config: Config,
        conversation: ConversationLog,
        carryover_target: Option<std::sync::Arc<std::sync::Mutex<CarryoverProfile>>>,
    ) -> Self {
        let model_name = adapter.model_info().name.clone();
        let (deny_list, path_guard, read_gate) = access_from_config(&config);
        warn_if_unsandboxed(&path_guard);

        // Push the session-level JSON-mode flag down to the active
        // adapter. The trait method has a default no-op for adapters
        // that don't support it, so unknown models (and the test
        // mocks) silently ignore the flag.
        adapter.set_json_mode(config.json_mode);

        let adapter_swap = AdapterSwap::new(
            model_name.clone(),
            config.ollama_host.clone(),
            None, // model_type_override not available here; set via CLI
        );

        let hook_runner = HookRunner::default();

        let event_bus = EventBus::new();

        let carryover_enabled = config.carryover_enabled;
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
            correction_loop: None,
            carryover,
            carryover_enabled,
            carryover_target,
        };
        this.init_default_verifiers();
        this
    }

    pub fn init_default_verifiers(&mut self) -> usize {
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
            let mut s = slots.write().unwrap();
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
            let mut s = slots.write().unwrap();
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
            let mut s = slots.write().unwrap();
            if s.register(Arc::new(GitV)).is_ok() {
                count += 1;
            }
        }

        let handler = Arc::new(VerifierHandler::new(slots));
        let bus = self.event_bus.clone();
        let h = handler.clone();
        tokio::spawn(async move {
            let _ = bus.register(h).await;
        });

        self.correction_loop = Some(CorrectionLoop::new(handler));
        count
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
    ) -> anyhow::Result<()> {
        let cancelled = Arc::new(AtomicBool::new(false));

        // Fire session-start hook (fire-and-forget, best-effort)
        self.run_hook("session-start", None, None);

        loop {
            tokio::select! {
                biased; // check cancel first, then input

                Some(()) = cancel_rx.recv() => {
                    cancelled.store(true, Ordering::SeqCst);
                    if event_tx.send(TurnEvent::Token("\n⚠️ Generation cancelled\n".into())).is_err() {

                        tracing::warn!("TUI event receiver dropped; executor driver exiting");
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
                    let new_name = self
                        .adapter_swap
                        .force_swap(&model_name, &mut self.adapter);
                    self.model_name = new_name.clone();
                    if event_tx
                        .send(TurnEvent::Token(format!(
                            "🔀 Switched to {}\n",
                            new_name
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
                    let mut did_summarize = false;

                    // Try LLM-based summarization if enabled
                    if self.config.summarize_enabled && history.len() > 2 {
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
                                    model: self.config.summarize_model.clone(),
                                    max_summary_tokens: 500,
                                    min_turns_for_summary: 4,
                                    min_compression_ratio: 0.4,
                                };

                                let result = crate::session::prompt::summarizer::summarize_conversation(
                                    &summarizer_config,
                                    &to_summarize,
                                    &self.config.ollama_host,
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
                                                "Summarization failed: {}",
                                                e
                                            )))
                                            .is_err()
                                        {
                                            self.flush_carryover();
                                            return Ok(());
                                        }
                                    } else {
                                        did_summarize = true;
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
                        let result = crate::session::prompt::compact(history);
                        let report = if let Err(e) = self.conversation.replace_all(result.new_messages.clone()) {
                            TurnEvent::Error(format!("Compaction failed: {}", e))
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
                }
                Some(input) = input_rx.recv() => {
                    cancelled.store(false, Ordering::SeqCst);
                    let events = self.run_turn(&input, &approval_tx, &cancelled).await;
                    match events {
                        Ok(evs) => {
                            for ev in evs {
                                if event_tx.send(ev).is_err() {

                                    tracing::warn!("TUI event receiver dropped mid-turn; executor driver exiting");
                                    self.flush_carryover();
                                    return Ok(());
                                }
                            }
                        }
                        Err(e) => {

                            if event_tx.send(TurnEvent::Error(format!("Turn failed: {}", e))).is_err() {
                                tracing::warn!(
                                    error = %e,
                                    "TUI event receiver dropped while reporting turn-failure event"
                                );
                                self.flush_carryover();
                                return Ok(());
                            }
                        }
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

    /// Run a lifecycle hook (fire-and-forget). Wraps HookRunner::run with
    /// common env vars derived from current session state.
    fn run_hook(&self, event: &str, tool_name: Option<&str>, args_json: Option<&str>) {
        let mut env_vars: Vec<(&str, &str)> = Vec::new();
        if let Some(name) = tool_name {
            env_vars.push(("KF_TOOL_NAME", name));
        }
        if let Some(json) = args_json {
            env_vars.push(("KF_TOOL_ARGS_JSON", json));
        }
        self.hook_runner.run(event, &env_vars);
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

    pub async fn run_turn(
        &mut self,
        user_input: &str,
        approval_sender: &mpsc::UnboundedSender<ApprovalRequest>,
        cancelled: &AtomicBool,
    ) -> anyhow::Result<Vec<TurnEvent>> {
        // Post-turn hook: fires on every exit path (Ok / Err / panic /
        // cancel / max-iterations / parse-error second retry) via the
        // `PostTurnHookGuard` constructed on the stack below. The guard
        // owns a cloned `HookRunner`, so it can outlive the `&mut self`
        // borrows inside `run_turn_inner` and fire on Drop without
        // aliasing.
        //
        // Supersedes the earlier inner/outer split where the hook was
        // fired manually after `run_turn_inner` returned. That worked
        // but was fragile (any new early-return in the inner had to
        // flow back to the wrapper). Review.md arch-concern #4
        // specifically called this out — "a Drop guard holding an &Fn
        // closure would be more robust." We keep the inner/outer split
        // (still useful for tests that want to call the inner directly)
        // but stop firing the hook manually.
        let _hook_guard = PostTurnHookGuard::new(self.hook_runner.clone());
        self.run_turn_inner(user_input, approval_sender, cancelled).await
    }

    async fn run_turn_inner(
        &mut self,
        user_input: &str,
        approval_sender: &mpsc::UnboundedSender<ApprovalRequest>,
        cancelled: &AtomicBool,
    ) -> anyhow::Result<Vec<TurnEvent>> {
        let mut events = Vec::new();

        // --- adapter hot-swap via smart routing ---
        if self.config.routing_enabled {
            let swapped = self
                .adapter_swap
                .maybe_swap(&self.config, &mut self.adapter, user_input);
            if let Some(new_model) = swapped {
                self.model_name = new_model.clone();
                events.push(TurnEvent::Token(format!(
                    "🔀 Switched to {}\n",
                    new_model
                )));
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

        let mut tool_calls: Vec<ToolInvocation> = Vec::new();
        let mut already_retried_parse = false;

        const MAX_ITERATIONS: usize = 10;

        for iteration in 0..MAX_ITERATIONS {
            if cancelled.load(Ordering::SeqCst) {
                events.push(TurnEvent::Token("\n⚠️ Cancelled\n".into()));
                return Ok(events);
            }

            let outcome = self
                .stream_iteration(approval_sender, cancelled, &mut events, &mut tool_calls)
                .await?;

            match outcome {
                IterationOutcome::Finished => return Ok(events),
                IterationOutcome::ToolCalls(mut tcs) => {
                    for tc in &mut tcs {
                        self.dispatch_tool_call(tc, approval_sender, &mut events)
                            .await?;
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
                        events.push(TurnEvent::Token("(JSON parse error, retrying…)\n".into()));
                    } else {

                        return Ok(events);
                    }
                }
            }

            if iteration + 1 >= MAX_ITERATIONS {
                events.push(TurnEvent::Error("Tool call loop limit reached".into()));
                return Ok(events);
            }
        }

        // Post-turn hook fires from the public `run_turn` wrapper
        // after this inner function returns. Do NOT add an explicit
        // `self.run_hook("post-turn", ...)` here — that double-fires
        // the hook on the natural completion path.
        Ok(events)
    }

    #[allow(unused_variables)]
    async fn stream_iteration(
        &mut self,
        approval_sender: &mpsc::UnboundedSender<ApprovalRequest>,
        cancelled: &AtomicBool,
        events: &mut Vec<TurnEvent>,
        tool_calls_out: &mut Vec<ToolInvocation>,
    ) -> anyhow::Result<IterationOutcome> {
        let model_info = self.adapter.model_info();
        let tool_defs: Vec<ToolDef> = self.tools.iter().map(|t| t.def()).collect();
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
                events.push(TurnEvent::Token("\n⚠️ Cancelled\n".into()));

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
                    events.push(TurnEvent::Token(t));
                }
                StreamEvent::Thinking(t) => {
                    assistant_thinking.push_str(&t);
                    events.push(TurnEvent::Thinking(t));
                }
                StreamEvent::ToolCall(tc) => {
                    tool_calls_out.push(tc);
                }
                StreamEvent::Error(e) => {

                    if e.contains("parse") || e.contains("parseable") {
                        had_parse_error = true;
                    }
                    events.push(TurnEvent::Error(e));
                }
                StreamEvent::Done {
                    finish_reason: _,
                    usage,
                } => {

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

                    if let Some(ref u) = usage {
                        let prompt = u.prompt_tokens.unwrap_or(0);
                        let completion = u.completion_tokens.unwrap_or(0);
                        let cost = crate::shared::calculate_cost(&self.model_name, u);
                        self.cost_tracking.record_turn(prompt, completion, cost);
                        events.push(TurnEvent::CostStats {
                            prompt_tokens: prompt,
                            completion_tokens: completion,
                            turn_cost: cost,
                            cumulative_cost: self.cost_tracking.cumulative_cost,
                        });
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
        events: &mut Vec<TurnEvent>,
    ) -> anyhow::Result<()> {

        let tool = match self.tools.iter().find(|t| t.def().name == tc.name) {
            Some(t) => t.clone(),
            None => {
                let err = format!("Unknown tool: {}", tc.name);
                events.push(TurnEvent::Error(err.clone()));
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

        let is_destructive = matches!(tc.name.as_str(), "write_file" | "edit_file" | "bash");
        let default_action = if self.config.auto_approve {
            PermissionAction::Allow
        } else {
            PermissionAction::Ask
        };
        let mut action = evaluate(
            &self.config.permission_rules,
            &tc.name,
            &tc.arguments,
            default_action,
        );

        if self.config.auto_approve
            && tc.name == "bash"
            && matches!(action, PermissionAction::Allow)
        {
            if let Some(cmd) = tc.arguments.get("command").and_then(|v| v.as_str()) {
                if !is_read_only_bash(cmd) {
                    action = PermissionAction::Ask;
                }
            }
        }

        let needs_approval = is_destructive && matches!(action, PermissionAction::Ask);

        if matches!(action, PermissionAction::Deny) && is_destructive {
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
            events.push(TurnEvent::ToolResult {
                name: tc.name.clone(),
                output: reason.clone(),
                success: false,
            });
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
                ApprovalDecision::Approved | ApprovalDecision::AlwaysApproved => {

                }
                ApprovalDecision::Denied => {
                    events.push(TurnEvent::ToolResult {
                        name: tc.name.clone(),
                        output: "❌ User denied this operation".into(),
                        success: false,
                    });
                    self.conversation.append(Message {
                        role: Role::Tool,
                        content: "Operation denied by user.".into(),
                        tool_call_id: Some(tc.id.clone()),
                        tool_name: Some(tc.name.clone()),
                        ..Default::default()
                    })?;
                    return Ok(());
                }
            }
        }

        if let Some(denied) = check_deny_list(&self.deny_list, &tc.name, &tc.arguments) {
            events.push(TurnEvent::ToolResult {
                name: tc.name.clone(),
                output: denied.clone(),
                success: false,
            });
            self.conversation.append(Message {
                role: Role::Tool,
                content: denied,
                tool_call_id: Some(tc.id.clone()),
                tool_name: Some(tc.name.clone()),
                ..Default::default()
            })?;
            return Ok(());
        }

        if matches!(tc.name.as_str(), "read_file" | "write_file" | "edit_file") {
            let path_str = tc
                .arguments
                .get("path")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let path = std::path::Path::new(path_str);

            let verdict = match tc.name.as_str() {
                "read_file" => self.path_guard.check_read(path),
                "write_file" | "edit_file" => self.path_guard.check_write(path),
                _ => unreachable!(),
            };

            match verdict {
                GuardVerdict::Allowed(resolved) => {
                    if tc.name == "edit_file" {
                        if let GuardVerdict::Denied(msg) = self.read_gate.check_edit(path) {
                            let denied = format!("🔒 Access denied: {msg}");
                            events.push(TurnEvent::ToolResult {
                                name: tc.name.clone(),
                                output: denied.clone(),
                                success: false,
                            });
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

                    if tc.name == "read_file" {
                        self.read_gate.mark_read(&resolved);
                    }

                    let mut run_args = tc.arguments.clone();
                    if let Ok(path_obj) = serde_json::to_value(resolved.to_string_lossy().as_ref())
                    {
                        if let Some(obj) = run_args.as_object_mut() {
                            obj.insert("path".into(), path_obj);
                        }
                    }

                    events.push(TurnEvent::ToolStart {
                        name: tc.name.clone(),
                        args: run_args.clone(),
                    });

                    // Pre-tool hook
                    let args_json =
                        serde_json::to_string(&run_args).unwrap_or_default();
                    self.run_hook(
                        &format!("pre-tool-{}", tc.name),
                        Some(&tc.name),
                        Some(&args_json),
                    );

                    let outcome = tool.run(run_args.clone()).await;
                    let edit_diff = handle_tool_outcome(
                        outcome,
                        tc,
                        events,
                        &mut self.conversation,
                    )?;

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
                            &run_args,
                            None,
                            None,
                            None,
                            edit_diff,
                        )
                        .await;
                    self.collect_carryover(tc, &crs);
                    emit_correction_results(crs, tc, events, &mut self.conversation)?;
                    return Ok(());
                }
                GuardVerdict::Denied(msg) => {
                    let denied = format!("🔒 Access denied: {msg}");
                    events.push(TurnEvent::ToolResult {
                        name: tc.name.clone(),
                        output: denied.clone(),
                        success: false,
                    });
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
            if self.config.bash_sandbox_workdir
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
                self.config.bash_sandbox_workdir,
            ) {
                events.push(TurnEvent::ToolResult {
                    name: tc.name.clone(),
                    output: denied.clone(),
                    success: false,
                });
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
            let path_str = tc
                .arguments
                .get("path")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let path = std::path::Path::new(path_str);
            if let GuardVerdict::Denied(msg) = check_search_path(&self.path_guard, path) {
                let denied = format!("🔒 Access denied: {msg}");
                events.push(TurnEvent::ToolResult {
                    name: tc.name.clone(),
                    output: denied.clone(),
                    success: false,
                });
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

        events.push(TurnEvent::ToolStart {
            name: tc.name.clone(),
            args: tc.arguments.clone(),
        });

        // Pre-tool hook
        let args_json = serde_json::to_string(&tc.arguments).unwrap_or_default();
        self.run_hook(
            &format!("pre-tool-{}", tc.name),
            Some(&tc.name),
            Some(&args_json),
        );

        let outcome = tool.run(tc.arguments.clone()).await;

        let (real_exit_code, real_stdout_len, real_stderr_len) = if tc.name == "bash" {
            extract_bash_metrics(&outcome)
        } else {
            (None, None, None)
        };
        let outcome = if tc.name == "bash" {
            truncate_tool_output(outcome, self.config.max_tool_result_chars)
        } else {
            outcome
        };
        let edit_diff = handle_tool_outcome(outcome, tc, events, &mut self.conversation)?;

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
        emit_correction_results(crs, tc, events, &mut self.conversation)?;
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
                response: response_tx,
            })
            .map_err(|_| anyhow::anyhow!("approval channel closed"))?;

        match response_rx.await {
            Ok(ApprovalResponse::Approved) => Ok(ApprovalDecision::Approved),
            Ok(ApprovalResponse::Denied) => Ok(ApprovalDecision::Denied),
            Ok(ApprovalResponse::AlwaysApprove) => {

                let rule = crate::shared::permission::suggest_rule(&tc.name, &tc.arguments);
                push_rule_unique(&mut self.config.permission_rules, rule);
                Ok(ApprovalDecision::AlwaysApproved)
            }
            Err(_) => {

                Ok(ApprovalDecision::Denied)
            }
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

        let _ = self.event_bus.dispatch(&event).await;

        let Some(ref correction_loop) = self.correction_loop else {
            return vec![];
        };
        correction_loop.run(&event).await
    }
}

pub struct ApprovalRequest {
    pub tool_name: String,
    pub args: serde_json::Value,
    pub response: tokio::sync::oneshot::Sender<ApprovalResponse>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ApprovalResponse {
    Approved,
    Denied,
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
}

const DANGEROUS_SHELL_COMMANDS: &[&str] = &[
    "rm -rf /",
    "rm -rf /*",
    ":(){ :|:& };:",
    "> /dev/sda",
    "mkfs.",
    "dd if=/dev/zero of=",
    "chmod -R 777 /",
    "chmod 777 /",
    "dd if=/dev/random",
    "> /dev/null < /dev/sda",
];

const READ_ONLY_COMMANDS: &[&str] = &[
    "ls", "cat", "head", "tail", "pwd", "echo", "printf", "which", "type", "file", "stat", "du",
    "df", "env", "printenv", "true", "false", "dirname", "basename", "realpath", "readlink",
    "grep", "rg", "sort", "wc", "cut", "tr", "uniq", "fold", "nl", "diff", "cmp", "comm",
    "jq", "date", "cal", "whoami", "id", "uname", "hostname", "uptime", "ps", "free", "lscpu",
    "lsblk", "lsof", "dmesg", "nproc", "arch", "tty", "jobs", "help",
    // `find` used to be in this list, but `find -delete` (and `-exec rm`,
    // `-fprint`, etc.) is destructive. Removing it forces find through
    // the approval gate — deepseek-v4 review finding #3.
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

    let rest = &trimmed[first.len()..];

    if rest.contains('>') {
        return false;
    }

    for segment in trimmed.split('|') {
        let seg = segment.trim();
        if let Some(word) = seg.split_whitespace().next() {
            if word == "sh" || word == "bash" {
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

fn word_boundary_match(cmd: &str, pattern: &str) -> bool {
    let boundaries = [' ', '\t', '\n', '|', ';', '&', '(', ')', '<', '>', '\0'];
    let p: Vec<char> = pattern.chars().collect();
    let chars: Vec<char> = cmd.chars().collect();
    let mut i = 0;
    while i + p.len() <= chars.len() {
        if chars[i..i + p.len()].iter().collect::<String>() == *pattern {
            let start_ok = i == 0 || boundaries.contains(&chars[i - 1]);
            let end_ok = i + p.len() >= chars.len() || boundaries.contains(&chars[i + p.len()]);
            if start_ok && end_ok {
                return true;
            }
        }
        i += 1;
    }
    false
}

fn check_bash_command(
    args: &serde_json::Value,
    deny_list: &DenyList,
    path_guard: &PathGuard,
    bash_sandbox_workdir: bool,
) -> Option<String> {
    let cmd = args.get("command").and_then(|c| c.as_str())?;

    // 1. Sandboxed workdir policy. If enabled, the bash subprocess is
    //    confined to the sandbox — either by overriding the workdir
    //    arg (when missing) or by rejecting an explicit workdir that
    //    points outside the sandbox. This is the bash-policy half of
    //    GPT 5.5's review finding #4 ("bash can run in arbitrary
    //    workdir").
    if bash_sandbox_workdir {
        if let Some(workdir) = args.get("workdir").and_then(|w| w.as_str()) {
            if !workdir.is_empty() {
                let workdir_path = std::path::Path::new(workdir);
                let resolved = workdir_path
                    .canonicalize()
                    .unwrap_or_else(|_| workdir_path.to_path_buf());
                if let Some(ref sandbox) = path_guard.sandbox_dir {
                    let sb = sandbox.canonicalize().unwrap_or_else(|_| sandbox.clone());
                    if !resolved.starts_with(&sb) {
                        return Some(format!(
                            "🔒 Bash workdir outside sandbox: {} (sandbox: {})",
                            workdir,
                            sandbox.display()
                        ));
                    }
                }
            }
        }
    }

    // 2. Hard-coded metadata endpoint blocks. These are the well-known
    //    cloud metadata IPs/hostnames that the model must never reach
    //    regardless of user config.
    if cmd.contains("169.254.169.254")
        || cmd.contains("metadata.google")
        || cmd.contains("metadata.aws")
    {
        return Some("🔒 Command blocked: contains reference to metadata endpoints".into());
    }

    // 3. User-configured URL deny list. Scans the command string for
    //    any of the blocked URL prefixes. Naive substring match is
    //    fine for prefixes (`http://169.254.169.254`,
    //    `http://metadata.google.internal`) because they're meant to
    //    be hard prefixes.
    for url_prefix in &deny_list.url_patterns {
        if !url_prefix.is_empty() && cmd.contains(url_prefix) {
            return Some(format!(
                "🔒 Command blocked: references denied URL '{}'",
                url_prefix
            ));
        }
    }

    // 4. Built-in dangerous shell patterns (rm -rf /, etc.) and
    //    hard-coded system path deny list.
    for pattern in DANGEROUS_SHELL_COMMANDS {
        let needs_word_boundary = pattern.ends_with('/') || pattern.ends_with(' ');
        let matches = if needs_word_boundary {
            word_boundary_match(cmd, pattern)
        } else {
            cmd.contains(pattern)
        };
        if matches {
            return Some(format!(
                "🔒 Command blocked: dangerous pattern '{}' detected",
                pattern
            ));
        }
    }

    for pat in [
        "/etc/shadow",
        "/etc/passwd",
        "/etc/sudoers",
        "~/.ssh",
        "/root/",
    ] {
        if cmd.contains(pat) {
            return Some(format!(
                "🔒 Command blocked: references denied path '{}'",
                pat
            ));
        }
    }

    // 5. User-configured path deny list. Scans the command string for
    //    any of the blocked glob patterns. Glob matchers from
    //    `DenyList::is_path_denied` work on `Path` arguments, so we
    //    tokenize the command into whitespace-separated tokens and
    //    check each one as a path. This catches `rm **/.ssh/**` etc.
    for token in cmd.split_whitespace() {
        if deny_list.is_path_denied(std::path::Path::new(token)) {
            return Some(format!(
                "🔒 Command blocked: references denied path '{}'",
                token
            ));
        }
    }

    None
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
        return GuardVerdict::Denied(format!(
            "Path denied by deny list: {}",
            path.display()
        ));
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
        "read_file" | "write_file" | "edit_file" => {
            if let Some(path) = args.get("path").and_then(|v| v.as_str()) {
                let p = std::path::Path::new(path);
                if deny_list.is_path_denied(p) {
                    return Some(format!("🔒 Path denied by deny list: {}", path));
                }
            }
        }
        "bash" => {

        }
        "grep" | "glob" => {
            if let Some(path) = args.get("path").and_then(|v| v.as_str()) {
                let p = std::path::Path::new(path);
                if deny_list.is_path_denied(p) {
                    return Some(format!("🔒 Path denied by deny list: {}", path));
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
    events: &mut Vec<TurnEvent>,
    conversation: &mut ConversationLog,
) -> anyhow::Result<Option<String>> {
    match outcome {
        ToolOutcome::Success { content } => {
            events.push(TurnEvent::ToolResult {
                name: tc.name.clone(),
                output: content.clone(),
                success: true,
            });
            conversation.append(Message {
                role: Role::Tool,
                content,
                tool_call_id: Some(tc.id.clone()),
                tool_name: Some(tc.name.clone()),
                ..Default::default()
            })?;
        }
        ToolOutcome::FileContent { content, .. } => {
            events.push(TurnEvent::ToolResult {
                name: tc.name.clone(),
                output: content.clone(),
                success: true,
            });
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
            events.push(TurnEvent::ToolResult {
                name: tc.name.clone(),
                output: diff.clone(),
                success: true,
            });
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
            events.push(TurnEvent::ToolResult {
                name: tc.name.clone(),
                output: output.clone(),
                success: true,
            });
            conversation.append(Message {
                role: Role::Tool,
                content: output,
                tool_call_id: Some(tc.id.clone()),
                tool_name: Some(tc.name.clone()),
                ..Default::default()
            })?;
        }
        ToolOutcome::Error { message } => {
            events.push(TurnEvent::ToolResult {
                name: tc.name.clone(),
                output: format!("Error: {}", message),
                success: false,
            });
            conversation.append(Message {
                role: Role::Tool,
                content: format!("Error: {}", message),
                tool_call_id: Some(tc.id.clone()),
                tool_name: Some(tc.name.clone()),
                ..Default::default()
            })?;

            // Attempt error recovery — analyze the error and inject a hint
            if let Some(hint) = crate::session::error_recovery::analyze_error(
                &tc.name,
                &message,
                &tc.arguments,
            ) {
                let recovery_msg =
                    crate::session::error_recovery::build_recovery_message(&hint);
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
            let projection = format!("[image: {} ({}, {} bytes)]", path.display(), mime, data_base64.len());
            events.push(TurnEvent::ToolResult {
                name: tc.name.clone(),
                output: projection.clone(),
                success: true,
            });
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
            out.push_str(&format!("  {}\n", ctx));
        }
        out.push_str(&format!(">{}: {}\n", m.line_number, m.line));
        for ctx in &m.context_after {
            out.push_str(&format!("  {}\n", ctx));
        }
        out.push('\n');
    }
    out
}

fn emit_correction_results(
    results: Vec<CorrectionResult>,
    tc: &ToolInvocation,
    events: &mut Vec<TurnEvent>,
    conversation: &mut ConversationLog,
) -> anyhow::Result<()> {
    for cr in &results {
        events.push(TurnEvent::Verification {
            message: cr.message.clone(),
            success: cr.success,
        });
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
    use crate::shared::{
        FinishReason, Message, ModelInfo, Role, StreamEvent, TokenUsage, ToolCallStyle, ToolDef,
        ToolInvocation, ToolOutcome,
    };
    use std::sync::atomic::AtomicBool;
    use std::sync::{Arc, Mutex};
    use tokio::sync::mpsc;

    fn never_cancelled() -> &'static AtomicBool {
        static NC: std::sync::LazyLock<AtomicBool> =
            std::sync::LazyLock::new(|| AtomicBool::new(false));
        &NC
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

        async fn run(&self, args: serde_json::Value) -> ToolOutcome {
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
        }
    }

    fn make_executor(
        adapter: Box<dyn ModelAdapter>,
        tools: Vec<Arc<dyn Tool>>,
        config: Config,
    ) -> Executor {
        let temp_dir = std::env::temp_dir();
        let log_path = temp_dir.join(format!("kirkforge-test-{}.ndjson", std::process::id()));
        let _ = std::fs::remove_file(&log_path);
        let conversation = ConversationLog::open(log_path).unwrap();
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
            .run_turn("hello", &approval_tx, never_cancelled())
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
            .run_turn("use echo", &approval_tx, never_cancelled())
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
    async fn test_approval_required_for_destructive_tool() {
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

        let approval_handle = tokio::spawn(async move {
            let req: ApprovalRequest = approval_rx.recv().await.unwrap();
            assert_eq!(req.tool_name, "bash");
            let _ = req.response.send(ApprovalResponse::Approved);
        });

        let mut exe = make_executor(Box::new(adapter), vec![Arc::new(tool)], make_config(false));
        let events = exe
            .run_turn("run command", &approval_tx, never_cancelled())
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
            .run_turn("run command", &approval_tx, never_cancelled())
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
            .run_turn("run tests", &approval_tx, never_cancelled())
            .await
            .unwrap();
        approval_handle.await.unwrap();

        assert_eq!(
            exe.config.permission_rules.len(),
            1,
            "AlwaysApprove should have appended exactly one rule, got {:?}",
            exe.config.permission_rules
        );
        let r = &exe.config.permission_rules[0];
        assert_eq!(r.tool, "bash");
        assert_eq!(r.key, "command");
        assert_eq!(r.pattern, "cargo test --release");
        assert_eq!(r.action, PermissionAction::Allow);

        assert!(
            !exe.config.auto_approve,
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
            .run_turn("list", &approval_tx, never_cancelled())
            .await
            .unwrap();
        drop(approval_tx);
        approval_handle.await.unwrap();

        assert_eq!(
            exe.config.permission_rules.len(),
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
            .run_turn("clean", &approval_tx, never_cancelled())
            .await
            .unwrap();
        drop(approval_tx);
        approval_handle.await.unwrap();

        assert_eq!(exe.config.permission_rules.len(), 1);
        assert_eq!(
            exe.config.permission_rules[0].action,
            PermissionAction::Deny,
            "Existing Deny should not be overwritten by a new Allow on the same pattern"
        );
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
            .run_turn("clean build", &approval_tx, never_cancelled())
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
            "Expected a permission-rule denial message, got events: {:?}",
            events
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
            .run_turn("save creds", &approval_tx, never_cancelled())
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
            "Expected a deny-list refusal ToolResult, got events: {:?}",
            events
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

        let (approval_tx, mut approval_rx) = mpsc::unbounded_channel::<ApprovalRequest>();
        let approval_handle = tokio::spawn(async move {
            let req: ApprovalRequest = approval_rx.recv().await.expect("approval request");
            let _ = req.response.send(ApprovalResponse::Approved);
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
            .run_turn("wipe disk", &approval_tx, never_cancelled())
            .await
            .unwrap();
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
            "Expected a dangerous-pattern refusal, got events: {:?}",
            events
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
            .run_turn("build", &approval_tx, never_cancelled())
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
            .run_turn("do it", &approval_tx, never_cancelled())
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
            .run_turn("use unknown tool", &approval_tx, never_cancelled())
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
                            id: format!("call-{}", count),
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
        let mut exe = make_executor(Box::new(adapter), vec![Arc::new(tool)], make_config(true));
        let _events = exe
            .run_turn("loop", &approval_tx, never_cancelled())
            .await
            .unwrap();

        let tool_calls = *call_count.lock().unwrap();
        assert!(
            tool_calls <= 10,
            "Should not exceed max_iterations (was {})",
            tool_calls
        );
    }

    #[test]
    fn test_word_boundary_match_exact() {
        assert!(word_boundary_match("rm -rf /", "rm -rf /"));
    }

    #[test]
    fn test_word_boundary_no_false_positive_trailing_slash() {

        assert!(!word_boundary_match("rm -rf /home/user", "rm -rf /"));
    }

    #[test]
    fn test_word_boundary_match_with_pipe_prefix() {
        assert!(word_boundary_match("echo foo | rm -rf /", "rm -rf /"));
    }

    #[test]
    fn test_word_boundary_match_with_semicolon() {
        assert!(word_boundary_match("cd /; rm -rf /", "rm -rf /"));
    }

    #[test]
    fn test_word_boundary_no_match_in_substring() {
        assert!(!word_boundary_match("rm -rf /home", "rm -rf /"));
    }

    #[test]
    fn test_check_bash_command_blocks_dangerous_exact() {
        let args = serde_json::json!({"command": "rm -rf /"});
        let result = check_bash_command(
            &args,
            &DenyList::default(),
            &PathGuard::default(),
            false,
        );
        assert!(result.is_some(), "rm -rf / should be blocked");
    }

    #[test]
    fn test_check_bash_command_allows_safe_similar() {

        let args = serde_json::json!({"command": "rm -rf /home/user/temp"});
        let result = check_bash_command(
            &args,
            &DenyList::default(),
            &PathGuard::default(),
            false,
        );
        assert!(
            result.is_none(),
            "rm -rf /home/user/temp should be allowed, got: {:?}",
            result
        );
    }

    #[test]
    fn test_check_bash_command_blocks_dd_by_substring() {

        let args = serde_json::json!({"command": "dd if=/dev/zero of=/tmp/out bs=1M count=1"});
        let result = check_bash_command(
            &args,
            &DenyList::default(),
            &PathGuard::default(),
            false,
        );
        assert!(result.is_some(), "dd if=/dev/zero should be blocked");
    }

    #[test]
    fn test_check_bash_command_blocks_fork_bomb() {
        let args = serde_json::json!({"command": ":(){ :|:& };:"});
        let result = check_bash_command(
            &args,
            &DenyList::default(),
            &PathGuard::default(),
            false,
        );
        assert!(result.is_some(), "Fork bomb should be blocked");
    }

    #[test]
    fn test_check_bash_command_allows_legitimate_curl() {
        let args = serde_json::json!({"command": "curl -s https://api.example.com/data"});
        let result = check_bash_command(
            &args,
            &DenyList::default(),
            &PathGuard::default(),
            false,
        );
        assert!(
            result.is_none(),
            "curl should not be blocked by check_bash_command"
        );
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
        // `find` used to be in READ_ONLY_COMMANDS but was removed
        // (deepseek-v4 review finding). All find invocations now go
        // through the approval gate.
        assert!(!is_read_only_bash("find . -name '*.rs'"));
        assert!(!is_read_only_bash("find . -type f"));
        assert!(!is_read_only_bash("find ."));
    }

    #[test]
    fn test_is_read_only_bash_find_delete_specifically_blocked() {
        // The `find . -delete` case was the specific bypass that
        // motivated removing find from the read-only list. The test
        // stays explicit so a future change that adds find back with
        // a flag check can update this test accordingly.
        assert!(!is_read_only_bash("find . -delete"));
        assert!(!is_read_only_bash("find . -type f -delete"));
        assert!(!is_read_only_bash("find . -exec rm {} \\;"));
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
        let mut exe = make_executor(
            Box::new(adapter),
            vec![Arc::new(tool)],
            make_config(true),
        );
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
            .run_turn("edit it", &approval_tx, never_cancelled())
            .await
            .unwrap();

        let last = captured.last.lock().unwrap().clone();
        let got = last.expect("EditEvent should have been dispatched");
        assert!(
            got.contains("--- a") && got.contains("+++ b") && got.contains("-old line") && got.contains("+new line"),
            "EditEvent.diff should be the rendered diff, got: {:?}",
            got
        );
        assert!(
            got.starts_with("---") || got.contains("\n---"),
            "diff should start with --- header, got: {:?}",
            got
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
        let _guard = PostTurnHookGuard::new(HookRunner::default());
    }
}
