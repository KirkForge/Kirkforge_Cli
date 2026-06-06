use crate::adapters::ModelAdapter;
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

/// Push a permission rule into a `Vec<PermissionRule>`, deduplicating
/// against an existing identical rule. Two rules are considered
/// identical if they have the same `(tool, key, pattern)` triple
/// regardless of `action` — pushing `Allow bash:command=ls` twice
/// only stores one. The `action` of the EXISTING rule is preserved
/// (so a user-written `Deny` for `rm` won't be silently overwritten
/// by an `Allow` from `[A]lways`).
///
/// **Why dedup at the call site instead of the TUI side:** both
/// `approval_keys.rs` (TUI) and `executor.rs` (engine) push rules
/// into their own `permission_rules` clones. Without dedup, hitting
/// `[A]lways` once would land the rule in both — but hitting it
/// twice on the same call (e.g. user mashes the key) would land
/// duplicates in whichever side is racing. This helper is the
/// single point of dedup, called from both sites.
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

/// What a single stream iteration produced. The orchestrator
/// (`run_turn`) drives this state machine.
///
/// **Why a private enum instead of nested Result types:** the
/// caller needs to distinguish three orthogonal outcomes
/// (continue, finish, retry) without an `Option<Option<…>>` pyramid.
/// The variants are exhaustive and small.
enum IterationOutcome {
    /// Model emitted tool calls; the orchestrator should dispatch
    /// each one and then loop for the model's next response.
    ToolCalls(Vec<ToolInvocation>),
    /// Model produced a final text response (or cancelled); the
    /// turn is over.
    Finished,
    /// Adapter reported a JSON-parse error. The orchestrator will
    /// inject a one-shot retry message and re-stream.
    ParseError,
}

/// Outcome of asking the user (or permission rules) whether a
/// destructive call may proceed. Returned by `run_approval_flow`.
///
/// `AlwaysApproved` is distinct from `Approved` so callers that
/// care about persistence (e.g. a future audit log) can
/// differentiate "this time" from "and also for future similar
/// calls". The current `dispatch_tool_call` ignores the
/// distinction; that's fine — the enum is the durable seam.
enum ApprovalDecision {
    Approved,
    Denied,
    AlwaysApproved,
}

/// The session executor runs the conversation loop:
///   1. Build the prompt → send to model → collect stream events
///   2. Emit text tokens to the UI
///   3. When tool calls arrive: validate → approve → execute → feed result back
///   4. Repeat until done
pub struct Executor {
    adapter: Box<dyn ModelAdapter>,
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

    // ── Carryover (Phase 17) ──────────────────────────────────
    /// The live carryover profile being accumulated this session.
    carryover: CarryoverProfile,
    /// Whether carryover is enabled (from config).
    carryover_enabled: bool,
    /// Shared target for saving after the executor exits.
    /// The TUI holds the other clone and saves after awaiting the handle.
    carryover_target: Option<std::sync::Arc<std::sync::Mutex<CarryoverProfile>>>,
}

impl Executor {
    pub fn new(adapter: Box<dyn ModelAdapter>, tools: Vec<Arc<dyn Tool>>, config: Config) -> Self {
        // Create a default temp conversation log
        let temp_dir = std::env::temp_dir().join("kirkforge-session");
        let log_path = temp_dir.join(format!(
            "session-{}.ndjson",
            chrono::Local::now().format("%Y%m%d-%H%M%S")
        ));
        let conversation = ConversationLog::open(log_path).unwrap();
        Self::with_log(adapter, tools, config, conversation, None)
    }

    pub fn with_log(
        adapter: Box<dyn ModelAdapter>,
        tools: Vec<Arc<dyn Tool>>,
        config: Config,
        conversation: ConversationLog,
        carryover_target: Option<std::sync::Arc<std::sync::Mutex<CarryoverProfile>>>,
    ) -> Self {
        let model_name = adapter.model_info().name.clone();
        let (deny_list, path_guard, read_gate) = access_from_config(&config);
        warn_if_unsandboxed(&path_guard);

        // Event bus — verifiers are registered by init_default_verifiers() called below
        let event_bus = EventBus::new();

        let carryover_enabled = config.carryover_enabled;
        let carryover = if carryover_enabled {
            crate::session::carryover::load_carryover()
        } else {
            CarryoverProfile::default()
        };

        let mut this = Self {
            adapter,
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

    /// Initialize default verifiers (security, lint, git).
    /// Returns the number of registered verifiers.
    pub fn init_default_verifiers(&mut self) -> usize {
        use crate::session::verifier::{Verdict, Verifier};

        let slots = Arc::new(std::sync::RwLock::new(VerifierSlots::new()));
        let mut count = 0;

        // Security verifier (priority 1 — runs first)
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

        // Lint verifier (priority 2)
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

        // Git verifier (priority 3)
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

    /// Run the executor as a long-lived background task.
    ///
    /// Listens for user input on `input_rx`, runs the full turn loop
    /// (stream → tool calls → approval → execute), and forwards all
    /// renderable events to the TUI via `event_tx`.
    ///
    /// When `()` arrives on `cancel_rx`, the current `run_turn()` is
    /// interrupted at the next yield point and subsequent turns are skipped
    /// until a new input arrives (which resets the flag).
    ///
    /// When a `ConversationLog` arrives on `resume_rx`, the executor's
    /// conversation is replaced in-place (fork resumption).
    ///
    /// When `()` arrives on `compact_rx`, the executor runs a
    /// `/compact`-style compaction: `PromptBuilder::compact` reduces
    /// the conversation, `ConversationLog::replace_all` atomically
    /// rewrites the NDJSON file, and a `TurnEvent::CompactionReport`
    /// is sent back via `event_tx` carrying the new message list
    /// (so the TUI can rebuild its display from the same source of
    /// truth).
    pub async fn run(
        &mut self,
        mut input_rx: mpsc::UnboundedReceiver<String>,
        event_tx: mpsc::UnboundedSender<TurnEvent>,
        approval_tx: mpsc::UnboundedSender<ApprovalRequest>,
        mut cancel_rx: mpsc::UnboundedReceiver<()>,
        mut resume_rx: mpsc::UnboundedReceiver<ConversationLog>,
        mut compact_rx: mpsc::UnboundedReceiver<()>,
    ) -> anyhow::Result<()> {
        let cancelled = Arc::new(AtomicBool::new(false));

        loop {
            tokio::select! {
                biased; // check cancel first, then input

                Some(()) = cancel_rx.recv() => {
                    cancelled.store(true, Ordering::SeqCst);
                    if event_tx.send(TurnEvent::Token("\n⚠️ Generation cancelled\n".into())).is_err() {
                        // TUI receiver gone — the user closed the window,
                        // hit Ctrl+C at the wrong layer, or the renderer
                        // task panicked. No point keeping the executor
                        // alive when nobody's listening. Flush
                        // carryover and bail (same idiom as the Ok
                        // path below when a per-event send fails).
                        tracing::warn!("TUI event receiver dropped; executor driver exiting");
                        self.flush_carryover();
                        return Ok(());
                    }
                }
                Some(new_log) = resume_rx.recv() => {
                    // Resume from a fork — swap the conversation log
                    self.replace_conversation(new_log);
                    if event_tx.send(TurnEvent::Token("✅ Resumed from fork\n".into())).is_err() {
                        tracing::warn!("TUI event receiver dropped during /resume; exiting");
                        self.flush_carryover();
                        return Ok(());
                    }
                }
                Some(()) = compact_rx.recv() => {
                    // /compact: walk the current history, condense
                    // middle turns, drop middle tool results, rewrite
                    // the NDJSON file atomically, and emit a report
                    // so the TUI can rebuild its display.
                    let history = self.conversation.all();
                    let result = crate::session::prompt::PromptBuilder::compact(history);
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
                Some(input) = input_rx.recv() => {
                    cancelled.store(false, Ordering::SeqCst);
                    let events = self.run_turn(&input, &approval_tx, &cancelled).await;
                    match events {
                        Ok(evs) => {
                            for ev in evs {
                                if event_tx.send(ev).is_err() {
                                    // Same idiom as the other branches
                                    // above: TUI gone, no audience, no
                                    // point continuing. Bail cleanly so
                                    // the tokio task doesn't keep
                                    // running model requests that
                                    // nobody will see.
                                    tracing::warn!("TUI event receiver dropped mid-turn; executor driver exiting");
                                    self.flush_carryover();
                                    return Ok(());
                                }
                            }
                        }
                        Err(e) => {
                            // One last attempt to surface the error to
                            // the TUI; if the receiver is gone, the
                            // session is over anyway. Don't swallow.
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

    /// Borrow the conversation log (for non-interactive JSON summaries).
    pub fn conversation_log(&self) -> &ConversationLog {
        &self.conversation
    }

    /// Replace the conversation log in-place (used by /resume fork).
    pub fn replace_conversation(&mut self, new_log: ConversationLog) {
        self.conversation = new_log;
    }

    /// Flush the carryover profile to the shared target at session end.
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

    /// Collect carryover data from a completed tool call + its corrections.
    fn collect_carryover(&mut self, tc: &ToolInvocation, crs: &[CorrectionResult]) {
        if !self.carryover_enabled {
            return;
        }
        self.carryover.record_tool_call(&tc.name);
        // Extract path if present
        if let Some(path) = tc.arguments.get("path").and_then(|v| v.as_str()) {
            if !path.is_empty() {
                self.carryover.record_path(path);
            }
        }
        // Track test-after-change pattern from bash commands
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
        // Track verifier warnings
        for cr in crs {
            self.carryover.record_verifier_warning(&cr.message);
        }
    }

    /// Run a single turn: send the current conversation to the model,
    /// handle tool calls, return the assistant's response.
    ///
    /// This is now a thin orchestrator over [`stream_iteration`] and
    /// [`dispatch_tool_call`]. It owns the iteration loop, the
    /// tool-call limit, and the one-shot parse-error retry.
    ///
    /// If `cancelled` is provided, it is checked at each streaming
    /// yield point and the turn stops early with a cancellation notice.
    pub async fn run_turn(
        &mut self,
        user_input: &str,
        approval_sender: &mpsc::UnboundedSender<ApprovalRequest>,
        cancelled: &AtomicBool,
    ) -> anyhow::Result<Vec<TurnEvent>> {
        // Append user message
        self.conversation.append(Message {
            role: Role::User,
            content: user_input.to_string(),
            thinking: None,
            tool_calls: None,
            tool_call_id: None,
            tool_name: None,
            token_count: None,
        })?;

        // ── Carryover: capture last user message ──
        if self.carryover_enabled {
            self.carryover.last_user_message = user_input.to_string();
        }

        let mut events = Vec::new();
        let mut tool_calls: Vec<ToolInvocation> = Vec::new();
        let mut already_retried_parse = false;

        // Safety cap on tool-call loops. The model is supposed to
        // reach a `FinishReason::Stop` before this, but a misbehaving
        // adapter could loop forever.
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
                IterationOutcome::ToolCalls(tcs) => {
                    for tc in &tcs {
                        self.dispatch_tool_call(tc, approval_sender, &mut events)
                            .await?;
                    }
                    // Loop again so the model can react to the tool results.
                }
                IterationOutcome::ParseError => {
                    if !already_retried_parse {
                        already_retried_parse = true;
                        // One-shot nudge: ask the model to re-emit
                        // just the tool call with valid JSON. We
                        // deliberately don't speculate about what
                        // was wrong — the model saw its own bad
                        // output in the conversation log.
                        let retry_msg = "Your previous response contained a tool call with malformed JSON arguments. Re-emit ONLY the tool call with the corrected JSON — no additional text, no explanation.";
                        self.conversation.append(Message {
                            role: Role::User,
                            content: retry_msg.into(),
                            thinking: None,
                            tool_calls: None,
                            tool_call_id: None,
                            tool_name: None,
                            token_count: None,
                        })?;
                        events.push(TurnEvent::Token("(JSON parse error, retrying…)\n".into()));
                    } else {
                        // Model failed twice. Give up and let the
                        // user see the events we've collected.
                        return Ok(events);
                    }
                }
            }

            if iteration + 1 >= MAX_ITERATIONS {
                events.push(TurnEvent::Error("Tool call loop limit reached".into()));
                return Ok(events);
            }
        }

        Ok(events)
    }

    /// Run one pass of: build prompt → stream → drain events →
    /// append assistant message → record cost.
    ///
    /// On `StreamEvent::Done`, decides between three outcomes:
    /// - `ToolCalls` — pending tool calls were collected; return
    ///   them to the orchestrator for dispatch + next iteration.
    /// - `Finished` — no tool calls, no parse error; turn is over.
    /// - `ParseError` — adapter reported a JSON parse error;
    ///   orchestrator may retry.
    ///
    /// `tool_calls_out` is borrowed from the orchestrator so
    /// completed tool calls land in the same buffer the caller
    /// iterates over after we return. Avoids a second `Vec` clone
    /// for the (common) "all calls completed, loop again" path.
    ///
    /// `approval_sender` is currently unused at this layer (the
    /// stream itself never needs to prompt), but we keep it in
    /// the signature so the orchestrator has one consistent
    /// place to pass the channel to both helpers.
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

        // Adapter-reported parse errors (JSON parse, no parseable
        // entries). We only return `ParseError` once per turn (the
        // orchestrator enforces the one-shot retry), but we still
        // need to track it here so we don't suppress errors that
        // arrive alongside a `Done` with tool calls.
        let mut had_parse_error = false;

        while let Some(event) = rx.recv().await {
            if cancelled.load(Ordering::SeqCst) {
                events.push(TurnEvent::Token("\n⚠️ Cancelled\n".into()));
                // Record what we have so far before exiting.
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
                    // Surface parse-related errors from adapters so
                    // the orchestrator can decide whether to retry.
                    if e.contains("parse") || e.contains("parseable") {
                        had_parse_error = true;
                    }
                    events.push(TurnEvent::Error(e));
                }
                StreamEvent::Done {
                    finish_reason: _,
                    usage,
                } => {
                    // Persist the assistant message (the body of
                    // text/thinking/tool-calls we just streamed).
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
                        tool_call_id: None,
                        tool_name: None,
                        token_count: usage.as_ref().and_then(|u| u.completion_tokens),
                    };
                    self.conversation.append(msg)?;

                    // Record cost
                    if let Some(ref u) = usage {
                        let prompt = u.prompt_tokens.unwrap_or(0);
                        let completion = u.completion_tokens.unwrap_or(0);
                        let cost =
                            crate::shared::calculate_cost(&self.model_name, prompt, completion);
                        self.cost_tracking.record_turn(prompt, completion, cost);
                        events.push(TurnEvent::CostStats {
                            prompt_tokens: prompt,
                            completion_tokens: completion,
                            turn_cost: cost,
                            cumulative_cost: self.cost_tracking.cumulative_cost,
                        });
                    }

                    if !tool_calls_out.is_empty() {
                        // Caller will dispatch them and loop again.
                        return Ok(IterationOutcome::ToolCalls(tool_calls_out.clone()));
                    }

                    // No tool calls. If the model reported a parse
                    // error, the orchestrator decides whether to
                    // retry. Otherwise the turn is done.
                    return Ok(if had_parse_error {
                        IterationOutcome::ParseError
                    } else {
                        IterationOutcome::Finished
                    });
                }
            }
        }

        // Stream ended without an explicit Done — treat as
        // finished. We may have missed parse errors in the
        // tail; surface them so the orchestrator can decide.
        if had_parse_error {
            Ok(IterationOutcome::ParseError)
        } else {
            Ok(IterationOutcome::Finished)
        }
    }

    /// Dispatch a single tool call through the full pipeline:
    /// tool lookup → permission rules → approval → deny list →
    /// path guard → read gate → bash minify → execute → event
    /// emit → correction loop → carryover.
    ///
    /// **Why this is one function and not a chain of small
    /// helpers:** the steps share state (e.g. the resolved path
    /// from the path guard feeds the read gate, the corrected
    /// path is the one the tool actually runs against). Splitting
    /// them forces the caller to thread that state through
    /// signatures; keeping them inlined here means the dataflow
    /// is visible in one place. The "if it's not destructive,
    /// skip approval" early return keeps the common case
    /// (read_file, grep, glob) fast.
    async fn dispatch_tool_call(
        &mut self,
        tc: &ToolInvocation,
        approval_sender: &mpsc::UnboundedSender<ApprovalRequest>,
        events: &mut Vec<TurnEvent>,
    ) -> anyhow::Result<()> {
        // 1. Tool lookup
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

        // 2. Permission rule evaluation.
        //    The `auto_approve` flag sets the default action
        //    (true → Allow, false → Ask); `permission_rules`
        //    can override per-call. First matching rule wins.
        let is_destructive = matches!(tc.name.as_str(), "write_file" | "edit_file" | "bash");
        let default_action = if self.config.auto_approve {
            PermissionAction::Allow
        } else {
            PermissionAction::Ask
        };
        let action = evaluate(
            &self.config.permission_rules,
            &tc.name,
            &tc.arguments,
            default_action,
        );
        let needs_approval = is_destructive && matches!(action, PermissionAction::Ask);

        // 3. Deny-list rule: refuse without prompting.
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

        // 4. Approval gate (file tools under !auto_approve, or
        //    non-read-only bash under auto_approve).
        if needs_approval {
            match self.run_approval_flow(tc, approval_sender).await? {
                ApprovalDecision::Approved | ApprovalDecision::AlwaysApproved => {
                    // Proceed to access checks below.
                }
                ApprovalDecision::Denied => {
                    events.push(TurnEvent::ToolResult {
                        name: tc.name.clone(),
                        output: "❌ User denied this operation".into(),
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

        // 5. Access control: deny list (all tools)
        if let Some(denied) = check_deny_list(&self.deny_list, &tc.name, &tc.arguments) {
            events.push(TurnEvent::ToolResult {
                name: tc.name.clone(),
                output: denied.clone(),
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

        // 6. File tools: path guard + read-before-edit gate
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

                    // Stash resolved path back for the tool.
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
                    let outcome = tool.run(run_args.clone()).await;
                    handle_tool_outcome(outcome, tc, events, &mut self.conversation)?;
                    let crs = self
                        .emit_tool_event_and_correct(tc, &tc.name, &run_args, None, None, None)
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

        // 7. Bash: deny-list + read-only classification for auto-approve
        if tc.name == "bash" {
            if let Some(denied) = check_bash_command(&tc.arguments) {
                events.push(TurnEvent::ToolResult {
                    name: tc.name.clone(),
                    output: denied.clone(),
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

        // 8. Execute
        events.push(TurnEvent::ToolStart {
            name: tc.name.clone(),
            args: tc.arguments.clone(),
        });
        let outcome = tool.run(tc.arguments.clone()).await;

        // 9. Cap output for bash to avoid blowing context
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
        handle_tool_outcome(outcome, tc, events, &mut self.conversation)?;

        // 10. Bus event + correction loop + carryover
        let crs = self
            .emit_tool_event_and_correct(
                tc,
                &tc.name,
                &tc.arguments,
                real_exit_code,
                real_stdout_len,
                real_stderr_len,
            )
            .await;
        self.collect_carryover(tc, &crs);
        emit_correction_results(crs, tc, events, &mut self.conversation)?;
        Ok(())
    }

    /// Run the "ask the user / wait for response" approval flow.
    ///
    /// Sends an [`ApprovalRequest`] over `approval_sender`, awaits
    /// the user's decision, and (on `AlwaysApprove`) pushes a
    /// matching `permission_rules` entry so future similar calls
    /// skip the prompt entirely. This is the single home for the
    /// duplicated approval logic that previously lived inline at
    /// two call sites in `run_turn` (file tools under
    /// `!auto_approve` and non-read-only bash under
    /// `auto_approve`).
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
                // **v1.2-p13 — permission rule persistence.**
                // Build a rule matching THIS specific call (tool
                // name + the key argument — `command` for bash,
                // `path` for edit_file) and push it into the
                // session's `permission_rules`. Future calls
                // matching this rule will skip the approval
                // dialog. The TUI side ALSO persists so the
                // rule survives across sessions via
                // `save_config`; this in-memory push keeps the
                // rest of THIS turn consistent.
                //
                // Deliberately does NOT flip `auto_approve`:
                // the user asked for "always this specific
                // command" — not "always everything". The old
                // `auto_approve = true` was a blanket bypass
                // that the new per-call rules replace.
                let rule = crate::shared::permission::suggest_rule(&tc.name, &tc.arguments);
                push_rule_unique(&mut self.config.permission_rules, rule);
                Ok(ApprovalDecision::AlwaysApproved)
            }
            Err(_) => {
                // Sender was dropped (UI exited). Conservatively
                // deny — better to refuse than to silently
                // execute.
                Ok(ApprovalDecision::Denied)
            }
        }
    }

    /// Emit a tool event on the event bus and run the correction loop.
    ///
    /// Constructs the appropriate BusEvent based on tool name and
    /// arguments, dispatches it to registered verifiers, and collects
    /// any correction results.
    async fn emit_tool_event_and_correct(
        &self,
        _tc: &ToolInvocation,
        tool_name: &str,
        args: &serde_json::Value,
        real_exit_code: Option<i32>,
        real_stdout_len: Option<usize>,
        real_stderr_len: Option<usize>,
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
                let diff = args
                    .get("old_string")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
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

        // Dispatch on event bus (fire-and-forget to registered handlers)
        let _ = self.event_bus.dispatch(&event).await;

        // Run correction loop
        let Some(ref correction_loop) = self.correction_loop else {
            return vec![];
        };
        correction_loop.run(&event).await
    }
}

/// Request from executor to the UI for user approval.
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

/// An event emitted during a turn, for the UI to render.
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
    /// Emitted when a `/compact` request has finished processing.
    /// `new_messages` is the compacted history that the TUI should
    /// use to rebuild its display. The executor's own
    /// `self.conversation` is already pointing at this new list
    /// (and the on-disk log has been atomically rewritten).
    CompactionReport {
        new_messages: Vec<crate::shared::Message>,
        dropped_tool_results: usize,
        condensed_assistant_turns: usize,
        original_count: usize,
        compacted_count: usize,
    },
}

/// Dangerous shell command patterns — always blocked.
/// (A subset of disk-wrecking patterns is also checked in verifier/security.rs).
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

/// Bash commands classified as read-only for auto-approve purposes.
/// Only the first word is checked. Commands starting with one of these
/// auto-approve UNLESS they chain into dangerous operations (redirects,
/// pipe-to-shell, chaining, command substitution).
const READ_ONLY_COMMANDS: &[&str] = &[
    "ls", "cat", "head", "tail", "pwd", "echo", "printf", "which", "type", "file", "stat", "du",
    "df", "env", "printenv", "true", "false", "dirname", "basename", "realpath", "readlink",
    "grep", "rg", "find", "sort", "wc", "cut", "tr", "uniq", "fold", "nl", "diff", "cmp", "comm",
    "jq", "date", "cal", "whoami", "id", "uname", "hostname", "uptime", "ps", "free", "lscpu",
    "lsblk", "lsof", "dmesg", "nproc", "arch", "tty", "jobs", "help",
];

/// Check whether a bash command is read-only (safe to auto-approve).
/// A command is read-only if its first word is a known read-only command
/// AND it doesn't chain into dangerous operations via:
///   - File redirects (> or >>)
///   - Pipe to shell (|sh or | bash or any segment starting with sh/bash)
///   - Command chaining (;, &&, ||)
///   - Command substitution ($(...) or backtick)
fn is_read_only_bash(cmd: &str) -> bool {
    let trimmed = cmd.trim();
    if trimmed.is_empty() {
        return true;
    }
    // Extract the first word
    let trimmed_stripped = trimmed.trim_start();
    let first = match trimmed_stripped.find(|c: char| c.is_whitespace()) {
        Some(pos) => &trimmed_stripped[..pos],
        None => trimmed_stripped,
    };

    if !READ_ONLY_COMMANDS.contains(&first) {
        return false;
    }

    // Check the rest of the command for risky operators
    let rest = &trimmed[first.len()..];

    // File redirect (> or >>)
    if rest.contains('>') {
        return false;
    }

    // Pipe to shell: check if any pipe-separated segment starts with sh or bash
    for segment in trimmed.split('|') {
        let seg = segment.trim();
        if let Some(word) = seg.split_whitespace().next() {
            if word == "sh" || word == "bash" {
                return false;
            }
        }
    }

    // Command chaining (;, &&, ||)
    if rest.contains(';') || rest.contains("&&") || rest.contains("||") {
        return false;
    }

    // Command substitution ($(...) or backtick)
    if rest.contains("$(") || rest.contains('`') {
        return false;
    }

    true
}

/// Check if a pattern appears as a whole word in a command string.
/// A "word boundary" is whitespace, pipe, semicolon, parens, angle brackets,
/// or start/end of the string.
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

/// Pre-execution bash command check — blocks dangerous patterns before they run.
fn check_bash_command(args: &serde_json::Value) -> Option<String> {
    let cmd = args.get("command").and_then(|c| c.as_str())?;

    // 1. Metadata endpoint access (cloud provider metadata)
    if cmd.contains("169.254.169.254")
        || cmd.contains("metadata.google")
        || cmd.contains("metadata.aws")
    {
        return Some("🔒 Command blocked: contains reference to metadata endpoints".into());
    }

    // 2. Dangerous command patterns (the opposite of read-only)
    //
    // Patterns ending in `/` or ` ` ("rm -rf /", "chmod 777 /") would
    // false-positive on legitimate paths ("rm -rf /home") with .contains(),
    // so they use word-boundary matching. Everything else uses .contains().
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

    // 3. Path deny-list (applied to paths appearing in the command)
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

    None
}

/// Truncate tool output to a maximum number of characters.
/// Applied to bash output to prevent context-window overflow.
fn truncate_tool_output(outcome: ToolOutcome, max_chars: usize) -> ToolOutcome {
    if max_chars == 0 {
        return outcome;
    }
    match outcome {
        ToolOutcome::Success { content } => {
            if content.len() > max_chars {
                // Walk back to a UTF-8 char boundary to avoid a panic on multi-byte chars
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

/// Extract bash execution metrics from a ToolOutcome.
/// Returns (outcome, exit_code, stdout_len, stderr_len).
/// For the verifier to see real data instead of hardcoded zeros.
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

/// Helper: check deny list before running a tool.
/// Returns Some(denial_message) if blocked, None if allowed.
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
            // Note: pre-execution dangerous-command check is in check_bash_command()
            // which runs right before tool execution. This function only handles
            // the generic deny-list (path patterns, URLs) for consistency.
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

/// Helper: process a tool outcome and push events/conversation entries.
fn handle_tool_outcome(
    outcome: ToolOutcome,
    tc: &ToolInvocation,
    events: &mut Vec<TurnEvent>,
    conversation: &mut ConversationLog,
) -> anyhow::Result<()> {
    match outcome {
        ToolOutcome::Success { content }
        | ToolOutcome::FileContent { content, .. }
        | ToolOutcome::FileEdit { diff: content, .. } => {
            events.push(TurnEvent::ToolResult {
                name: tc.name.clone(),
                output: content.clone(),
            });
            conversation.append(Message {
                role: Role::Tool,
                content,
                tool_call_id: Some(tc.id.clone()),
                tool_name: Some(tc.name.clone()),
                ..Default::default()
            })?;
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
            });
            conversation.append(Message {
                role: Role::Tool,
                content: format!("Error: {}", message),
                tool_call_id: Some(tc.id.clone()),
                tool_name: Some(tc.name.clone()),
                ..Default::default()
            })?;
        }
    }
    Ok(())
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

/// Helper: emit correction results as TurnEvents and conversation entries.
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

    /// Convenience: an `AtomicBool` that is always false — used by tests
    /// that don't need cancellation.
    fn never_cancelled() -> &'static AtomicBool {
        static NC: std::sync::LazyLock<AtomicBool> =
            std::sync::LazyLock::new(|| AtomicBool::new(false));
        &NC
    }

    // ── Mocks ──────────────────────────────────────────────────────

    struct MockAdapter {
        /// Events to emit on the first stream() call.
        first_events: Vec<StreamEvent>,
        /// Events to emit on subsequent stream() calls (e.g. empty to break tool-call loops).
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
            carryover_enabled: false,
            permission_rules: vec![],
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

    // ── Tests ─────────────────────────────────────────────────────

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

        // Verify conversation was appended
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

        // Should have token + tool_start + tool_result
        let has_token = events.iter().any(|e| matches!(e, TurnEvent::Token(_)));
        let has_start = events
            .iter()
            .any(|e| matches!(e, TurnEvent::ToolStart { name, .. } if name == "echo"));
        let has_result = events.iter().any(|e| matches!(e, TurnEvent::ToolResult { name, output } if name == "echo" && output == "echoed!"));

        assert!(has_token, "Should stream text before tool call");
        assert!(has_start, "Should emit ToolStart");
        assert!(has_result, "Should emit ToolResult");

        // Verify tool was actually called with correct args
        let called_with = captured.lock().unwrap().take();
        assert!(called_with.is_some(), "Tool should have been called");
        assert_eq!(
            called_with.unwrap().get("val").and_then(|v| v.as_str()),
            Some("test")
        );

        // Tool result should be in conversation
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

        // auto_approve = false — should require approval
        let (approval_tx, mut approval_rx) = mpsc::unbounded_channel();

        // Spawn a task to respond to the approval request
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

        // Should have ToolResult since we approved
        let result = events.iter().find_map(|e| match e {
            TurnEvent::ToolResult { name, output } => Some((name.as_str(), output.as_str())),
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

        // Tool should NOT have been called (denied)
        assert!(
            captured.lock().unwrap().is_none(),
            "Tool should not have been called when denied"
        );

        // Should have a denied-message result
        let denied = events.iter().any(|e| matches!(e, TurnEvent::ToolResult { name, output } if name == "bash" && output.contains("denied")));
        assert!(denied, "Should report that operation was denied");
    }

    /// **v1.2-p13 — `[A]lways` should push a permission rule, NOT
    /// flip `auto_approve`.** The old code did `self.config.auto_approve
    /// = true;` which was a blanket bypass for the rest of the session.
    /// The new code builds a rule matching THIS specific call and
    /// pushes it into `permission_rules`, so future matching calls
    /// skip the dialog but unrelated destructive calls still ask.
    #[tokio::test]
    async fn test_always_approve_pushes_permission_rule_not_auto_approve() {
        let captured = Arc::new(Mutex::new(None));
        let tool = MockTool {
            def: ToolDef {
                name: "bash".into(),
                description: "run a command".into(),
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
            // User hit `[A]lways`.
            let _ = req.response.send(ApprovalResponse::AlwaysApprove);
        });

        // Start with auto_approve = false and an empty permission_rules
        // list — the realistic starting state.
        let config = make_config(false);
        assert!(config.permission_rules.is_empty());
        assert!(!config.auto_approve);

        let mut exe = make_executor(Box::new(adapter), vec![Arc::new(tool)], config);
        let _events = exe
            .run_turn("run tests", &approval_tx, never_cancelled())
            .await
            .unwrap();
        approval_handle.await.unwrap();

        // **The new rule must be in permission_rules.**
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

        // **auto_approve must NOT have been flipped.** This is the
        // regression guard for the old buggy behaviour.
        assert!(
            !exe.config.auto_approve,
            "AlwaysApprove should NOT flip auto_approve — the new rule is the user's intent"
        );
    }

    /// Hitting `[A]lways` twice on the same call should NOT add a
    /// duplicate rule. This is the in-memory counterpart to the TUI
    /// `push_rule_unique` test — both sites use the same dedup
    /// strategy, and both should be regression-tested.
    #[tokio::test]
    async fn test_always_approve_dedups_repeated_calls() {
        let captured = Arc::new(Mutex::new(None));
        let tool = MockTool {
            def: ToolDef {
                name: "bash".into(),
                description: "run a command".into(),
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
        // `drop(approval_tx)` at the end of this test closes the
        // channel, which makes the spawned `approval_rx.recv()`
        // return `None` and complete. In THIS test the pre-populated
        // `Allow` rule on `bash:command=ls` short-circuits the
        // approval flow (action = Allow, needs_approval = false),
        // so the spawned task never receives an `ApprovalRequest` —
        // we rely on the channel-close to unblock it.
        let approval_handle = tokio::spawn(async move {
            while let Some(req) = approval_rx.recv().await {
                // Rule short-circuited — no approval will ever be
                // sent. We still handle it defensively in case the
                // test author changes the rule setup.
                let _ = req.response.send(ApprovalResponse::AlwaysApprove);
            }
        });

        let config = make_config(false);
        // Pre-populate with the EXACT rule that suggest_rule would
        // build for `bash:command=ls`. The dedup should leave it
        // untouched (and not add a duplicate).
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

        // Still exactly one rule — no duplicate added.
        assert_eq!(
            exe.config.permission_rules.len(),
            1,
            "AlwaysApprove should dedup against an existing identical rule"
        );
    }

    /// User-written `Deny` rule should NOT be overwritten by an
    /// `Allow` from `[A]lways` on the same pattern. The dedup logic
    /// preserves the EXISTING rule's action. This catches a "user
    /// denies `rm -rf` then accidentally allows it by hitting
    /// `[A]lways`" footgun.
    #[tokio::test]
    async fn test_always_approve_does_not_overwrite_existing_deny() {
        let captured = Arc::new(Mutex::new(None));
        let tool = MockTool {
            def: ToolDef {
                name: "bash".into(),
                description: "run a command".into(),
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
        // Same pattern as the dedup test above: the pre-populated
        // `Deny` rule on `rm -rf build` short-circuits the approval
        // flow (action = Deny → refuses without prompting), so the
        // spawned task never receives an `ApprovalRequest`. We make
        // the receiver robust to channel close + drop the sender
        // after the turn to unblock `recv()`.
        let approval_handle = tokio::spawn(async move {
            while let Some(req) = approval_rx.recv().await {
                let _ = req.response.send(ApprovalResponse::AlwaysApprove);
            }
        });

        let mut config = make_config(false);
        // User previously DENIED this exact command (a sensible config).
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

        // Still exactly one rule, and it's still Deny — the user's
        // explicit Deny was preserved over the AlwaysApprove's Allow.
        assert_eq!(exe.config.permission_rules.len(), 1);
        assert_eq!(
            exe.config.permission_rules[0].action,
            PermissionAction::Deny,
            "Existing Deny should not be overwritten by a new Allow on the same pattern"
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
        // A tool that returns success but causes the model to keep calling it
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

        // Every turn, the model calls the tool again (emulate via MockAdapter returning
        // the same ToolCall pattern each iteration)
        // We need a more sophisticated mock for this. Let's use a custom adapter
        // that returns a fresh tool-call stream every invocation.
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

        // Should NOT have hit the limit (max_iterations = 10, and we'd get error if we did)
        let tool_calls = *call_count.lock().unwrap();
        assert!(
            tool_calls <= 10,
            "Should not exceed max_iterations (was {})",
            tool_calls
        );
    }

    // ── Word-boundary matching tests ─────────────────────────────

    #[test]
    fn test_word_boundary_match_exact() {
        assert!(word_boundary_match("rm -rf /", "rm -rf /"));
    }

    #[test]
    fn test_word_boundary_no_false_positive_trailing_slash() {
        // "rm -rf /" should NOT match "rm -rf /home" (the / is not word-boundary-terminated)
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
        let result = check_bash_command(&args);
        assert!(result.is_some(), "rm -rf / should be blocked");
    }

    #[test]
    fn test_check_bash_command_allows_safe_similar() {
        // "rm -rf /" should NOT block "rm -rf /home/user/temp"
        let args = serde_json::json!({"command": "rm -rf /home/user/temp"});
        let result = check_bash_command(&args);
        assert!(
            result.is_none(),
            "rm -rf /home/user/temp should be allowed, got: {:?}",
            result
        );
    }

    #[test]
    fn test_check_bash_command_blocks_dd_by_substring() {
        // "dd if=/dev/zero of=" uses .contains() — should still block regardless of boundaries
        let args = serde_json::json!({"command": "dd if=/dev/zero of=/tmp/out bs=1M count=1"});
        let result = check_bash_command(&args);
        assert!(result.is_some(), "dd if=/dev/zero should be blocked");
    }

    #[test]
    fn test_check_bash_command_blocks_fork_bomb() {
        let args = serde_json::json!({"command": ":(){ :|:& };:"});
        let result = check_bash_command(&args);
        assert!(result.is_some(), "Fork bomb should be blocked");
    }

    #[test]
    fn test_check_bash_command_allows_legitimate_curl() {
        let args = serde_json::json!({"command": "curl -s https://api.example.com/data"});
        let result = check_bash_command(&args);
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
        assert!(is_read_only_bash("find . -name '*.rs'"));
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
        // Both glued |sh and spaced | sh must be caught
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
        // "scurling" contains "curl" but should NOT match READ_ONLY_COMMANDS
        assert!(!is_read_only_bash("scurling is not curl"));
        // "cat" is read-only but only if it's the first word
        assert!(is_read_only_bash("cat /etc/hostname"));
        // A path or variable containing "cat" as first word isn't a problem
        // because the first word has to EXACTLY match
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
}
