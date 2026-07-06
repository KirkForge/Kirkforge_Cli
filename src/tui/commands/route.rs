//! `/route simple|medium|complex` slash command — switch to the model
//! configured for a complexity tier.
//!
//! The user can pre-define per-tier models in `KirkForge.toml` under
//! `[routing_model_map]` (keys: `simple`, `medium`, `complex`). Missing
//! entries fall back to built-in defaults:
//!
//! - simple → `qwen2.5:3b`
//! - medium → `deepseek-v4-flash:cloud`
//! - complex → `deepseek-v4-pro:cloud`
//!
//! The resolved model name is forwarded through the same `model_tx`
//! channel used by `/model`, so the executor performs the actual swap
//! and emits the same "🔀 Switched to …" confirmation token.

use crate::session::executor::TurnEvent;
use crate::shared::read_shared_config;
use crate::tui::app::AppState;
use tokio::sync::mpsc;

const USAGE: &str = r#"Usage: /route simple|medium|complex

Switches to the model configured for the requested complexity tier.
Tier-to-model mappings live in `routing_model_map` in KirkForge.toml;
missing tiers fall back to built-in defaults.

Examples:
  /route simple
  /route medium
  /route complex"#;

/// Built-in tier defaults when the user has not overridden them in
/// `routing_model_map`.
fn default_model_for_tier(tier: &str) -> &'static str {
    match tier {
        "simple" => "qwen2.5:3b",
        "medium" => "deepseek-v4-flash:cloud",
        "complex" => "deepseek-v4-pro:cloud",
        _ => unreachable!("validated tier"),
    }
}

/// Parse and normalise a tier argument.
fn parse_tier(args: &str) -> Option<&'static str> {
    match args.trim().to_ascii_lowercase().as_str() {
        "simple" | "s" | "easy" => Some("simple"),
        "medium" | "m" | "med" => Some("medium"),
        "complex" | "c" | "hard" | "difficult" => Some("complex"),
        _ => None,
    }
}

/// Resolve a tier to a concrete model name using `Config::routing_model_map`
/// and the built-in defaults.
fn resolve_tier_model(tier: &str, state: &AppState) -> String {
    let cfg = read_shared_config(&state.config);
    cfg.routing_model_map
        .get(tier)
        .cloned()
        .unwrap_or_else(|| default_model_for_tier(tier).to_string())
}

/// Handle `/route <tier>`.
///
/// Validates the tier, resolves it to a model name, then reuses the
/// `/model` switch path so local Ollama validation and background pull
/// behaviour are identical.
pub async fn handle_route_command(
    args: &str,
    model_tx: &mpsc::UnboundedSender<String>,
    event_tx: &mpsc::UnboundedSender<TurnEvent>,
    state: &AppState,
) -> String {
    let Some(tier) = parse_tier(args) else {
        return USAGE.to_string();
    };

    let model = resolve_tier_model(tier, state);
    let base = crate::tui::commands::handle_model_command(&model, model_tx, event_tx, state).await;
    format!("Routing to {tier} tier ({model}).\n{base}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shared::Config;

    fn dummy_state() -> AppState {
        AppState::new(std::sync::Arc::new(std::sync::RwLock::new(
            Config::default(),
        )))
    }

    fn state_with_map(map: std::collections::HashMap<String, String>) -> AppState {
        let cfg = Config {
            routing_model_map: map,
            ..Default::default()
        };
        AppState::new(std::sync::Arc::new(std::sync::RwLock::new(cfg)))
    }

    #[test]
    fn parse_tier_accepts_aliases() {
        assert_eq!(parse_tier("simple"), Some("simple"));
        assert_eq!(parse_tier("s"), Some("simple"));
        assert_eq!(parse_tier("easy"), Some("simple"));
        assert_eq!(parse_tier("medium"), Some("medium"));
        assert_eq!(parse_tier("m"), Some("medium"));
        assert_eq!(parse_tier("complex"), Some("complex"));
        assert_eq!(parse_tier("c"), Some("complex"));
        assert_eq!(parse_tier("hard"), Some("complex"));
        assert_eq!(parse_tier("  COMPLEX  "), Some("complex"));
    }

    #[test]
    fn parse_tier_rejects_unknown() {
        assert_eq!(parse_tier(""), None);
        assert_eq!(parse_tier("unknown"), None);
        assert_eq!(parse_tier("simple medium"), None);
    }

    #[test]
    fn resolve_tier_model_uses_defaults() {
        let state = dummy_state();
        assert_eq!(resolve_tier_model("simple", &state), "qwen2.5:3b");
        assert_eq!(
            resolve_tier_model("medium", &state),
            "deepseek-v4-flash:cloud"
        );
        assert_eq!(
            resolve_tier_model("complex", &state),
            "deepseek-v4-pro:cloud"
        );
    }

    #[test]
    fn resolve_tier_model_uses_config_map() {
        let mut map = std::collections::HashMap::new();
        map.insert("simple".to_string(), "my-simple-model".to_string());
        map.insert("complex".to_string(), "my-big-model".to_string());
        let state = state_with_map(map);
        assert_eq!(resolve_tier_model("simple", &state), "my-simple-model");
        assert_eq!(
            resolve_tier_model("medium", &state),
            "deepseek-v4-flash:cloud"
        );
        assert_eq!(resolve_tier_model("complex", &state), "my-big-model");
    }

    #[tokio::test]
    async fn empty_args_returns_usage() {
        let (tx, mut rx) = mpsc::unbounded_channel::<String>();
        let (_event_tx, _event_rx) = mpsc::unbounded_channel::<TurnEvent>();
        let state = dummy_state();
        let out = handle_route_command("", &tx, &_event_tx, &state).await;
        assert!(out.starts_with("Usage"), "got: {out}");
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn unknown_tier_returns_usage() {
        let (tx, mut rx) = mpsc::unbounded_channel::<String>();
        let (_event_tx, _event_rx) = mpsc::unbounded_channel::<TurnEvent>();
        let state = dummy_state();
        let out = handle_route_command("superuser", &tx, &_event_tx, &state).await;
        assert!(out.starts_with("Usage"), "got: {out}");
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn valid_tier_sends_resolved_model() {
        let (tx, mut rx) = mpsc::unbounded_channel::<String>();
        let (_event_tx, _event_rx) = mpsc::unbounded_channel::<TurnEvent>();
        let state = dummy_state();
        let out = handle_route_command("simple", &tx, &_event_tx, &state).await;
        assert!(out.contains("simple"), "got: {out}");
        assert!(out.contains("qwen2.5:3b"), "got: {out}");
        let received = rx.try_recv().expect("channel should have a value");
        assert_eq!(received, "qwen2.5:3b");
    }

    #[tokio::test]
    async fn valid_tier_sends_config_override() {
        let mut map = std::collections::HashMap::new();
        map.insert("medium".to_string(), "custom-medium".to_string());
        let state = state_with_map(map);
        let (tx, mut rx) = mpsc::unbounded_channel::<String>();
        let (_event_tx, _event_rx) = mpsc::unbounded_channel::<TurnEvent>();
        let out = handle_route_command("medium", &tx, &_event_tx, &state).await;
        assert!(out.contains("custom-medium"), "got: {out}");
        let received = rx.try_recv().expect("channel should have a value");
        assert_eq!(received, "custom-medium");
    }
}
