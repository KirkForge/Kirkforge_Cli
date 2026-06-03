use crate::adapters::ModelAdapter;
use crate::session::conversation::ConversationLog;
use crate::session::prompt::PromptBuilder;
use crate::shared::{
    Config, Message, Role, StreamEvent, ToolDef, ToolInvocation, ToolOutcome,
};
use crate::tools::Tool;
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
        Self {
            adapter,
            conversation,
            prompt_builder: PromptBuilder::new(),
            tools,
            config,
        }
    }

    /// Run a single turn: send the current conversation to the model,
    /// handle tool calls, return the assistant's response.
    ///
    /// Returns a list of events that the UI should render.
    pub async fn run_turn(
        &mut self,
        user_input: &str,
        approval_sender: mpsc::UnboundedSender<ApprovalRequest>,
        _approval_receiver: mpsc::UnboundedReceiver<ApprovalResponse>,
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
                        let msg = Message {
                            role: Role::Assistant,
                            content: assistant_content.clone(),
                            thinking: if assistant_thinking.is_empty() {
                                None
                            } else {
                                Some(assistant_thinking.clone())
                            },
                            tool_calls: if pending_tool_calls.is_empty() {
                                None
                            } else {
                                Some(std::mem::take(&mut pending_tool_calls))
                            },
                            tool_call_id: None,
                            tool_name: None,
                            token_count: usage.and_then(|u| u.completion_tokens),
                        };
                        self.conversation.append(msg)?;

                        // Process tool calls
                        for tc in &pending_tool_calls {
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
                                        true // and set auto_approve
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

                            let outcome = tool.run(tc.arguments.clone()).await;

                            match outcome {
                                ToolOutcome::Success { content }
                                | ToolOutcome::FileContent { content, .. }
                                | ToolOutcome::FileEdit { diff: content, .. } => {
                                    events.push(TurnEvent::ToolResult {
                                        name: tc.name.clone(),
                                        output: content.clone(),
                                    });
                                    self.conversation.append(Message {
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
                                    self.conversation.append(Message {
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
                                    self.conversation.append(Message {
                                        role: Role::Tool,
                                        content: format!("Error: {}", message),
                                        tool_call_id: Some(tc.id.clone()),
                                        tool_name: Some(tc.name.clone()),
                                        ..Default::default()
                                    })?;
                                }
                            }
                        }

                        // If there were tool calls, continue the loop for another model turn
                        if !pending_tool_calls.is_empty() {
                            pending_tool_calls.clear();
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

