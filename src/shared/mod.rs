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

pub mod metrics;
pub mod minify;
pub mod permission;

#[cfg(test)]
pub mod test_util;

use kirkforge_plugin::TrustTier;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};

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
    None,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub default_model: String,
    pub ollama_host: String,
    pub auto_approve: bool,

    #[serde(default)]
    pub permission_rules: Vec<crate::shared::permission::PermissionRule>,
    pub truncation_strategy: TruncationStrategy,
    pub max_tool_result_chars: usize,

    #[serde(default)]
    pub deny_paths: Vec<String>,
    #[serde(default)]
    pub deny_urls: Vec<String>,
    #[serde(default)]
    pub deny_extensions: Vec<String>,
    #[serde(default)]
    pub allowed_write_dirs: Vec<String>,

    #[serde(default)]
    pub sandbox_dir: Option<String>,

    #[serde(default)]
    pub block_dotfiles: bool,

    #[serde(default = "default_max_file_read_size")]
    pub max_file_read_size: usize,

    /// Maximum size (in bytes) of an existing file that `edit_file` or
    /// `write_file` may overwrite. Files larger than this are blocked
    /// to prevent the model from silently clobbering large assets.
    /// Default 1 MiB. Set to 0 to disable the limit.
    #[serde(default = "default_max_overwrite_size")]
    pub max_overwrite_size: usize,

    #[serde(default)]
    pub follow_symlinks: bool,

    #[serde(default)]
    pub block_binary_reads: bool,

    /// When `true` (the default), the bash tool runs with its working
    /// directory forced inside `sandbox_dir`. An explicit `workdir` arg
    /// that points outside the sandbox is rejected; a missing workdir
    /// defaults to the sandbox. Set to `false` to allow the bash
    /// subprocess to run anywhere on the filesystem (the old
    /// behavior). This is the bash-policy half of GPT 5.5's review
    /// finding #4.
    #[serde(default = "default_bash_sandbox_workdir")]
    pub bash_sandbox_workdir: bool,

    #[serde(default = "default_carryover_enabled")]
    pub carryover_enabled: bool,

    /// Model to use for semantic context summarization (fast/cheap).
    /// When set, `/compact` will use this model to summarise old turns
    /// instead of naive truncation. Defaults to "qwen2.5:3b".
    #[serde(default = "default_summarize_model")]
    pub summarize_model: String,

    /// Enable LLM-based context summarisation on `/compact`.
    /// Disabled by default — falls back to truncation if summarization fails.
    #[serde(default)]
    pub summarize_enabled: bool,

    /// Enable smart model routing (task complexity classification).
    /// Disabled by default. When enabled, the user's first message each turn
    /// is classified as simple/medium/complex. Currently advisory — does not
    /// hot-swap adapters mid-session.
    #[serde(default)]
    pub routing_enabled: bool,

    /// Model for routing classification (fast/cheap). When empty,
    /// classification uses local keyword heuristics.
    #[serde(default)]
    pub router_model: String,

    /// Optional per-tier model overrides for smart routing.
    /// Keys are "simple", "medium", "complex"; values are model names.
    /// When a tier has no entry, built-in defaults are used
    /// (qwen2.5:3b / deepseek-v4-flash:cloud / deepseek-v4-pro:cloud).
    #[serde(default)]
    pub routing_model_map: HashMap<String, String>,

    /// Maximum file size (in bytes) allowed in a commit produced by the
    /// `/commit` command. Files larger than this are a hard blocker. The
    /// default is 5 MiB — enough for small assets but small enough to catch
    /// accidentally committed binaries and dumps.
    #[serde(default = "default_commit_max_file_size")]
    pub commit_max_file_size: u64,

    /// MCP (Model Context Protocol) servers to connect to at session start.
    /// Each server is spawned as a subprocess and its tools are made
    /// available alongside built-in tools (prefixed with `mcp/<server>/`).
    #[serde(default)]
    pub mcp_servers: Vec<McpServerConfig>,

    /// When `true`, the `!` bash passthrough routes through the same
    /// approval gate that the bash tool uses. The escape-hatch UX is
    /// preserved (still no model round trip), but the user must
    /// confirm the command. Default `false` — the passthrough is
    /// deliberately friction-free for fast feedback loops. Set to
    /// `true` for high-stakes environments where you want explicit
    /// confirmation even for user-typed commands.
    ///
    /// This is the "configurable `!` passthrough" half of GPT 5.5's
    /// review finding #4.
    #[serde(default)]
    pub bang_requires_approval: bool,

    /// When `true`, the active adapter is asked to constrain its
    /// output to well-formed JSON. Concretely: the OpenAI-compat body
    /// builder adds `response_format: {type: "json_object"}` (and
    /// `tool_choice: "auto"` when tools are present); the Ollama body
    /// builder adds `format: "json"`. The regex-based tool-call
    /// fallback is left in place regardless — `response_format` only
    /// constrains the *content*, not the in-band `<tool_call>` block
    /// emission, and some models honour the format hint while still
    /// emitting tool calls inline. Default `false` — opt-in only
    /// because forcing JSON breaks chat-style models.
    #[serde(default)]
    pub json_mode: bool,

    /// Number of recent messages to keep verbatim during naive context
    /// compaction. Kimi-style "tail preservation": the last N messages
    /// (default 2, i.e. one user + one assistant turn) stay untouched so
    /// the model retains the immediate thread. Older messages are
    /// stubbed/condensed. Must be at least 1.
    #[serde(default = "default_preserve_recent_messages")]
    pub preserve_recent_messages: usize,

    /// Maximum trust tier allowed for loaded plugins. Plugins that request
    /// more trust are rejected at load time.
    #[serde(default = "default_max_plugin_trust")]
    pub max_plugin_trust: TrustTier,

    /// If true, a plugin whose manifest `trust` exceeds `max_plugin_trust`
    /// is rejected. If false, the plugin is loaded but its capabilities are
    /// capped to `max_plugin_trust`. Default `true` — least surprise.
    #[serde(default = "default_reject_on_excess_plugin_trust")]
    pub reject_on_excess_plugin_trust: bool,

    /// If true, every loaded plugin directory must contain a `.kirkforge.sig`
    /// detached signature that can be verified with `minisign`. Off by
    /// default.
    #[serde(default)]
    pub plugin_signature_validation: bool,

    /// Path to the minisign public key used for plugin signature
    /// validation. Required when `plugin_signature_validation` is true.
    #[serde(default)]
    pub plugin_public_key_path: Option<String>,

    /// Extra environment variables to forward into plugin tool subprocesses.
    /// A curated baseline (`PATH`, `HOME`, `USER`, `SHELL`, `KIRKFORGE_TOOL_ARGS`,
    /// and a few locale/temp variables) is always forwarded; this list
    /// adds application-specific variables.
    #[serde(default)]
    pub plugin_allowed_env_vars: Vec<String>,

    /// Enable injecting persisted memory facts into the system prompt.
    #[serde(default = "default_memory_enabled")]
    pub memory_enabled: bool,

    /// Token budget for the memory block injected into the system prompt.
    /// Facts are scored and selected greedily until this budget is reached.
    #[serde(default = "default_memory_max_tokens")]
    pub memory_max_tokens: usize,

    /// Maximum number of memory facts to consider for injection per turn,
    /// regardless of budget.
    #[serde(default = "default_memory_top_n")]
    pub memory_top_n: usize,

    /// Write a conversation checkpoint every N messages. 0 disables
    /// message-count checkpointing (the default). Checkpoints are still
    /// written after each completed tool batch regardless of this value.
    #[serde(default = "default_checkpoint_interval_messages")]
    pub checkpoint_interval_messages: usize,

    /// Maximum number of model↔tool iterations within a single turn.
    /// Each tool call response is fed back to the model, which may emit
    /// another tool call. This cap prevents runaway loops during large
    /// codebase analysis. Default 50.
    #[serde(default = "default_max_tool_calls_per_turn")]
    pub max_tool_calls_per_turn: usize,

    /// Maximum number of high-level turns a fork-isolated persona
    /// (/explore, /plan, /coder) may consume before returning to the main
    /// thread. Each persona currently runs one self-contained `run_turn`
    /// (which already caps its internal tool-call loop), so the field acts
    /// as an on/off guard and a reservation for future multi-turn personas.
    #[serde(default = "default_max_persona_turns")]
    pub max_persona_turns: usize,

    /// Per-tool hard timeout in seconds. The executor wraps every tool
    /// call with this deadline. Individual tools may apply shorter
    /// internal timeouts. Default 30 s, clamped to [1, 3600].
    #[serde(default = "default_tool_timeout_secs")]
    pub tool_timeout_secs: Option<u64>,

    /// When `true`, destructive tools (write_file, edit_file, bash)
    /// report what they *would* do without actually doing it. Useful for
    /// reviewing the model's intended edits before allowing them. Read-
    /// only tools still run normally.
    #[serde(default)]
    pub dry_run: bool,

    /// Optional directory containing lifecycle hook scripts (`<event>.sh`).
    /// When `None`, the executor uses the default hooks directory
    /// (`~/.local/share/kirkforge/hooks/`). Set this for tests or custom
    /// deployments.
    #[serde(default)]
    pub hooks_dir: Option<PathBuf>,

    /// HTTP request timeout in seconds for model API calls. Increase for
    /// slow local models (e.g. 600 for a 3B quant on CPU).
    #[serde(default = "default_request_timeout_secs")]
    pub request_timeout_secs: u64,

    /// When `true`, successful model streams are cached on disk and replayed
    /// for identical subsequent requests. Keys are content-addressed by
    /// `(model, system_prompt_hash, messages_hash, tools_hash, json_mode)`.
    /// Default: false.
    #[serde(default)]
    pub cache_enabled: bool,

    /// Directory where the model-response cache stores entries. When `None`,
    /// the default `~/.local/share/kirkforge/cache/` is used.
    #[serde(default)]
    pub cache_dir: Option<PathBuf>,
}

/// Configuration for a single MCP server connection.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServerConfig {
    /// Human-readable name for this server (used in tool prefix).
    pub name: String,
    /// Command to spawn (e.g., "npx", "python3").
    pub command: String,
    /// Arguments passed to the command.
    #[serde(default)]
    pub args: Vec<String>,
    /// Additional environment variables for the subprocess.
    #[serde(default)]
    pub env_vars: HashMap<String, String>,
}

fn default_summarize_model() -> String {
    "qwen2.5:3b".into()
}

fn default_carryover_enabled() -> bool {
    true
}

fn default_bash_sandbox_workdir() -> bool {
    true
}

fn default_max_file_read_size() -> usize {
    1024 * 1024
}

fn default_max_overwrite_size() -> usize {
    1024 * 1024
}

fn default_preserve_recent_messages() -> usize {
    2
}

fn default_max_plugin_trust() -> TrustTier {
    TrustTier::Shell
}

fn default_reject_on_excess_plugin_trust() -> bool {
    true
}

fn default_max_tool_calls_per_turn() -> usize {
    50
}

fn default_max_persona_turns() -> usize {
    10
}

fn default_commit_max_file_size() -> u64 {
    5 * 1024 * 1024
}

fn default_tool_timeout_secs() -> Option<u64> {
    Some(30)
}

fn default_request_timeout_secs() -> u64 {
    600
}

fn default_memory_enabled() -> bool {
    true
}

fn default_memory_max_tokens() -> usize {
    500
}

fn default_memory_top_n() -> usize {
    10
}

fn default_checkpoint_interval_messages() -> usize {
    0
}

impl Default for Config {
    fn default() -> Self {
        // `sandbox_dir` is left as `None` here. The launch-time
        // resolution of the working directory (which would otherwise
        // belong in `Default::default`) is the caller's job — see
        // `main.rs::run_session`, which calls `current_dir()` once,
        // freezes the value, and assigns it before constructing the
        // executor.
        //
        // Review.md arch concern #3: the previous code resolved
        // `current_dir()` inside `Default::default()`. That ran
        // *before* any validation, and a launch-time cwd failure
        // (e.g. cwd deleted) silently dropped sandbox protection
        // because the value fell through to `None`. Resolving once
        // at startup and freezing it gives a deterministic, auditable
        // policy: the operator sees exactly which directory was
        // captured, and a deletion-after-launch race can't widen the
        // sandbox.
        //
        // Operators who want explicit opt-out still can: set
        // `sandbox_dir = ""` in the config file or export
        // `KIRKFORGE_SANDBOX_DIR=""` (both are checked in
        // `access_from_config` and resolve to `sandbox_dir = None`).
        Self {
            default_model: "qwen2.5:7b".into(),
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
            max_overwrite_size: 1024 * 1024,
            follow_symlinks: false,
            block_binary_reads: false,
            bash_sandbox_workdir: true,
            carryover_enabled: true,
            summarize_model: "qwen2.5:3b".into(),
            summarize_enabled: false,
            routing_enabled: false,
            router_model: String::new(),
            routing_model_map: HashMap::new(),
            mcp_servers: vec![],
            bang_requires_approval: false,
            json_mode: false,
            preserve_recent_messages: default_preserve_recent_messages(),
            max_plugin_trust: default_max_plugin_trust(),
            reject_on_excess_plugin_trust: default_reject_on_excess_plugin_trust(),
            plugin_signature_validation: false,
            plugin_public_key_path: None,
            plugin_allowed_env_vars: vec![],
            memory_enabled: default_memory_enabled(),
            memory_max_tokens: default_memory_max_tokens(),
            memory_top_n: default_memory_top_n(),
            checkpoint_interval_messages: default_checkpoint_interval_messages(),
            max_tool_calls_per_turn: default_max_tool_calls_per_turn(),
            max_persona_turns: default_max_persona_turns(),
            hooks_dir: None,
            commit_max_file_size: default_commit_max_file_size(),
            tool_timeout_secs: default_tool_timeout_secs(),
            request_timeout_secs: default_request_timeout_secs(),
            dry_run: false,
            cache_enabled: false,
            cache_dir: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum TruncationStrategy {
    DropOldest,
    KeepToolOnly,
    SummarizeMiddle,
}

#[derive(Debug, Clone)]
pub struct ToolDef {
    pub name: &'static str,
    pub description: &'static str,
    pub parameters: serde_json::Value,
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

#[derive(Debug, Clone, Copy, PartialEq, clap::ValueEnum)]
pub enum OutputFormat {
    Text,

    Json,

    StreamJson,
}

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
