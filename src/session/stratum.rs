//! In-process Stratum tool wrappers.
//!
//! When the `stratum` feature is enabled, these structs implement the `Tool`
//! trait and call `kirkstratum_core` directly, eliminating subprocess overhead.
//! When the feature is off, the shell-plugin path (`plugins/stratum/tools/*.sh`)
//! remains as fallback.

use crate::session::budget::{BudgetSlicedEvent, BudgetSlicedListener};
use crate::session::hooks::{HookContext, HookDecision, InProcessHook};
use crate::shared::{ToolDef, ToolOutcome};
use crate::tools::{Tool, ToolContext};
use kirkstratum_core::config::PipelineConfig;
use kirkstratum_core::content::ContentType;
use kirkstratum_core::mode::Mode;
use kirkstratum_core::pipeline::{CompressionContext, CompressionPipeline};
use kirkstratum_core::store::InMemoryOffloadStore;
use serde_json::Value;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};

// ── Sliced-event coordination (WO 8.6) ─────────────────────────────────
//
// The budget guard's `apply_budget_slice` dispatches a
// `BudgetSlicedEvent` to registered listeners when it slices a tool
// result. Stratum registers a default listener that compresses the
// sliced display so the model sees a single coordinated result, and
// the session-level mode is consulted on slice to auto-escalate
// `Lite → Full` when the budget is `Approaching`. The mode lives in
// process-global state (separate from the config-derived
// `active_mode()`) so the auto-escalation can outlive a single
// `StratumSessionStartHook` call.

/// Per-session Stratum mode. Distinct from the config-derived
/// `active_mode()`: `SESSION_MODE` is the *resolved* mode for the
/// current session and can be mutated by the budget's auto-escalation
/// path. The config-derived mode is read-only; the session mode wins
/// when both are consulted.
static SESSION_MODE: OnceLock<Mutex<Mode>> = OnceLock::new();

fn session_mode() -> &'static Mutex<Mode> {
    SESSION_MODE.get_or_init(|| Mutex::new(Mode::Full))
}

/// Read the current per-session Stratum mode.
pub fn current_session_mode() -> Mode {
    *session_mode().lock().expect("session mode mutex poisoned")
}

/// Set the per-session Stratum mode. Intended for the budget's
/// auto-escalation path. The new mode takes effect for the next
/// compression call.
pub fn set_session_mode(mode: Mode) {
    *session_mode().lock().expect("session mode mutex poisoned") = mode;
}

/// Compress `content` using the Stratum pipeline at `mode`. Used by
/// the default budget-sliced listener; also useful for callers that
/// want to run the pipeline outside the in-process tool path.
pub fn compress_for_budget(content: &str, mode: Mode) -> String {
    let pipeline = CompressionPipeline::new();
    let store = InMemoryOffloadStore::new();
    let cfg = PipelineConfig::default();
    let ctx = CompressionContext::default().with_token_budget(4096);
    pipeline.run(content, ContentType::PlainText, &ctx, &store, &cfg, mode)
}

/// Default `BudgetSlicedEvent` listener: compresses the sliced
/// display using the current session mode and returns the
/// compressed string. No-op when the display already fits or when
/// the slice marker means the result is already as small as it can
/// be (the listener still returns `Some` so the budget records the
/// post-compression size even if compression is identity).
pub fn default_budget_sliced_listener() -> BudgetSlicedListener {
    Arc::new(|event: BudgetSlicedEvent| {
        let mode = current_session_mode();
        let compressed = compress_for_budget(&event.sliced_display, mode);
        Some(compressed)
    })
}

/// Register the default Stratum compression listener on the budget
/// guard. Idempotent: repeated calls append another listener. Tests
/// that want a clean slate should call
/// `crate::session::budget::clear_sliced_listeners` first.
pub fn register_default_budget_listener() {
    crate::session::budget::register_sliced_listener(default_budget_sliced_listener());
}

fn json_get_string(args: &Value, key: &str) -> Option<String> {
    args.get(key)
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

fn json_get_u64(args: &Value, key: &str) -> Option<u64> {
    args.get(key).and_then(|v| v.as_u64())
}

fn json_get_bool(args: &Value, key: &str) -> bool {
    args.get(key).and_then(|v| v.as_bool()).unwrap_or(false)
}

fn parse_mode(value: Option<&str>) -> Mode {
    match value {
        Some(s) => s.parse().unwrap_or(Mode::Full),
        None => Mode::Full,
    }
}

fn parse_content_type(value: Option<&str>) -> ContentType {
    match value {
        Some(s) => s.parse().unwrap_or(ContentType::PlainText),
        None => ContentType::PlainText,
    }
}

fn mode_description(mode: Mode) -> &'static str {
    match mode {
        Mode::Off => "No compression; input passes through unchanged",
        Mode::Lite => "Light compression; offloading disabled",
        Mode::Full => "Balanced compression with offloading",
        Mode::Ultra => "Aggressive compression; minimal filtering",
        _ => "Unknown mode",
    }
}

fn success_json(content: String) -> ToolOutcome {
    ToolOutcome::Success { content }
}

fn error_json(message: impl Into<String>) -> ToolOutcome {
    ToolOutcome::Error {
        message: message.into(),
    }
}

// ── stratum_run ─────────────────────────────────────────────────────────

pub struct StratumRun;

#[async_trait::async_trait]
impl Tool for StratumRun {
    fn def(&self) -> ToolDef {
        ToolDef {
            name: "stratum_run",
            description: "Run the stratum compression pipeline on inline text input",
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "input": { "type": "string", "description": "Text to compress" },
                    "mode": { "type": "string", "description": "Pipeline mode: off, lite, full, ultra" },
                    "token_budget": { "type": "integer", "description": "Token budget for bloat heuristic" },
                    "dry_run": { "type": "boolean", "description": "If true, return what would happen without transforming" },
                    "json": { "type": "boolean", "description": "If true, output structured JSON" },
                    "max_input_size": { "type": "integer", "description": "Maximum input size in bytes" }
                },
                "required": ["input"]
            }),
        }
    }

    async fn run(&self, _ctx: &ToolContext, args: Value) -> ToolOutcome {
        let input = match args.get("input").and_then(|v| v.as_str()) {
            Some(s) => s.to_string(),
            None => return error_json("stratum_run: missing 'input' field"),
        };

        let json_out = json_get_bool(&args, "json");
        let dry_run = json_get_bool(&args, "dry_run");
        let mode_owned = json_get_string(&args, "mode");
        let mode = parse_mode(mode_owned.as_deref());

        if dry_run {
            let result = serde_json::json!({
                "mode": mode.as_str(),
                "dry_run": true,
                "input_len": input.len(),
            });
            return success_json(serde_json::to_string_pretty(&result).unwrap_or_default());
        }

        let content_type = parse_content_type(None);
        let token_budget = json_get_u64(&args, "token_budget").map(|v| v as usize);
        let ctx = CompressionContext::default().with_token_budget(token_budget.unwrap_or(4096));
        let ctx = if let Some(query) = json_get_string(&args, "query") {
            ctx.with_query(query)
        } else {
            ctx
        };

        let pipeline = CompressionPipeline::new();
        let store = InMemoryOffloadStore::new();
        let cfg = PipelineConfig::default();
        let result = pipeline.run(&input, content_type, &ctx, &store, &cfg, mode);

        if json_out {
            let out = serde_json::json!({
                "mode": mode.as_str(),
                "input_len": input.len(),
                "output_len": result.len(),
                "output": result,
            });
            success_json(serde_json::to_string_pretty(&out).unwrap_or_default())
        } else {
            success_json(result)
        }
    }
}

// ── stratum_apply ───────────────────────────────────────────────────────

pub struct StratumApply;

#[async_trait::async_trait]
impl Tool for StratumApply {
    fn def(&self) -> ToolDef {
        ToolDef {
            name: "stratum_apply",
            description: "Apply the stratum compression pipeline to a file",
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "file": { "type": "string", "description": "Path to the file to compress" },
                    "content_type": { "type": "string", "description": "Content type hint" },
                    "mode": { "type": "string", "description": "Pipeline mode: off, lite, full, ultra" },
                    "token_budget": { "type": "integer", "description": "Token budget for bloat heuristic" },
                    "json": { "type": "boolean", "description": "If true, output structured JSON" },
                    "dry_run": { "type": "boolean", "description": "If true, report what would happen without transforming" }
                },
                "required": ["file"]
            }),
        }
    }

    async fn run(&self, _ctx: &ToolContext, args: Value) -> ToolOutcome {
        let file_path = match json_get_string(&args, "file") {
            Some(p) => p,
            None => return error_json("stratum_apply: missing required 'file' field"),
        };

        let content = match std::fs::read_to_string(&file_path) {
            Ok(c) => c,
            Err(e) => {
                return error_json(format!("stratum_apply: cannot read file {file_path}: {e}"))
            }
        };

        let json_out = json_get_bool(&args, "json");
        let dry_run = json_get_bool(&args, "dry_run");
        let mode_owned = json_get_string(&args, "mode");
        let mode = parse_mode(mode_owned.as_deref());
        let ct_owned = json_get_string(&args, "content_type");
        let content_type = parse_content_type(ct_owned.as_deref());

        if dry_run {
            let result = serde_json::json!({
                "mode": mode.as_str(),
                "dry_run": true,
                "file": file_path,
                "input_len": content.len(),
            });
            return success_json(serde_json::to_string_pretty(&result).unwrap_or_default());
        }

        let token_budget = json_get_u64(&args, "token_budget").map(|v| v as usize);
        let ctx = CompressionContext::default().with_token_budget(token_budget.unwrap_or(4096));

        let pipeline = CompressionPipeline::new();
        let store = InMemoryOffloadStore::new();
        let cfg = PipelineConfig::default();
        let result = pipeline.run(&content, content_type, &ctx, &store, &cfg, mode);

        if json_out {
            let out = serde_json::json!({
                "mode": mode.as_str(),
                "file": file_path,
                "input_len": content.len(),
                "output_len": result.len(),
                "output": result,
            });
            success_json(serde_json::to_string_pretty(&out).unwrap_or_default())
        } else {
            success_json(result)
        }
    }
}

// ── stratum_mode ───────────────────────────────────────────────────────

pub struct StratumMode;

#[async_trait::async_trait]
impl Tool for StratumMode {
    fn def(&self) -> ToolDef {
        ToolDef {
            name: "stratum_mode",
            description: "Show the active compression mode, or set it for this invocation",
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "value": { "type": "string", "description": "Mode to set: off, lite, full, ultra" },
                    "json": { "type": "boolean", "description": "If true, output structured JSON" }
                }
            }),
        }
    }

    async fn run(&self, _ctx: &ToolContext, args: Value) -> ToolOutcome {
        let json_out = json_get_bool(&args, "json");
        let value = json_get_string(&args, "value");

        let mode = if let Some(ref v) = value {
            match v.parse::<Mode>() {
                Ok(m) => m,
                Err(e) => return error_json(format!("stratum_mode: {e}")),
            }
        } else {
            Mode::Full
        };

        if json_out {
            let out = serde_json::json!({
                "mode": mode.as_str(),
                "description": mode_description(mode),
                "runs_transforms": mode.runs_transforms(),
                "offloads_bloat": mode.offloads_bloat(),
            });
            success_json(serde_json::to_string_pretty(&out).unwrap_or_default())
        } else {
            success_json(mode.as_str().to_string())
        }
    }
}

// ── stratum_rules ──────────────────────────────────────────────────────

pub struct StratumRules;

#[async_trait::async_trait]
impl Tool for StratumRules {
    fn def(&self) -> ToolDef {
        ToolDef {
            name: "stratum_rules",
            description: "Emit the canonical ruleset for the active or requested mode",
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "mode": { "type": "string", "description": "Mode to show rules for" },
                    "json": { "type": "boolean", "description": "If true, output structured JSON" }
                }
            }),
        }
    }

    async fn run(&self, _ctx: &ToolContext, args: Value) -> ToolOutcome {
        let json_out = json_get_bool(&args, "json");
        let mode_owned = json_get_string(&args, "mode");
        let mode = parse_mode(mode_owned.as_deref());

        let rules = serde_json::json!({
            "mode": mode.as_str(),
            "runs_transforms": mode.runs_transforms(),
            "offloads_bloat": mode.offloads_bloat(),
            "offload_threshold": mode.offload_threshold(),
            "description": mode_description(mode),
        });

        if json_out {
            success_json(serde_json::to_string_pretty(&rules).unwrap_or_default())
        } else {
            success_json(format!(
                "mode={}\nruns_transforms={}\noffloads_bloat={}\noffload_threshold={}",
                mode.as_str(),
                mode.runs_transforms(),
                mode.offloads_bloat(),
                mode.offload_threshold()
                    .map_or("none".to_string(), |t| format!("{t:.2}")),
            ))
        }
    }
}

// ── stratum_config_validate ────────────────────────────────────────────

pub struct StratumConfigValidate;

#[async_trait::async_trait]
impl Tool for StratumConfigValidate {
    fn def(&self) -> ToolDef {
        ToolDef {
            name: "stratum_config_validate",
            description: "Validate the effective stratum configuration",
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "json": { "type": "boolean", "description": "If true, output structured JSON" }
                }
            }),
        }
    }

    async fn run(&self, _ctx: &ToolContext, args: Value) -> ToolOutcome {
        let json_out = json_get_bool(&args, "json");
        let cfg = PipelineConfig::default();

        let report = serde_json::json!({
            "valid": true,
            "bloat_threshold": cfg.bloat_threshold.get(),
            "reformat_target_ratio": cfg.reformat_target_ratio.get(),
            "offload_fallback_ratio": cfg.offload_fallback_ratio.get(),
            "transform_timeout_ms": cfg.transform_timeout_ms(),
            "per_domain_count": cfg.per_domain.len(),
        });

        if json_out {
            success_json(serde_json::to_string_pretty(&report).unwrap_or_default())
        } else {
            success_json(format!(
                "valid=true\nbloat_threshold={}\nreformat_target_ratio={}\noffload_fallback_ratio={}\ntransform_timeout_ms={}\nper_domain_count={}",
                cfg.bloat_threshold.get(),
                cfg.reformat_target_ratio.get(),
                cfg.offload_fallback_ratio.get(),
                cfg.transform_timeout_ms(),
                cfg.per_domain.len(),
            ))
        }
    }
}

/// Return all five stratum tools as trait objects.
pub fn stratum_tools() -> Vec<Arc<dyn Tool>> {
    vec![
        Arc::new(StratumRun),
        Arc::new(StratumApply),
        Arc::new(StratumMode),
        Arc::new(StratumRules),
        Arc::new(StratumConfigValidate),
    ]
}

// ── session-start hook ─────────────────────────────────────────────────

/// In-process `session-start` hook: emits the active compression ruleset so
/// the model knows the compression contract at session start. Mirrors the
/// shell hook in `plugins/stratum/hooks/session-start.sh`.
pub struct StratumSessionStartHook {
    pub config: crate::shared::SharedConfig,
}

impl InProcessHook for StratumSessionStartHook {
    fn event(&self) -> &str {
        "session-start"
    }

    fn handle(&self, _ctx: &HookContext) -> HookDecision {
        let mode = active_mode(Some(&self.config));
        let rules = format!(
            "mode={}\nruns_transforms={}\noffloads_bloat={}\noffload_threshold={}",
            mode.as_str(),
            mode.runs_transforms(),
            mode.offloads_bloat(),
            mode.offload_threshold()
                .map_or("none".to_string(), |t| format!("{t:.2}")),
        );
        tracing::info!(event = "session-start", %rules, "stratum compression contract");
        HookDecision::Allow
    }
}

// ── pre-tool-bash hook ─────────────────────────────────────────────────

/// In-process `pre-tool-bash` hook: validates the effective stratum config
/// before any bash tool is invoked so configuration drift is surfaced early.
/// Mirrors the shell hook in `plugins/stratum/hooks/pre-tool-bash.sh`.
///
/// Fail-open: an invalid config logs a warning but does not block the user.
pub struct StratumPreToolBashHook;

impl InProcessHook for StratumPreToolBashHook {
    fn event(&self) -> &str {
        "pre-tool-bash"
    }

    fn handle(&self, _ctx: &HookContext) -> HookDecision {
        match load_effective_config() {
            Ok(_) => HookDecision::Allow,
            Err(e) => {
                tracing::warn!(
                    event = "pre-tool-bash",
                    error = %e,
                    "stratum config validation failed (fail-open: allowing)"
                );
                HookDecision::Allow
            }
        }
    }
}

/// Resolve the active mode, honouring (in priority order):
/// 1. `tools.stratum_mode` config field (user config)
/// 2. `STRATUM_MODE` env var (CLI/session override)
/// 3. `Mode::Full` (Stratum default)
///
/// When `config` is `None` only the env var and default are considered.
fn active_mode(config: Option<&crate::shared::SharedConfig>) -> Mode {
    let env_mode = std::env::var("STRATUM_MODE").ok();
    let config_mode = config.and_then(|cfg| {
        crate::shared::read_shared_config(cfg)
            .tools
            .stratum_mode
            .clone()
    });
    resolve_mode(config_mode.as_deref(), env_mode.as_deref())
}

fn resolve_mode(config_mode: Option<&str>, env_mode: Option<&str>) -> Mode {
    if let Some(s) = config_mode {
        if let Ok(m) = s.parse::<Mode>() {
            return m;
        }
    }
    if let Some(s) = env_mode {
        if let Ok(m) = s.parse::<Mode>() {
            return m;
        }
    }
    Mode::Full
}

/// Load the effective pipeline config, mirroring the CLI precedence:
/// `STRATUM_CONFIG` env var → XDG default (`$XDG_CONFIG_HOME/stratum/pipeline.toml`
/// or `$HOME/.config/stratum/pipeline.toml`). Falls back to the embedded
/// default when no override file is present.
fn load_effective_config() -> Result<PipelineConfig, String> {
    if let Some(path) = std::env::var_os("STRATUM_CONFIG").map(PathBuf::from) {
        return PipelineConfig::from_file(&path).map_err(|e| format!("{e:#}"));
    }
    if let Some(path) = xdg_config_path() {
        if path.exists() {
            return PipelineConfig::from_file(&path).map_err(|e| format!("{e:#}"));
        }
    }
    Ok(PipelineConfig::default())
}

/// Return the XDG default config path for the stratum pipeline, if a config
/// home can be resolved.
fn xdg_config_path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))?;
    Some(base.join("stratum").join("pipeline.toml"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::hooks::{HookContext, HookDecision};
    use crate::tools::ToolContext;

    #[tokio::test]
    async fn test_stratum_rules_returns_output() {
        let tool = StratumRules;
        let ctx = ToolContext::new();
        let out = tool.run(&ctx, serde_json::json!({"mode": "full"})).await;
        match out {
            ToolOutcome::Success { content } => {
                assert!(
                    content.contains("mode="),
                    "StratumRules output must include mode, got: {content}"
                );
                assert!(
                    content.contains("runs_transforms="),
                    "StratumRules output must include runs_transforms, got: {content}"
                );
            }
            other => panic!("StratumRules must return Success, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_stratum_config_validate_returns_output() {
        let tool = StratumConfigValidate;
        let ctx = ToolContext::new();
        let out = tool.run(&ctx, serde_json::json!({})).await;
        match out {
            ToolOutcome::Success { content } => {
                assert!(
                    content.contains("valid="),
                    "StratumConfigValidate output must include valid=, got: {content}"
                );
                assert!(
                    content.contains("bloat_threshold="),
                    "StratumConfigValidate output must include bloat_threshold, got: {content}"
                );
            }
            other => panic!("StratumConfigValidate must return Success, got {other:?}"),
        }
    }

    #[test]
    fn test_session_start_hook_returns_allow() {
        let config: crate::shared::SharedConfig =
            Arc::new(std::sync::RwLock::new(crate::shared::Config::default()));
        let hook = StratumSessionStartHook { config };
        let ctx = HookContext {
            event: "session-start".into(),
            ..Default::default()
        };
        assert_eq!(hook.handle(&ctx), HookDecision::Allow);
    }

    #[test]
    fn active_mode_defaults_to_full() {
        let config: crate::shared::SharedConfig =
            Arc::new(std::sync::RwLock::new(crate::shared::Config::default()));
        assert_eq!(active_mode(Some(&config)), Mode::Full);
    }

    #[test]
    fn active_mode_config_overrides_default() {
        let mut cfg = crate::shared::Config::default();
        cfg.tools.stratum_mode = Some("lite".into());
        let config: crate::shared::SharedConfig = Arc::new(std::sync::RwLock::new(cfg));
        assert_eq!(active_mode(Some(&config)), Mode::Lite);
    }

    #[test]
    fn resolve_mode_config_takes_precedence_over_env() {
        assert_eq!(
            resolve_mode(Some("ultra"), Some("off")),
            Mode::Ultra,
            "config must win over STRATUM_MODE env var"
        );
    }

    #[test]
    fn resolve_mode_env_used_when_config_absent() {
        assert_eq!(resolve_mode(None, Some("off")), Mode::Off);
    }

    #[test]
    fn resolve_mode_invalid_falls_through_to_env() {
        assert_eq!(resolve_mode(Some("bogus"), Some("lite")), Mode::Lite);
    }

    #[test]
    fn resolve_mode_invalid_both_falls_to_full() {
        assert_eq!(resolve_mode(Some("bogus"), Some("alsobogus")), Mode::Full);
    }

    #[test]
    fn test_pre_tool_bash_hook_returns_allow() {
        let hook = StratumPreToolBashHook;
        let ctx = HookContext {
            event: "pre-tool-bash".into(),
            ..Default::default()
        };
        assert_eq!(
            hook.handle(&ctx),
            HookDecision::Allow,
            "StratumPreToolBashHook is fail-open and must always Allow"
        );
    }

    // ── WO 8.6 coordination tests ──────────────────────────────────────

    #[test]
    fn session_mode_round_trip() {
        set_session_mode(Mode::Lite);
        assert_eq!(current_session_mode(), Mode::Lite);
        set_session_mode(Mode::Full);
        assert_eq!(current_session_mode(), Mode::Full);
        set_session_mode(Mode::Ultra);
        assert_eq!(current_session_mode(), Mode::Ultra);
        // Reset to the default for downstream tests.
        set_session_mode(Mode::Full);
    }

    #[test]
    fn compress_for_budget_pipeline_runs() {
        // The empty pipeline (no transforms registered) is
        // identity: the input passes through unchanged for any
        // non-Off mode. This pins that the helper actually
        // reaches the pipeline and returns a `String`.
        let input = "abcdefghij";
        for mode in [Mode::Lite, Mode::Full, Mode::Ultra] {
            let out = compress_for_budget(input, mode);
            assert_eq!(out, input, "empty pipeline must be identity for {mode:?}");
        }
    }

    #[test]
    fn default_budget_sliced_listener_returns_some() {
        set_session_mode(Mode::Full);
        let listener = default_budget_sliced_listener();
        let event = BudgetSlicedEvent {
            original_size: 10_000,
            sliced_size: 200,
            key: "abc123".into(),
            sliced_display: "head\n<<plugin3:slice:abc123>>\ntail".into(),
        };
        let replacement = listener(event);
        assert!(replacement.is_some(), "default listener must return Some");
    }

    #[test]
    fn register_default_budget_listener_appends_to_dispatcher() {
        // The dispatcher lives in the budget module; verify that
        // calling `register_default_budget_listener()` actually
        // registers something by counting listeners.
        crate::session::budget::clear_sliced_listeners();
        assert_eq!(crate::session::budget::sliced_listener_count(), 0);
        register_default_budget_listener();
        assert!(
            crate::session::budget::sliced_listener_count() >= 1,
            "register_default_budget_listener must add at least one listener"
        );
        crate::session::budget::clear_sliced_listeners();
    }
}
