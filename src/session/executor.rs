use crate::adapters::ModelAdapter;
use crate::session::conversation::ConversationLog;
use crate::session::event_bus::{BusEvent, EventBus};
use crate::session::prompt::PromptBuilder;
use crate::session::verifier::CorrectionResult;
use crate::session::verifier::{CorrectionLoop, VerifierHandler, VerifierSlots};
use crate::shared::{
    Config, Message, Role, StreamEvent, ToolDef, ToolInvocation, ToolOutcome,
};
use crate::tools::Tool;
use crate::session::access::{access_from_config, DenyList, GuardVerdict, PathGuard, ReadGate};
use std::sync::Arc;
use tokio::sync::mpsc;

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
}

impl Executor {
    pub fn new(
        adapter: Box<dyn ModelAdapter>,
        tools: Vec<Arc<dyn Tool>>,
        config: Config,
    ) -> Self {
        // Create a default temp conversation log
        let temp_dir = std::env::temp_dir().join("kirkforge-session");
        let log_path = temp_dir.join(format!("session-{}.ndjson", chrono::Local::now().format("%Y%m%d-%H%M%S")));
        let conversation = ConversationLog::open(log_path).unwrap();
        Self::with_log(adapter, tools, config, conversation)
    }

    pub fn with_log(
        adapter: Box<dyn ModelAdapter>,
        tools: Vec<Arc<dyn Tool>>,
        config: Config,
        conversation: ConversationLog,
    ) -> Self {
        let model_name = adapter.model_info().name.clone();
        let (deny_list, path_guard, read_gate) = access_from_config(&config);

        // Event bus — verifiers are registered by init_default_verifiers() called below
        let event_bus = EventBus::new();

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
            fn name(&self) -> &str { "security" }
            fn priority(&self) -> u8 { 1 }
            async fn verify(&self, event: &BusEvent) -> Verdict {
                crate::session::verifier::security::verify_security(event).await
            }
        }
        {
            let mut s = slots.write().unwrap();
            if s.register(Arc::new(SecV)).is_ok() { count += 1; }
        }

        // Lint verifier (priority 2)
        struct LintV;
        #[async_trait::async_trait]
        impl Verifier for LintV {
            fn name(&self) -> &str { "lint" }
            fn priority(&self) -> u8 { 2 }
            async fn verify(&self, event: &BusEvent) -> Verdict {
                crate::session::verifier::lint::verify_lint(event).await
            }
        }
        {
            let mut s = slots.write().unwrap();
            if s.register(Arc::new(LintV)).is_ok() { count += 1; }
        }

        // Git verifier (priority 3)
        struct GitV;
        #[async_trait::async_trait]
        impl Verifier for GitV {
            fn name(&self) -> &str { "git" }
            fn priority(&self) -> u8 { 3 }
            async fn verify(&self, event: &BusEvent) -> Verdict {
                crate::session::verifier::git::verify_git(event).await
            }
        }
        {
            let mut s = slots.write().unwrap();
            if s.register(Arc::new(GitV)).is_ok() { count += 1; }
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
    pub async fn run(
        &mut self,
        mut input_rx: mpsc::UnboundedReceiver<String>,
        event_tx: mpsc::UnboundedSender<TurnEvent>,
        approval_tx: mpsc::UnboundedSender<ApprovalRequest>,
    ) -> anyhow::Result<()> {
        while let Some(input) = input_rx.recv().await {
            let events = self.run_turn(&input, &approval_tx).await?;
            for ev in events {
                if event_tx.send(ev).is_err() {
                    // TUI closed — stop the executor
                    return Ok(());
                }
            }
        }
        Ok(())
    }

    /// Run a single turn: send the current conversation to the model,
    /// handle tool calls, return the assistant's response.
    pub async fn run_turn(
        &mut self,
        user_input: &str,
        approval_sender: &mpsc::UnboundedSender<ApprovalRequest>,
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

        let mut events = Vec::new();
        let model_info = self.adapter.model_info();
        let tool_defs: Vec<ToolDef> = self.tools.iter().map(|t| t.def()).collect();
        let tool_names: Vec<&str> = tool_defs.iter().map(|t| t.name).collect();

        // Main loop: model responds, may call tools, we feed results back
        let max_iterations = 10; // safety cap on tool call loops
        for iteration in 0..max_iterations {
            let system = self.prompt_builder.build(
                &model_info.name,
                model_info.supports_thinking,
                &tool_names,
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
            let mut pending_tool_calls: Vec<ToolInvocation> = Vec::new();

            // Stream events from the adapter
            while let Some(event) = rx.recv().await {
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
                        pending_tool_calls.push(tc);
                    }
                    StreamEvent::Error(e) => {
                        events.push(TurnEvent::Error(e));
                    }
                    StreamEvent::Done { finish_reason: _, usage } => {
                        // Save the assistant message
                        let has_tool_calls = !pending_tool_calls.is_empty();
                        let msg = Message {
                            role: Role::Assistant,
                            content: assistant_content.clone(),
                            thinking: if assistant_thinking.is_empty() {
                                None
                            } else {
                                Some(assistant_thinking.clone())
                            },
                            tool_calls: if has_tool_calls {
                                Some(pending_tool_calls.clone())
                            } else {
                                None
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
                            let cost = crate::shared::calculate_cost(&self.model_name, prompt, completion);
                            self.cost_tracking.record_turn(prompt, completion, cost);
                            events.push(TurnEvent::CostStats {
                                prompt_tokens: prompt,
                                completion_tokens: completion,
                                turn_cost: cost,
                                cumulative_cost: self.cost_tracking.cumulative_cost,
                            });
                        }

                        // Process tool calls (clone because pending_tool_calls is
                        // reused in the outer loop if we continue for another turn)
                        let tool_calls = std::mem::take(&mut pending_tool_calls);
                        for tc in &tool_calls {
                            // Find the tool
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
                                    continue;
                                }
                            };

                            // Check if destructive and needs approval
                            let is_destructive = matches!(tc.name.as_str(), "write_file" | "edit_file" | "bash");
                            let needs_approval = is_destructive && !self.config.auto_approve;

                            if needs_approval {
                                // Request approval from the UI
                                let (response_tx, response_rx) = tokio::sync::oneshot::channel::<ApprovalResponse>();
                                approval_sender.send(ApprovalRequest {
                                    tool_name: tc.name.clone(),
                                    args: tc.arguments.clone(),
                                    response: response_tx,
                                })?;

                                // Wait for user decision
                                let approved = match response_rx.await {
                                    Ok(ApprovalResponse::Approved) => true,
                                    Ok(ApprovalResponse::Denied) => false,
                                    Ok(ApprovalResponse::AlwaysApprove) => {
                                        self.config.auto_approve = true;
                                        true
                                    }
                                    Err(_) => false,
                                };

                                if !approved {
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
                                    continue;
                                }
                            }

                            // Execute the tool
                            events.push(TurnEvent::ToolStart {
                                name: tc.name.clone(),
                                args: tc.arguments.clone(),
                            });

                            // ── Access control checks ──────────────
                            // 1. Deny list check (all tools)
                            let tool_name = tc.name.as_str();
                            if let Some(denied) = check_deny_list(&self.deny_list, tool_name, &tc.arguments) {
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
                                continue;
                            }

                            // 2. Path guard check for file tools
                            if matches!(tool_name, "read_file" | "write_file" | "edit_file") {
                                let path_str = tc.arguments.get("path")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("");
                                let path = std::path::Path::new(path_str);

                                let verdict = match tool_name {
                                    "read_file" => self.path_guard.check_read(path),
                                    "write_file" | "edit_file" => self.path_guard.check_write(path),
                                    _ => unreachable!(),
                                };

                                match verdict {
                                    GuardVerdict::Allowed(resolved) => {
                                        // 3. Read-before-edit gate
                                        if tool_name == "edit_file" {
                                            match self.read_gate.check_edit(path) {
                                                GuardVerdict::Allowed(_) => {}
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
                                                    continue;
                                                }
                                            }
                                        }

                                        // 4. Mark as read if read_file
                                        if tool_name == "read_file" {
                                            self.read_gate.mark_read(&resolved);
                                        }

                                        // Stash resolved path back for the tool
                                        if let Ok(path_obj) = serde_json::to_value(resolved.to_string_lossy().as_ref()) {
                                            // Use the resolved path
                                            let mut updated_args = tc.arguments.clone();
                                            if let Some(obj) = updated_args.as_object_mut() {
                                                obj.insert("path".into(), path_obj);
                                            }
                                            let crs_args = updated_args.clone();
                                            let outcome = tool.run(updated_args).await;
                                            handle_tool_outcome(outcome, tc, &mut events, &mut self.conversation)?;
                                            let crs = self.emit_tool_event_and_correct(tc, tool_name, &crs_args).await;
                                            emit_correction_results(crs, tc, &mut events, &mut self.conversation)?;
                                        } else {
                                            let args = tc.arguments.clone();
                                            let outcome = tool.run(tc.arguments.clone()).await;
                                            handle_tool_outcome(outcome, tc, &mut events, &mut self.conversation)?;
                                            let crs = self.emit_tool_event_and_correct(tc, tool_name, &args).await;
                                            emit_correction_results(crs, tc, &mut events, &mut self.conversation)?;
                                        }
                                        continue;
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
                                        continue;
                                    }
                                }
                            }

                            // Non-file tools: bash, grep, glob — execute directly
                            let outcome = tool.run(tc.arguments.clone()).await;
                            handle_tool_outcome(outcome, tc, &mut events, &mut self.conversation)?;

                            // Emit event + run correction loop
                            let crs = self.emit_tool_event_and_correct(tc, tool_name, &tc.arguments).await;
                            emit_correction_results(crs, tc, &mut events, &mut self.conversation)?;
                        }

                        // If there were tool calls, continue the loop for another model turn
                        if !tool_calls.is_empty() {
                            break; // break the event loop, continue the outer turn loop
                        }

                        // No tool calls — we're done
                        return Ok(events);
                    }
                }
            }

            // If we processed tool calls, continue to let the model respond
            if iteration + 1 >= max_iterations {
                events.push(TurnEvent::Error("Tool call loop limit reached".into()));
            }
        }

        Ok(events)
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
    ) -> Vec<CorrectionResult> {
        let bus_event = match tool_name {
            "read_file" => {
                let path = args.get("path").and_then(|v| v.as_str()).unwrap_or("").to_string();
                Some(BusEvent::FileRead(crate::session::event_bus::FileReadEvent {
                    path: std::path::PathBuf::from(&path),
                    size_bytes: 0,
                    truncated: false,
                }))
            }
            "write_file" => {
                let path = args.get("path").and_then(|v| v.as_str()).unwrap_or("").to_string();
                let content = args.get("content").and_then(|v| v.as_str()).unwrap_or("");
                Some(BusEvent::FileWrite(crate::session::event_bus::FileWriteEvent {
                    path: std::path::PathBuf::from(&path),
                    content_length: content.len(),
                }))
            }
            "edit_file" => {
                let path = args.get("path").and_then(|v| v.as_str()).unwrap_or("").to_string();
                let diff = args.get("old_string")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                Some(BusEvent::Edit(crate::session::event_bus::EditEvent {
                    path: std::path::PathBuf::from(&path),
                    diff,
                }))
            }
            "bash" => {
                let command = args.get("command").and_then(|v| v.as_str()).unwrap_or("").to_string();
                Some(BusEvent::BashExec(crate::session::event_bus::BashExecEvent {
                    command,
                    exit_code: 0,
                    stdout_len: 0,
                    stderr_len: 0,
                }))
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
}

/// Helper: check deny list before running a tool.
/// Returns Some(denial_message) if blocked, None if allowed.
fn check_deny_list(deny_list: &DenyList, tool_name: &str, args: &serde_json::Value) -> Option<String> {
    match tool_name {
        "read_file" | "write_file" | "edit_file" => {
            if let Some(path) = args.get("path").and_then(|v| v.as_str()) {
                let p = std::path::Path::new(path);
                if deny_list.is_path_denied(p) {
                    return Some(format!(
                        "🔒 Path denied by deny list: {}",
                        path
                    ));
                }
            }
        }
        "bash" => {
            if let Some(cmd) = args.get("command").and_then(|v| v.as_str()) {
                // Check for dangerous patterns
                if cmd.contains("169.254.169.254") || cmd.contains("metadata.google") {
                    return Some("🔒 Command blocked: contains reference to metadata endpoints".into());
                }
            }
        }
        "grep" | "glob" => {
            if let Some(path) = args.get("path").and_then(|v| v.as_str()) {
                let p = std::path::Path::new(path);
                if deny_list.is_path_denied(p) {
                    return Some(format!(
                        "🔒 Path denied by deny list: {}",
                        path
                    ));
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
    use crate::shared::{ModelInfo, ToolCallStyle, ToolDef, StreamEvent, ToolOutcome, Message, Role, TokenUsage, FinishReason, ToolInvocation};
    use std::sync::{Arc, Mutex};
    use tokio::sync::mpsc;

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
        }
    }

    fn make_executor(adapter: Box<dyn ModelAdapter>, tools: Vec<Arc<dyn Tool>>, config: Config) -> Executor {
        let temp_dir = std::env::temp_dir();
        let log_path = temp_dir.join(format!("kirkforge-test-{}.ndjson", std::process::id()));
        let _ = std::fs::remove_file(&log_path);
        let conversation = ConversationLog::open(log_path).unwrap();
        Executor::with_log(adapter, tools, config, conversation)
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
        let events = exe.run_turn("hello", &approval_tx).await.unwrap();

        let tokens: Vec<_> = events.iter().filter_map(|e| match e {
            TurnEvent::Token(t) => Some(t.as_str()),
            _ => None,
        }).collect();
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
        let events = exe.run_turn("use echo", &approval_tx).await.unwrap();

        // Should have token + tool_start + tool_result
        let has_token = events.iter().any(|e| matches!(e, TurnEvent::Token(_)));
        let has_start = events.iter().any(|e| matches!(e, TurnEvent::ToolStart { name, .. } if name == "echo"));
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
                    arguments: serde_json::json!({"command": "rm -rf /"}),
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
        let events = exe.run_turn("run command", &approval_tx).await.unwrap();

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
        let events = exe.run_turn("run command", &approval_tx).await.unwrap();

        approval_handle.await.unwrap();

        // Tool should NOT have been called (denied)
        assert!(captured.lock().unwrap().is_none(), "Tool should not have been called when denied");

        // Should have a denied-message result
        let denied = events.iter().any(|e| matches!(e, TurnEvent::ToolResult { name, output } if name == "bash" && output.contains("denied")));
        assert!(denied, "Should report that operation was denied");
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
        let events = exe.run_turn("do it", &approval_tx).await.unwrap();

        let has_error = events.iter().any(|e| matches!(e, TurnEvent::Error(msg) if msg == "connection lost"));
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
        let events = exe.run_turn("use unknown tool", &approval_tx).await.unwrap();

        let has_error = events.iter().any(|e| matches!(e, TurnEvent::Error(msg) if msg.contains("Unknown tool")));
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
                    let _ = tx.send(StreamEvent::ToolCall(ToolInvocation {
                        id: format!("call-{}", count),
                        name: "looper".into(),
                        arguments: serde_json::json!({"x": format!("round-{}", count)}),
                    })).await;
                    let _ = tx.send(StreamEvent::Done {
                        finish_reason: FinishReason::ToolCalls,
                        usage: None,
                    }).await;
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
        let _events = exe.run_turn("loop", &approval_tx).await.unwrap();

        // Should NOT have hit the limit (max_iterations = 10, and we'd get error if we did)
        let tool_calls = *call_count.lock().unwrap();
        assert!(tool_calls <= 10, "Should not exceed max_iterations (was {})", tool_calls);
    }
}

