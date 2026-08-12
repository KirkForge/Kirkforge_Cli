//! In-process Stratum tool wrappers.
//!
//! These structs implement the `Tool` trait and call `kf_compress_core`
//! directly, eliminating subprocess overhead.

use crate::session::budget::{BudgetSlicedEvent, BudgetSlicedListener};
use crate::session::hooks::{HookContext, HookDecision, InProcessHook, PostHook};
use crate::shared::minify::minify_content_by_ext;
use crate::shared::{ToolDef, ToolOutcome};
use crate::tools::{Tool, ToolContext};
use kf_compress_core::config::PipelineConfig;
use kf_compress_core::content::{detect_content_type, ContentType};
use kf_compress_core::mode::Mode;
use kf_compress_core::pipeline::{CompressionContext, CompressionPipeline, Transform};
use kf_compress_core::rules::build_rules;
use kf_compress_core::store::InMemoryOffloadStore;
use serde_json::Value;
use std::path::PathBuf;
use std::sync::Arc;

// WO 28.2: session-mode global + accessors moved to shared/session_mode.rs
// to break the budget↔stratum production cycle. Re-exported here so all
// existing callers (executor, tests, TUI) compile unchanged.
pub use crate::shared::session_mode::{current_session_mode, set_session_mode};

#[derive(Debug)]
struct MinifyTransform;

impl Transform for MinifyTransform {
    fn apply(&self, content: &str, content_type: ContentType) -> String {
        match content_type {
            ContentType::SourceCode => {
                let ext = guess_ext(content);
                minify_content_by_ext(content, ext, false)
            }
            _ => content.to_string(),
        }
    }
}

fn guess_ext(content: &str) -> &'static str {
    let first = content.lines().next().unwrap_or("");
    if first.contains("fn ")
        || first.contains("pub ")
        || first.contains("use ")
        || content.contains("impl ")
        || content.contains("struct ")
    {
        "rs"
    } else if first.contains("def ") || first.contains("import ") || first.contains("from ") {
        "py"
    } else if first.contains("function ") || first.contains("const ") || first.contains("let ") {
        "js"
    } else {
        "rs"
    }
}

fn make_pipeline() -> CompressionPipeline {
    let mut pipeline = CompressionPipeline::new();
    pipeline.register_content_transform(Arc::new(MinifyTransform));
    pipeline
}

/// Compress `content` using the Stratum pipeline at `mode`. Used by
/// the default budget-sliced listener; also useful for callers that
/// want to run the pipeline outside the in-process tool path.
pub fn compress_with_store(content: &str, mode: Mode, store: &InMemoryOffloadStore) -> String {
    let pipeline = make_pipeline();
    let cfg = PipelineConfig::default();
    let ctx = CompressionContext::default().with_token_budget(4096);
    let content_type = detect_content_type(content);
    pipeline.run(content, content_type, &ctx, store, &cfg, mode)
}
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

// ── Sliced-event coordination (WO 8.6) ─────────────────────────────────

/// Default `BudgetSlicedEvent` listener: compresses the sliced
/// display using the current session mode and returns the
/// compressed string. No-op when the display already fits or when
/// the slice marker means the result is already as small as it can
/// be (the listener still returns `Some` so the budget records the
/// post-compression size even if compression is identity).
///
/// The `store` parameter is the per-session Stratum offload store,
/// replacing the old process-global `OnceLock`.
pub fn default_budget_sliced_listener(store: Arc<InMemoryOffloadStore>) -> BudgetSlicedListener {
    Arc::new(move |event: BudgetSlicedEvent| {
        let mode = current_session_mode();
        let compressed = compress_with_store(&event.sliced_display, mode, &store);
        Some(compressed)
    })
}

/// Register the default Stratum compression listener on the budget
/// guard. Idempotent: repeated calls append another listener. Tests
/// that want a clean slate should call
/// `crate::session::budget::clear_sliced_listeners` first.
pub fn register_default_budget_listener(store: Arc<InMemoryOffloadStore>) {
    crate::session::budget::register_sliced_listener(default_budget_sliced_listener(store));
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

pub struct StratumRun {
    offload_store: Arc<InMemoryOffloadStore>,
}

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

        let content_type = json_get_string(&args, "content_type")
            .as_deref()
            .map(|s| s.parse().unwrap_or(ContentType::PlainText))
            .unwrap_or_else(|| detect_content_type(&input));
        let token_budget = json_get_u64(&args, "token_budget").map(|v| v as usize);
        let ctx = CompressionContext::default().with_token_budget(token_budget.unwrap_or(4096));
        let ctx = if let Some(query) = json_get_string(&args, "query") {
            ctx.with_query(query)
        } else {
            ctx
        };

        let pipeline = make_pipeline();
        let cfg = PipelineConfig::default();
        let result = pipeline.run(&input, content_type, &ctx, &*self.offload_store, &cfg, mode);

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

pub struct StratumApply {
    offload_store: Arc<InMemoryOffloadStore>,
}

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

        let pipeline = make_pipeline();
        let cfg = PipelineConfig::default();
        let result = pipeline.run(
            &content,
            content_type,
            &ctx,
            &*self.offload_store,
            &cfg,
            mode,
        );

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
        let canonical = build_rules(mode);

        let rules = serde_json::json!({
            "mode": mode.as_str(),
            "runs_transforms": mode.runs_transforms(),
            "offloads_bloat": mode.offloads_bloat(),
            "offload_threshold": mode.offload_threshold(),
            "description": mode_description(mode),
            "canonical_rules": canonical,
        });

        if json_out {
            success_json(serde_json::to_string_pretty(&rules).unwrap_or_default())
        } else {
            success_json(format!(
                "mode={}\nruns_transforms={}\noffloads_bloat={}\noffload_threshold={}\n\n{canonical}",
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
        let (valid, issues) = match load_effective_config() {
            Ok(_) => (true, Vec::new()),
            Err(e) => (false, vec![e]),
        };
        let cfg = PipelineConfig::default();

        let report = serde_json::json!({
            "valid": valid,
            "issues": issues,
            "bloat_threshold": cfg.bloat_threshold.get(),
            "reformat_target_ratio": cfg.reformat_target_ratio.get(),
            "offload_fallback_ratio": cfg.offload_fallback_ratio.get(),
            "per_domain_count": cfg.per_domain.len(),
        });

        if json_out {
            success_json(serde_json::to_string_pretty(&report).unwrap_or_default())
        } else {
            let issues_str = if issues.is_empty() {
                String::new()
            } else {
                format!(
                    "\nissues:\n{}",
                    issues
                        .iter()
                        .map(|i| format!("  - {i}"))
                        .collect::<Vec<_>>()
                        .join("\n")
                )
            };
            success_json(format!(
                "valid={valid}\n{issues_str}bloat_threshold={}\nreformat_target_ratio={}\noffload_fallback_ratio={}\nper_domain_count={}",
                cfg.bloat_threshold.get(),
                cfg.reformat_target_ratio.get(),
                cfg.offload_fallback_ratio.get(),
                cfg.per_domain.len(),
            ))
        }
    }
}

/// Return all five stratum tools as trait objects.
/// The `offload_store` is the per-session store shared by all stratum tools.
pub fn stratum_tools(offload_store: Arc<InMemoryOffloadStore>) -> Vec<Arc<dyn Tool>> {
    vec![
        Arc::new(StratumRun {
            offload_store: offload_store.clone(),
        }),
        Arc::new(StratumApply {
            offload_store: offload_store.clone(),
        }),
        Arc::new(StratumMode),
        Arc::new(StratumRules),
        Arc::new(StratumConfigValidate),
    ]
}

// ── session-start hook ─────────────────────────────────────────────────

/// In-process `session-start` hook: emits the active compression ruleset so
/// the model knows the compression contract at session start.
pub struct StratumSessionStartHook {
    pub config: crate::shared::SharedConfig,
}

impl PostHook for StratumSessionStartHook {
    fn event(&self) -> &str {
        "session-start"
    }

    fn handle(&self, _ctx: &HookContext) -> Result<(), String> {
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
        Ok(())
    }
}

// ── pre-tool-bash hook ─────────────────────────────────────────────────

/// In-process `pre-tool-bash` hook: validates the effective stratum config
/// before any bash tool is invoked so configuration drift is surfaced early.
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
    use crate::session::hooks::{HookContext, HookDecision, PostHook};
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
    async fn test_stratum_rules_emits_canonical_rules() {
        let tool = StratumRules;
        let ctx = ToolContext::new();
        let out = tool
            .run(&ctx, serde_json::json!({"mode": "off", "json": true}))
            .await;
        match out {
            ToolOutcome::Success { content } => {
                assert!(
                    content.contains("Ship the smallest change"),
                    "StratumRules must include canonical rules, got: {content}"
                );
            }
            other => panic!("StratumRules must return Success, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_stratum_run_detects_json_content_type() {
        let tool = StratumRun {
            offload_store: Arc::new(InMemoryOffloadStore::new()),
        };
        let ctx = ToolContext::new();
        let json_input =
            serde_json::to_string_pretty(&serde_json::json!({"key": "value"})).unwrap();
        let out = tool
            .run(
                &ctx,
                serde_json::json!({
                    "input": json_input,
                    "json": true,
                    "token_budget": 100000,
                }),
            )
            .await;
        match out {
            ToolOutcome::Success { content } => {
                let v: serde_json::Value = serde_json::from_str(&content).unwrap();
                assert_eq!(v["input_len"], json_input.len() as i64);
            }
            other => panic!("StratumRun must return Success, got {other:?}"),
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
    fn test_session_start_hook_returns_ok() {
        let config: crate::shared::SharedConfig =
            Arc::new(std::sync::RwLock::new(crate::shared::Config::default()));
        let hook = StratumSessionStartHook { config };
        let ctx = HookContext {
            event: "session-start".into(),
            ..Default::default()
        };
        assert_eq!(hook.handle(&ctx), Ok(()));
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
        set_session_mode(Mode::Full);
    }

    #[test]
    fn compress_with_store_pipeline_runs() {
        let store = Arc::new(InMemoryOffloadStore::new());
        let input = "abcdefghij";
        for mode in [Mode::Lite, Mode::Full, Mode::Ultra] {
            let out = compress_with_store(input, mode, &store);
            assert_eq!(
                out, input,
                "pipeline must be identity for plain text in {mode:?}"
            );
        }
    }

    #[test]
    fn default_budget_sliced_listener_returns_some() {
        set_session_mode(Mode::Full);
        let store = Arc::new(InMemoryOffloadStore::new());
        let listener = default_budget_sliced_listener(store);
        let event = BudgetSlicedEvent {
            original_size: 10_000,
            sliced_size: 200,
            key: "abc123".into(),
            sliced_display: "head\n<<kf-budget:slice:abc123>>\ntail".into(),
        };
        let replacement = listener(event);
        assert!(replacement.is_some(), "default listener must return Some");
    }

    #[test]
    fn register_default_budget_listener_appends_to_dispatcher() {
        let store = Arc::new(InMemoryOffloadStore::new());
        crate::session::budget::clear_sliced_listeners();
        assert_eq!(crate::session::budget::sliced_listener_count(), 0);
        register_default_budget_listener(store);
        assert!(
            crate::session::budget::sliced_listener_count() >= 1,
            "register_default_budget_listener must add at least one listener"
        );
        crate::session::budget::clear_sliced_listeners();
    }

    #[test]
    fn json_get_string_returns_string_value() {
        let v = serde_json::json!({"key": "value"});
        assert_eq!(json_get_string(&v, "key"), Some("value".to_string()));
    }

    #[test]
    fn json_get_string_returns_none_for_non_string() {
        let v = serde_json::json!({"key": 42, "n": null, "b": true});
        assert!(json_get_string(&v, "key").is_none());
        assert!(json_get_string(&v, "n").is_none());
        assert!(json_get_string(&v, "b").is_none());
    }

    #[test]
    fn json_get_string_returns_none_for_missing_key() {
        let v = serde_json::json!({"other": "x"});
        assert!(json_get_string(&v, "key").is_none());
    }

    #[test]
    fn json_get_u64_returns_number_value() {
        let v = serde_json::json!({"n": 42});
        assert_eq!(json_get_u64(&v, "n"), Some(42));
    }

    #[test]
    fn json_get_u64_returns_none_for_non_u64() {
        let v = serde_json::json!({"s": "x", "f": 3.5, "neg": -1, "b": true});
        assert!(json_get_u64(&v, "s").is_none());
        assert!(json_get_u64(&v, "f").is_none());
        assert!(json_get_u64(&v, "neg").is_none());
        assert!(json_get_u64(&v, "b").is_none());
    }

    #[test]
    fn json_get_bool_returns_bool_value() {
        let v = serde_json::json!({"t": true, "f": false});
        assert!(json_get_bool(&v, "t"));
        assert!(!json_get_bool(&v, "f"));
    }

    #[test]
    fn json_get_bool_defaults_false_for_missing_or_non_bool() {
        let v = serde_json::json!({"s": "x", "n": 5});
        assert!(!json_get_bool(&v, "missing"));
        assert!(!json_get_bool(&v, "s"));
        assert!(!json_get_bool(&v, "n"));
    }

    #[test]
    fn parse_mode_valid_strings_parse() {
        assert_eq!(parse_mode(Some("off")), Mode::Off);
        assert_eq!(parse_mode(Some("lite")), Mode::Lite);
        assert_eq!(parse_mode(Some("full")), Mode::Full);
        assert_eq!(parse_mode(Some("ultra")), Mode::Ultra);
    }

    #[test]
    fn parse_mode_none_defaults_to_full() {
        assert_eq!(parse_mode(None), Mode::Full);
    }

    #[test]
    fn parse_mode_invalid_defaults_to_full() {
        assert_eq!(parse_mode(Some("bogus")), Mode::Full);
        assert_eq!(parse_mode(Some("")), Mode::Full);
    }

    #[test]
    fn parse_content_type_valid_parses() {
        assert_eq!(parse_content_type(None), ContentType::PlainText);
        let _ = parse_content_type(Some("plaintext"));
    }

    #[test]
    fn parse_content_type_invalid_defaults_to_plain_text() {
        assert_eq!(parse_content_type(Some("bogus")), ContentType::PlainText);
        assert_eq!(parse_content_type(Some("")), ContentType::PlainText);
    }

    #[test]
    fn mode_description_covers_all_known_modes() {
        for mode in [Mode::Off, Mode::Lite, Mode::Full, Mode::Ultra] {
            let desc = mode_description(mode);
            assert!(
                !desc.is_empty(),
                "description for {mode:?} should not be empty"
            );
            assert_ne!(
                desc, "Unknown mode",
                "{mode:?} should have a real description"
            );
        }
    }

    #[test]
    fn success_json_wraps_content() {
        match success_json("hello".to_string()) {
            ToolOutcome::Success { content } => assert_eq!(content, "hello"),
            other => panic!("expected Success, got {other:?}"),
        }
    }

    #[test]
    fn error_json_wraps_message() {
        match error_json("boom") {
            ToolOutcome::Error { message } => assert_eq!(message, "boom"),
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[test]
    fn xdg_config_path_uses_xdg_config_home_when_set() {
        let tmp = tempfile::tempdir().unwrap();
        std::env::set_var("XDG_CONFIG_HOME", tmp.path());
        let path = xdg_config_path().expect("XDG_CONFIG_HOME set should resolve a path");
        assert!(path.ends_with("stratum/pipeline.toml"), "got {path:?}");
        std::env::remove_var("XDG_CONFIG_HOME");
    }

    #[test]
    fn xdg_config_path_falls_back_to_home_dot_config() {
        std::env::remove_var("XDG_CONFIG_HOME");
        let home = std::env::var_os("HOME");
        if home.is_some() {
            let path = xdg_config_path().expect("HOME set should resolve a path");
            assert!(path.ends_with("stratum/pipeline.toml"), "got {path:?}");
        }
    }
}
