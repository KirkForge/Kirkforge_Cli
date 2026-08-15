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

pub fn build_reqwest_client(timeout: Option<std::time::Duration>) -> reqwest::Client {
    let mut builder = reqwest::Client::builder().tcp_nodelay(true);
    if let Some(t) = timeout {
        builder = builder.timeout(t);
    }
    builder.build().unwrap_or_else(|e| {
        tracing::warn!(error = %e, "failed to build reqwest client; falling back to default");
        reqwest::Client::new()
    })
}

pub mod audit;
pub mod event_bus;
pub mod metrics;
pub mod minify;
pub mod permission;
// WO 28.2: gated on the stratum feature because session_mode uses
// kf_compress_core::mode::Mode, and kf-compress-core is only a dep when
// stratum is enabled (mirrors `pub mod stratum;` gating in session/mod.rs).
#[cfg(feature = "stratum")]
pub mod session_mode;

#[cfg(test)]
pub mod test_util;

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock, RwLock};

pub mod config;

// WO 28.1: access (PathGuard/DenyList/GuardVerdict) relocated from session —
// pure data/logic with no session state. session re-exports it for legacy callers.
pub mod access;
// WO 28.1: bash command-string safety gate relocated from session/bash_runner —
// pure static analysis (no I/O). session::bash_runner re-exports for legacy callers.
pub mod bash_safety;
// WO 28.1: UndoKind relocated from session::undo — pure enum. session::undo
// re-exports it for its own callers (UndoOp/UndoStack stay in session).
pub mod undo;
// WO 32.8: shell I/O + background-job registry port. Re-exports
// session::bash_runner / session::bash_jobs so tools/ depends on shared,
// not session. Impls stay in session (process_group/landlock/seccomp deps).
pub mod shell;
// WO 32.8: prompt-injection memory store port. Re-exports session::memory
// so tools/ depends on shared, not session. Impl stays in session
// (data_dir/prompt deps). Not the routing store (crates/kf-memory-store).
pub mod memory;

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

pub fn write_shared_config(cfg: &SharedConfig) -> std::sync::RwLockWriteGuard<'_, Config> {
    cfg.write().unwrap_or_else(|e| e.into_inner())
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
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum ToolChoice {
    #[default]
    Auto,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ResponseFormat {
    #[default]
    Text,
    JsonObject,
    JsonSchema {
        name: String,
        schema: serde_json::Value,
    },
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

    /// Activate the Anthropic HOSTED computer_use beta (coordinate-vision
    /// model) instead of the local headless-Chrome CDP tool. Requires the
    /// `computer_use` Cargo feature; otherwise this flag is inert. When
    /// true, `width`/`height` are sent as the hosted display dims. Default
    /// false. See WO 28.16.
    #[serde(default)]
    pub hosted: bool,
}

// ponytail: fields serde-deserialized from config; replace with const when config schema stabilizes
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

/// Lightweight sandbox hardening for the non-Docker bash path.
///
/// When `harden` is true and Docker is NOT enabled, the bash tool applies
/// rlimits to the child shell before exec (Unix only): `RLIMIT_CPU` caps
/// CPU seconds (SIGXCPU on exhaustion), `RLIMIT_AS` caps address space
/// (ENOMEM on malloc/brk past the cap), `RLIMIT_FSIZE` caps the size of
/// any single file the child creates (SIGXFSZ on write past the cap).
///
/// Default limits are deliberately generous (5 min CPU, 2 GiB address
/// space, 512 MiB file) so a normal `cargo build` / `cargo test` completes
/// but a runaway `:(){ :|:& };:` fork bomb or `cat /dev/urandom > /tmp/x`
/// is contained. seccomp is documented as future work in ADR-054 — it
/// needs a BPF compiler that's too heavy for the size-optimized binary.
///
/// On Windows `harden` is a no-op with a one-shot warning (rlimits are a
/// Unix-only concept; Windows has job objects but they're a separate
/// API surface and out of scope for this WO).
// ponytail: fields serde-deserialized from config; replace with const when config schema stabilizes
// NOTE: Default is NOT derived — #[derive(Default)] would zero the rlimit fields
// (u64::default() = 0), contradicting the serde defaults of 300/2048/512. A zero
// CPU/memory/filesize limit kills every spawned subprocess instantly via SIGXCPU/
// SIGXFSZ. The manual Default below matches the serde defaults. (WO 27.2-R2 root
// cause for 11 "known-broken" ignored tests + a latent production footgun.)
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SandboxConfig {
    /// Enable rlimit hardening for the non-Docker bash path. Default
    /// false. When Docker is enabled, this flag is ignored (Docker
    /// already enforces `--memory` and `--cpus`).
    #[serde(default)]
    pub harden: bool,

    /// Disable network access for bash commands (Linux only). Uses
    /// `unshare(CLONE_NEWNET)` to place the child in an empty network
    /// namespace. No-op on non-Linux with a one-shot warning.
    /// Requires `--harden` to be set; ignored otherwise.
    #[serde(default)]
    pub no_network: bool,

    /// Block file edits in --harden mode. When true, edit_file and
    /// write_file return Failure instead of applying the edit.
    #[serde(default)]
    pub block_edits: bool,

    /// Escape hatch: when true, suppresses the production-mode refusal
    /// for missing sandbox configuration (WO 21.7-R5). Set by
    /// `--i-accept-unsandboxed`. Not persisted to config.toml.
    #[serde(default)]
    pub accept_unsandboxed: bool,

    /// CPU time limit in seconds. Default 300 (5 min). When the child
    /// exceeds this *wall-clock CPU* (not elapsed time), the kernel
    /// sends SIGXCPU; if uncaught the process dies with SIGKILL after
    /// a one-second grace period.
    #[serde(default = "default_sandbox_cpu_limit_secs")]
    pub cpu_limit_secs: u64,

    /// Address space limit in megabytes. Default 2048 (2 GiB). Maps to
    /// `RLIMIT_AS` in bytes. A child that mallocs/mmaps past this gets
    /// ENOMEM from the kernel.
    #[serde(default = "default_sandbox_memory_limit_mb")]
    pub memory_limit_mb: u64,

    /// Max file size in megabytes. Default 512 (MiB). Maps to
    /// `RLIMIT_FSIZE` in bytes. A child that writes past this gets
    /// SIGXFSZ.
    #[serde(default = "default_sandbox_filesize_limit_mb")]
    pub filesize_limit_mb: u64,
}

impl SandboxConfig {
    /// Produce a per-plugin config by overlaying the manifest's
    /// `resource_limits` on the global default (WO 11.5, ADR-060).
    /// Each `Some` field in `limits` overrides the global; `None`
    /// fields fall back to the global. The `harden` flag is inherited
    /// from the global (a per-plugin manifest cannot disable
    /// hardening — only raise limits).
    pub fn merge_with(&self, limits: Option<&kf_plugin_sdk::ResourceLimits>) -> Self {
        let Some(limits) = limits else {
            return self.clone();
        };
        Self {
            harden: self.harden,
            no_network: self.no_network,
            block_edits: self.block_edits,
            accept_unsandboxed: self.accept_unsandboxed,
            cpu_limit_secs: limits.cpu_secs.unwrap_or(self.cpu_limit_secs),
            memory_limit_mb: limits.memory_mb.unwrap_or(self.memory_limit_mb),
            filesize_limit_mb: limits.filesize_mb.unwrap_or(self.filesize_limit_mb),
        }
    }
}

fn default_sandbox_cpu_limit_secs() -> u64 {
    300
}
fn default_sandbox_memory_limit_mb() -> u64 {
    2048
}
fn default_sandbox_filesize_limit_mb() -> u64 {
    512
}

/// Manual `Default` matching the serde defaults above. Do NOT derive —
/// see the struct-level note (derive would zero the rlimits).
impl Default for SandboxConfig {
    fn default() -> Self {
        Self {
            harden: false,
            no_network: false,
            block_edits: false,
            accept_unsandboxed: false,
            cpu_limit_secs: default_sandbox_cpu_limit_secs(),
            memory_limit_mb: default_sandbox_memory_limit_mb(),
            filesize_limit_mb: default_sandbox_filesize_limit_mb(),
        }
    }
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

    /// Extract the text content of the outcome for hook consumers.
    ///
    /// Returns the primary text content (success message, file content, diff,
    /// grep matches, or error message). Used by in-process hooks (e.g. the
    /// budget guard) to inspect tool results without parsing the full
    /// enum at the call site.
    pub fn text_content(&self) -> String {
        match self {
            Self::Success { content } => content.clone(),
            Self::Error { message } => message.clone(),
            Self::Failure(e) => e.to_user_message(),
            Self::FileContent { content, .. } => content.clone(),
            Self::FileEdit { diff, .. } => diff.clone(),
            Self::GrepMatches { matches, total, .. } => {
                format!("{} matches ({} total shown)", matches.len(), total)
            }
            Self::Image { .. } => String::from("[image data]"),
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
        .unwrap_or_else(|| {
            PRICING_TABLE
                .last()
                .expect("PRICING_TABLE must not be empty")
        });

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

/// Compute the backoff for retry `attempt` (1-indexed).
///
/// Uses exponential backoff starting at 1 s with a small deterministic
/// jitter (up to 250 ms per attempt, capped at 1 s). The jitter is
/// computed from the attempt number rather than a random source so tests
/// are stable and no new dependency is required.
pub fn retry_backoff(attempt: u32) -> std::time::Duration {
    let shift = (attempt - 1).min(63);
    let base_s = 1u64 << shift;
    let jitter_ms = (attempt as u64).saturating_mul(250).min(1000);
    std::time::Duration::from_millis(base_s.saturating_mul(1000).saturating_add(jitter_ms))
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

#[cfg(test)]
mod backoff_tests {
    use super::*;

    #[test]
    fn backoff_grows_with_capped_jitter() {
        let b1 = retry_backoff(1);
        let b2 = retry_backoff(2);
        let b3 = retry_backoff(3);

        assert!(b1 >= std::time::Duration::from_secs(1));
        assert!(b1 <= std::time::Duration::from_millis(1250));

        assert!(b2 >= std::time::Duration::from_secs(2));
        assert!(b2 <= std::time::Duration::from_millis(2500));

        assert!(b3 >= std::time::Duration::from_secs(4));
        assert!(b3 <= std::time::Duration::from_millis(5000));

        assert!(b3 > b2 && b2 > b1);
    }
}
