// Public/shared API surface with fields/methods intentionally unused until later phases.
#![allow(dead_code)]

/// Send a value over a channel and log a warning if the receiver is gone.
///
/// Use this instead of `let _ = tx.send(...)` so channel drops are not silent.
/// Works with mpsc, oneshot, and any `send` call that returns a `Result`.
#[macro_export]
macro_rules! send_or_warn {
    ($expr:expr, $($fmt:tt)*) => {
        if let ::core::result::Result::Err(_) = $expr {
            ::tracing::warn!($($fmt)*);
        }
    };
}

pub mod audit;
pub mod backoff;
pub mod metrics;
pub mod minify;
pub mod permission;

#[cfg(test)]
pub mod test_util;

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock, RwLock};

pub mod config;

pub use config::{Config, DisplayConfig, ModelConfig, SecurityConfig, SessionConfig, ToolConfig};

/// Thread-safe shared configuration. Used by both the TUI and the executor
/// so that config hot-reload affects live behavior without restarting.
pub type SharedConfig = Arc<RwLock<Config>>;

/// Read a shared config, recovering from lock poisoning if necessary.
///
/// `unwrap_or_else` here is deliberate: if a writer panicked and poisoned
/// the lock, we still return the inner guard so the TUI/executor can keep
/// running with the last-known config rather than crashing.
pub fn read_shared_config(cfg: &SharedConfig) -> std::sync::RwLockReadGuard<'_, Config> {
    cfg.read().unwrap_or_else(|e| e.into_inner())
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct Message {
    #[serde(default)]
    pub role: Role,
    #[serde(default)]
    pub content: String,
    /// Multimodal content parts. When set, the adapter emits `content` as a
    /// structured array (OpenAI vision format for `OpenAiCompatAdapter`,
    /// Ollama's `images` field for the GLM/DeepSeek/Gemini/Native path).
    /// When `None`, the adapter falls through to the legacy `content: String`
    /// path — zero behaviour change for old log files.
    ///
    /// `skip_serializing_if = "Option::is_none"` keeps the on-disk NDJSON
    /// log compact: text-only messages stay `{role, content}` as before.
    /// `Default` on `Message` produces `None` here.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub content_parts: Option<Vec<ContentPart>>,
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

/// A single part of a multimodal message.
///
/// Tag-serialised as `{"type": "text", "text": "…"}` or
/// `{"type": "image", "data_base64": "…", "mime": "image/png"}` —
/// compact, human-readable, forward-compatible (new variants can be
/// added without breaking old logs because the `type` tag discriminates).
///
/// `data_base64` is the standard content transport for OpenAI vision and
/// Ollama's native `images: [string]` field. Adapters do the per-protocol
/// translation (e.g. OpenAI wraps it as
/// `{"type":"image_url","image_url":{"url":"data:<mime>;base64,<data>"}}`,
/// Ollama just emits the base64 string).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentPart {
    Text { text: String },
    Image { data_base64: String, mime: String },
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolInvocation {
    pub id: String,
    pub name: String,
    pub arguments: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResult {
    pub id: String,
    pub content: String,
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub truncated: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum FinishReason {
    Stop,
    Length,
    ToolCalls,
    Error,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TokenUsage {
    pub prompt_tokens: Option<usize>,
    pub completion_tokens: Option<usize>,
    /// Tokens served from the provider's prompt cache (e.g. Anthropic's
    /// `cache_read_input_tokens` or OpenAI's
    /// `prompt_tokens_details.cached_tokens`). The cost-tracker applies the
    /// discounted read-rate to this portion. Absent = unknown / not
    /// reported by the server.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub cached_tokens: Option<usize>,
}

#[derive(Debug, Clone)]
pub struct ModelInfo {
    pub name: String,
    pub supports_thinking: bool,
    pub tool_call_format: ToolCallStyle,
    pub max_context_tokens: usize,
    pub recommended_temperature: f64,
    /// Whether the model accepts image inputs (OpenAI vision, Anthropic
    /// `claude-3-*`, etc.). Drives the runtime registration of the
    /// `read_image` tool: a non-vision model never sees that tool in its
    /// available-tool list, and a tool-call attempt is a clear
    /// "model not supported" error rather than a silent failure at the
    /// adapter. Default `false`; the `OpenAiCompatAdapter` factory
    /// sets it to `true` only for known vision model names.
    pub supports_images: bool,
    /// Whether the model / server supports prompt caching breakpoints
    /// (Anthropic's `cache_control: {type: "ephemeral"}` or the OpenAI
    /// equivalent). When `true`, the OpenAI-compat body builder marks
    /// the last 2 messages of the prefix with `cache_control` so the
    /// server can reuse its prompt KV-cache. Ollama-native and the
    /// GLM/DeepSeek/Gemini adapters ignore this flag — they have no
    /// equivalent field.
    pub supports_cache: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ToolCallStyle {
    Native,
    OpenAiCompat,
    /// Anthropic Messages API native `tool_use` / `tool_result` blocks.
    Anthropic,
    None,
}

/// Headless Chrome configuration for the `computer_use` tool.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ComputerUseConfig {
    /// Enable the `computer_use` tool. Default false.
    #[serde(default)]
    pub enabled: bool,

    /// Explicit path to the Chrome / Chromium binary. When empty the
    /// tool uses headless_chrome's automatic lookup.
    #[serde(default)]
    pub chrome_path: Option<PathBuf>,

    /// When true, launch Chrome in a visible window instead of headless.
    /// Useful for local debugging; default false.
    #[serde(default)]
    pub headful: bool,

    /// Default viewport width. Default 1280.
    #[serde(default = "default_computer_use_width")]
    pub width: u32,

    /// Default viewport height. Default 800.
    #[serde(default = "default_computer_use_height")]
    pub height: u32,

    /// Seconds to wait for Chrome startup before failing. Default 30.
    #[serde(default = "default_computer_use_startup_timeout")]
    pub startup_timeout_secs: u64,

    /// Seconds to wait for page navigation / element selectors.
    /// Default 10.
    #[serde(default = "default_computer_use_wait_timeout")]
    pub wait_timeout_secs: u64,

    /// Maximum number of steps in a browser session before it is
    /// forcibly closed. Prevents infinite loops. Default 20.
    #[serde(default = "default_computer_use_max_steps")]
    pub max_steps: u32,
}

/// Docker execution configuration for the bash tool.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct DockerConfig {
    /// Enable Docker execution for bash commands. Default false.
    #[serde(default)]
    pub enabled: bool,

    /// Docker image to use for command execution.
    #[serde(default = "default_docker_image")]
    pub image: String,

    /// Memory limit for the container (e.g. "2g"). Default "2g".
    #[serde(default = "default_docker_memory")]
    pub memory: String,

    /// CPU limit for the container. Default "2".
    #[serde(default = "default_docker_cpus")]
    pub cpus: String,
}

fn default_docker_image() -> String {
    "ubuntu:24.04".into()
}
fn default_docker_memory() -> String {
    "2g".into()
}
fn default_docker_cpus() -> String {
    "2".into()
}

/// Configuration for a single MCP server connection.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServerConfig {
    /// Human-readable name for this server (used in tool prefix).
    pub name: String,
    /// Transport kind. `stdio` spawns `command` with `args`. `http` connects
    /// to `url` via streamable-HTTP (GET for SSE, POST for messages).
    /// Default is `stdio` for backward compatibility.
    #[serde(default = "default_mcp_transport")]
    pub transport: String,
    /// Command to spawn (e.g., "npx", "python3"). Used only for stdio.
    #[serde(default)]
    pub command: String,
    /// Arguments passed to the command.
    #[serde(default)]
    pub args: Vec<String>,
    /// Additional environment variables for the subprocess.
    #[serde(default)]
    pub env_vars: HashMap<String, String>,
    /// Base URL for streamable-HTTP transport (e.g. `http://localhost:8080/mcp`).
    /// Used only for `transport = "http"`.
    #[serde(default)]
    pub url: String,
    /// Optional bearer token for HTTP transport. If present, sent as
    /// `Authorization: Bearer <token>`.
    #[serde(default)]
    pub bearer_token: String,
}

fn default_mcp_transport() -> String {
    "stdio".to_string()
}

/// Configuration for a single LSP server entry. Mirrors `[[mcp_servers]]`
/// but for language servers — each entry launches a subprocess speaking
/// LSP over stdio and serves files with the listed extensions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LspServerEntry {
    /// Language name (e.g. "rust", "typescript", "python"). Used as the
    /// key in the LSP pool and in `lsp_query` error messages.
    pub language: String,
    /// File extensions this server handles (e.g. [".rs"]). Extensions are
    /// matched case-insensitively and may be given with or without the
    /// leading dot.
    pub extensions: Vec<String>,
    /// Command to spawn (e.g. "rust-analyzer", "typescript-language-server").
    pub command: String,
    /// Arguments passed to the command (e.g. ["--stdio"]).
    #[serde(default)]
    pub args: Vec<String>,
    /// Additional environment variables for the subprocess.
    #[serde(default)]
    pub env_vars: HashMap<String, String>,
}

fn default_computer_use_width() -> u32 {
    1280
}

fn default_computer_use_height() -> u32 {
    800
}

fn default_computer_use_startup_timeout() -> u64 {
    30
}

fn default_computer_use_wait_timeout() -> u64 {
    10
}

fn default_computer_use_max_steps() -> u32 {
    20
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub enum TruncationStrategy {
    DropOldest,
    #[default]
    KeepToolOnly,
    SummarizeMiddle,
}

#[derive(Debug, Clone)]
pub struct ToolDef {
    pub name: &'static str,
    pub description: &'static str,
    pub parameters: serde_json::Value,
}

// ponytail: deduplicating string interner for runtime-discovered tool metadata.
// `ToolDef` requires `&'static str`, so tool names/descriptions read from MCP
// servers or plugin manifests at runtime must be made 'static. The previous code
// `Box::leak`ed a fresh allocation on every construction — and `/reload plugins`
// (executor::reload_plugins) rebuilds every plugin wrapper each invocation, so
// repeated reloads leaked unboundedly. Interning leaks at most once per unique
// string and reuses it on every reload, bounding growth to the set of distinct
// tool names ever seen (stable across reloads in practice).
//
// Ceiling: one `Box<str>` per unique name is held for the process lifetime (never
// freed even if the tool is removed). Upgrade path: change `ToolDef` to own
// `Arc<str>` so dropped ToolDefs free their strings — but `Arc<str> == &str` is
// not in std, so that is a ~90-site change (every `def().name == "x"` comparison,
// `assert_eq!`, `matches!`, and `Vec<&str>` collect) and is not justified by the
// reload-growth defect this interner already fixes.
static INTERNED_STRS: OnceLock<Mutex<HashMap<String, &'static str>>> = OnceLock::new();

/// Intern a runtime string as `&'static str`, leaking at most one allocation per
/// distinct value. Use for `ToolDef` name/description built from dynamic sources
/// (MCP tool names, plugin manifests) so repeated rebuilds do not accumulate.
pub fn intern_static_str(s: &str) -> &'static str {
    let map = INTERNED_STRS.get_or_init(|| Mutex::new(HashMap::new()));
    let mut guard = map.lock().expect("static-str interner mutex poisoned");
    if let Some(existing) = guard.get(s).copied() {
        return existing;
    }
    let leaked: &'static str = Box::leak(s.to_string().into_boxed_str());
    guard.insert(s.to_string(), leaked);
    leaked
}

#[derive(Debug, Clone)]
pub enum ToolOutcome {
    Success {
        content: String,
    },
    Error {
        message: String,
    },
    Failure(ToolError),
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
    /// Multimodal result from the `read_image` tool. Carries the raw
    /// base64 bytes + mime type. `handle_tool_outcome` translates this
    /// into a `Message { Role::Tool, content_parts: [Image{…}] }` so the
    /// next user turn can splice the image onto the user message and the
    /// model sees it as part of the user's question.
    Image {
        path: PathBuf,
        mime: String,
        data_base64: String,
    },
}

impl ToolOutcome {
    /// Convenience constructor for the legacy unstructured error path.
    /// Prefer `ToolOutcome::Failure(ToolError::...)` when the error kind
    /// is known.
    pub fn error(message: impl Into<String>) -> Self {
        Self::Error {
            message: message.into(),
        }
    }
}

/// Structured tool failure. Carries enough detail for the executor/TUI
/// to decide how to present the result and whether the failure is
/// retryable.
#[derive(Debug, Clone)]
pub enum ToolError {
    /// Tool arguments were missing or malformed.
    InvalidArgs { message: String },
    /// The operation was denied by the permission/path guard.
    AccessDenied { message: String },
    /// The tool ran but exited non-zero. Includes the exit code and any
    /// captured stderr.
    Execution {
        message: String,
        exit_code: Option<i32>,
        stderr: String,
    },
    /// The tool did not complete before its deadline.
    Timeout { after_secs: u64 },
    /// The caller cancelled the tool mid-flight.
    Cancelled,
    /// Catch-all for unexpected tool-internal errors.
    Internal { message: String },
}

impl ToolError {
    /// Human-readable single-line summary. This is what the model sees in
    /// the conversation log and what line-mode prints.
    pub fn to_user_message(&self) -> String {
        match self {
            Self::InvalidArgs { message } => format!("Invalid tool arguments: {message}"),
            Self::AccessDenied { message } => format!("Access denied: {message}"),
            Self::Execution {
                message,
                exit_code,
                stderr,
            } => {
                let code = exit_code
                    .map(|c| format!("exit code {c}"))
                    .unwrap_or_else(|| "no exit code".to_string());
                if stderr.is_empty() {
                    format!("{message} ({code})")
                } else {
                    format!("{message} ({code})\nstderr:\n{stderr}")
                }
            }
            Self::Timeout { after_secs } => format!("Tool timed out after {after_secs}s"),
            Self::Cancelled => "Tool cancelled by user".to_string(),
            Self::Internal { message } => format!("Internal tool error: {message}"),
        }
    }

    /// Convenience for the legacy `ToolOutcome::Error` constructor.
    pub fn invalid_args(message: impl Into<String>) -> Self {
        Self::InvalidArgs {
            message: message.into(),
        }
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self::Internal {
            message: message.into(),
        }
    }
}

#[cfg(test)]
mod tool_error_tests {
    use super::ToolError;

    #[test]
    fn invalid_args_message() {
        let err = ToolError::invalid_args("missing 'path'");
        assert_eq!(
            err.to_user_message(),
            "Invalid tool arguments: missing 'path'"
        );
    }

    #[test]
    fn execution_message_includes_exit_code_and_stderr() {
        let err = ToolError::Execution {
            message: "Command failed".into(),
            exit_code: Some(42),
            stderr: "oh no".into(),
        };
        assert!(err.to_user_message().contains("exit code 42"));
        assert!(err.to_user_message().contains("oh no"));
    }

    #[test]
    fn execution_message_without_stderr_omits_stderr_block() {
        let err = ToolError::Execution {
            message: "Command failed".into(),
            exit_code: Some(1),
            stderr: String::new(),
        };
        assert_eq!(err.to_user_message(), "Command failed (exit code 1)");
    }

    #[test]
    fn timeout_message() {
        let err = ToolError::Timeout { after_secs: 7 };
        assert_eq!(err.to_user_message(), "Tool timed out after 7s");
    }

    #[test]
    fn cancelled_message() {
        assert_eq!(
            ToolError::Cancelled.to_user_message(),
            "Tool cancelled by user"
        );
    }
}

#[derive(Debug, Clone)]
pub struct Match {
    pub line_number: usize,
    pub line: String,
    pub context_before: Vec<String>,
    pub context_after: Vec<String>,
}

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

#[derive(Debug, Clone)]
pub struct Pricing {
    pub model_prefix: &'static str,

    pub input_per_mtok: f64,

    pub output_per_mtok: f64,

    pub cache_write_per_mtok: f64,

    pub cache_read_per_mtok: f64,
}

const PRICING_FALLBACK: Pricing = Pricing {
    model_prefix: "",
    input_per_mtok: 0.0,
    output_per_mtok: 0.0,
    cache_write_per_mtok: 0.0,
    cache_read_per_mtok: 0.0,
};

pub const PRICING_TABLE: &[Pricing] = &[
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
    Pricing {
        model_prefix: "",
        input_per_mtok: 0.0,
        output_per_mtok: 0.0,
        cache_write_per_mtok: 0.0,
        cache_read_per_mtok: 0.0,
    },
];

pub fn calculate_cost(model: &str, usage: &TokenUsage) -> f64 {
    let prompt = usage.prompt_tokens.unwrap_or(0);
    let completion = usage.completion_tokens.unwrap_or(0);
    let cached = usage.cached_tokens.unwrap_or(0).min(prompt); // never let cached exceed the prompt itself

    let p = PRICING_TABLE
        .iter()
        .find(|p| !p.model_prefix.is_empty() && model.starts_with(p.model_prefix))
        .unwrap_or_else(|| PRICING_TABLE.last().unwrap_or(&PRICING_FALLBACK));

    // Cached tokens are billed at the discounted read rate; the rest of
    // the prompt at the regular input rate. Servers that don't
    // distinguish (most OpenAI-compat) return `cached_tokens = None`,
    // and the discount path is a no-op. `cache_read_per_mtok` is
    // `0.0` for non-cached pricing rows (e.g. `gpt-4` in the table),
    // so a stale or wrong `cached_tokens` value still produces a
    // reasonable upper-bound cost.
    let cached_cost = (cached as f64 / 1_000_000.0) * p.cache_read_per_mtok;
    let fresh_input_cost = ((prompt - cached) as f64 / 1_000_000.0) * p.input_per_mtok;
    let output_cost = (completion as f64 / 1_000_000.0) * p.output_per_mtok;
    cached_cost + fresh_input_cost + output_cost
}

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

pub use crate::cli::OutputFormat;

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
