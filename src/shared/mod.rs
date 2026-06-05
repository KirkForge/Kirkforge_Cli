pub mod minify;
pub mod permission;

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// The core unit of conversation — one turn from any participant.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
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
#[serde(rename_all = "lowercase")]
pub enum Role {
    #[default]
    User,
    System,
    Assistant,
    Tool,
}

/// A tool call emitted by the model.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
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
    /// Per-tool permission rules — v1.2-p12. When non-empty, these
    /// are evaluated (first match wins) **before** the binary
    /// `auto_approve` default is applied. With empty rules, behaviour
    /// is identical to the pre-p12 flow (`auto_approve: true` → Allow,
    /// `auto_approve: false` → Ask).
    ///
    /// Wire shape: `[[permission_rules]]` in TOML, e.g.
    ///
    /// ```toml
    /// [[permission_rules]]
    /// tool = "bash"
    /// key = "command"
    /// pattern = "cargo test*"
    /// action = "allow"
    /// ```
    #[serde(default)]
    pub permission_rules: Vec<crate::shared::permission::PermissionRule>,
    pub truncation_strategy: TruncationStrategy,
    pub max_tool_result_chars: usize,

    // ── Access control (Phase 2 — deny list + path safety) ──────────
    #[serde(default)]
    pub deny_paths: Vec<String>,
    #[serde(default)]
    pub deny_urls: Vec<String>,
    #[serde(default)]
    pub deny_extensions: Vec<String>,
    #[serde(default)]
    pub allowed_write_dirs: Vec<String>,
    /// Sandbox directory — all file operations restricted to this tree.
    #[serde(default)]
    pub sandbox_dir: Option<String>,
    /// Block dotfile writes
    #[serde(default)]
    pub block_dotfiles: bool,
    /// Maximum readable file size in bytes (0 = unlimited).
    #[serde(default = "default_max_file_read_size")]
    pub max_file_read_size: usize,
    /// Whether to follow symlinks during file reads.
    #[serde(default)]
    pub follow_symlinks: bool,
    /// Whether to block reading of binary files.
    #[serde(default)]
    pub block_binary_reads: bool,

    /// Enable session carryover profile for cross-session awareness.
    /// When enabled, a tiny profile (~200 bytes) is accumulated during
    /// the session and injected into the next session's system prompt.
    #[serde(default = "default_carryover_enabled")]
    pub carryover_enabled: bool,
}

fn default_carryover_enabled() -> bool {
    true
}

fn default_max_file_read_size() -> usize {
    1024 * 1024
}

impl Default for Config {
    fn default() -> Self {
        Self {
            default_model: "deepseek-v4-flash:cloud".into(),
            ollama_host: "http://localhost:11434".into(),
            auto_approve: false,
            permission_rules: vec![],
            truncation_strategy: TruncationStrategy::KeepToolOnly,
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
            carryover_enabled: true,
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
    Success {
        content: String,
    },
    Error {
        message: String,
    },
    FileContent {
        path: PathBuf,
        content: String,
        truncated: bool,
    },
    FileEdit {
        path: PathBuf,
        diff: String,
    },
    GrepMatches {
        path: PathBuf,
        matches: Vec<Match>,
        total: usize,
    },
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

// ── Cost Tracking ────────────────────────────────────────────────

/// Per-model pricing entry (cost per million tokens).
#[derive(Debug, Clone)]
pub struct Pricing {
    /// Model name prefix for matching (longest prefix wins).
    pub model_prefix: &'static str,
    /// Cost per million input tokens (USD).
    pub input_per_mtok: f64,
    /// Cost per million output tokens (USD).
    pub output_per_mtok: f64,
    /// Cost per million cache-write tokens (USD).
    pub cache_write_per_mtok: f64,
    /// Cost per million cache-read tokens (USD).
    pub cache_read_per_mtok: f64,
}

/// Pricing table — order matters, longest prefix should come first
/// within each provider group. The final catch-all entry has empty prefix (free).
pub const PRICING_TABLE: &[Pricing] = &[
    // Anthropic (via proxy)
    Pricing {
        model_prefix: "opus-4",
        input_per_mtok: 15.00,
        output_per_mtok: 75.00,
        cache_write_per_mtok: 18.75,
        cache_read_per_mtok: 1.50,
    },
    Pricing {
        model_prefix: "sonnet-4",
        input_per_mtok: 3.00,
        output_per_mtok: 15.00,
        cache_write_per_mtok: 3.75,
        cache_read_per_mtok: 0.30,
    },
    Pricing {
        model_prefix: "haiku-4",
        input_per_mtok: 0.25,
        output_per_mtok: 1.25,
        cache_write_per_mtok: 0.30,
        cache_read_per_mtok: 0.05,
    },
    // OpenAI (via proxy)
    Pricing {
        model_prefix: "gpt-4",
        input_per_mtok: 10.00,
        output_per_mtok: 30.00,
        cache_write_per_mtok: 0.0,
        cache_read_per_mtok: 0.0,
    },
    Pricing {
        model_prefix: "gpt-5",
        input_per_mtok: 15.00,
        output_per_mtok: 60.00,
        cache_write_per_mtok: 7.50,
        cache_read_per_mtok: 0.75,
    },
    // Free catch-all for local Ollama models
    Pricing {
        model_prefix: "",
        input_per_mtok: 0.0,
        output_per_mtok: 0.0,
        cache_write_per_mtok: 0.0,
        cache_read_per_mtok: 0.0,
    },
];

/// Compute the cost of a single turn in USD.
pub fn calculate_cost(model: &str, input_tokens: usize, output_tokens: usize) -> f64 {
    let p = PRICING_TABLE
        .iter()
        .find(|p| !p.model_prefix.is_empty() && model.starts_with(p.model_prefix))
        .unwrap_or_else(|| PRICING_TABLE.last().unwrap());
    let input_cost = (input_tokens as f64 / 1_000_000.0) * p.input_per_mtok;
    let output_cost = (output_tokens as f64 / 1_000_000.0) * p.output_per_mtok;
    input_cost + output_cost
}

/// Per-session cost accumulator.
#[derive(Debug, Clone, Default)]
pub struct CostTracking {
    pub total_prompt_tokens: usize,
    pub total_completion_tokens: usize,
    pub cumulative_cost: f64,
}

impl CostTracking {
    pub fn record_turn(&mut self, prompt: usize, completion: usize, cost: f64) {
        self.total_prompt_tokens += prompt;
        self.total_completion_tokens += completion;
        self.cumulative_cost += cost;
    }
}

// ── Output Format ────────────────────────────────────────────────

/// Output format for non-interactive mode.
#[derive(Debug, Clone, Copy, PartialEq, clap::ValueEnum)]
pub enum OutputFormat {
    /// Raw text (current default)
    Text,
    /// Single JSON object with all session data
    Json,
    /// One JSON line per event (streaming)
    StreamJson,
}

/// Structured session summary for JSON output.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionSummary {
    pub version: String,
    pub session: SessionInfo,
    pub messages: Vec<Message>,
    pub tool_calls: Vec<ToolCallRecord>,
    pub usage: UsageSummary,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionInfo {
    pub id: String,
    pub model: String,
    pub duration_ms: u64,
    pub started_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsageSummary {
    pub prompt_tokens: usize,
    pub completion_tokens: usize,
    pub total_tokens: usize,
    pub cost_usd: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCallRecord {
    pub name: String,
    pub arguments: serde_json::Value,
    pub result: String,
    pub success: bool,
    pub duration_ms: u64,
}
