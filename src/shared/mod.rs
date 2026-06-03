pub mod minify;

use std::path::PathBuf;
use serde::{Deserialize, Serialize};

/// The core unit of conversation — one turn from any participant.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Message {
    #[serde(default)]
    pub role: Role,
    #[serde(default)]
    pub content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolInvocation>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token_count: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub enum Role {
    #[default]
    User,
    System,
    Assistant,
    Tool,
}

/// A tool call emitted by the model.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolInvocation {
    pub id: String,
    pub name: String,
    pub arguments: serde_json::Value,
}

/// The result of executing a tool.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResult {
    pub id: String,
    pub content: String,
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub truncated: Option<bool>,
}

/// Events flowing from the model adapter to the session layer.
#[derive(Debug, Clone)]
pub enum StreamEvent {
    Text(String),
    Thinking(String),
    ToolCall(ToolInvocation),
    Error(String),
    Done {
        finish_reason: FinishReason,
        usage: Option<TokenUsage>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum FinishReason {
    Stop,
    Length,
    ToolCalls,
    Error,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenUsage {
    pub prompt_tokens: Option<usize>,
    pub completion_tokens: Option<usize>,
}

/// Static information about a model, used by the session and UI.
#[derive(Debug, Clone)]
pub struct ModelInfo {
    pub name: String,
    pub supports_thinking: bool,
    pub tool_call_format: ToolCallStyle,
    pub max_context_tokens: usize,
    pub recommended_temperature: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ToolCallStyle {
    Native,
    OpenAiCompat,
    None,
}

/// Configuration loaded from ~/.local/share/kirkforge/config.toml
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub default_model: String,
    pub ollama_host: String,
    pub auto_approve: bool,
    pub truncation_strategy: TruncationStrategy,
    pub max_tool_result_chars: usize,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            default_model: "glm-5.1:cloud".into(),
            ollama_host: "http://localhost:11434".into(),
            auto_approve: false,
            truncation_strategy: TruncationStrategy::KeepToolOnly,
            max_tool_result_chars: 4000,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum TruncationStrategy {
    DropOldest,
    KeepToolOnly,
    SummarizeMiddle,
}

/// User approval decision for a destructive tool call.
#[derive(Debug, Clone, PartialEq)]
pub enum Approval {
    Approved,
    Denied,
    AlwaysApprove,
}

/// A tool definition as exposed to the model.
#[derive(Debug, Clone)]
pub struct ToolDef {
    pub name: &'static str,
    pub description: &'static str,
    pub parameters: serde_json::Value,
}

/// Result types for tools.
#[derive(Debug, Clone)]
pub enum ToolOutcome {
    Success { content: String },
    Error { message: String },
    FileContent { path: PathBuf, content: String, truncated: bool },
    FileEdit { path: PathBuf, diff: String },
    GrepMatches { path: PathBuf, matches: Vec<Match>, total: usize },
}

#[derive(Debug, Clone)]
pub struct Match {
    pub line_number: usize,
    pub line: String,
    pub context_before: Vec<String>,
    pub context_after: Vec<String>,
}

/// Session identity for log files.
#[derive(Debug, Clone)]
pub struct SessionId {
    pub date: String,
    pub seq: u32,
}

impl std::fmt::Display for SessionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}-session-{:02}", self.date, self.seq)
    }
}