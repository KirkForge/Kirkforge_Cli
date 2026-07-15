//! `/route simple|medium|complex` slash command — switch to the model
//! configured for a complexity tier.
//!
//! The user pre-defines per-tier models in `config.toml` under
//! `[routing_model_map]` (keys: `simple`, `medium`, `complex`). Missing
//! entries fall back to `default_model`. If neither is configured, the
//! command tells the user how to fix it instead of guessing a model.
//!
//! The resolved model name is forwarded through the same `model_tx`
//! channel used by `/model`, so the executor performs the actual swap
//! and emits the same "🔀 Switched to …" confirmation token.

use crate::session::executor::TurnEvent;
use crate::session::router::resolve_tier_model;
use crate::shared::read_shared_config;
use crate::tui::app::AppState;
use tokio::sync::mpsc;

const USAGE: &str = r#"Usage: /route simple|medium|complex

Switches to the model configured for the requested complexity tier.
Tier-to-model mappings live in `routing_model_map` in config.toml;
missing tiers fall back to `default_model`.

Examples:
  /route simple
  /route medium
  /route complex"#;

const NO_MODEL: &str = r#"No model configured for that tier.

Set `routing_model_map.<tier>` or `default_model` in config.toml, e.g.:

[routing_model_map]
simple = "qwen3:32b:cloud"
medium = "glm-5.2:cloud"
complex = "kimi-2.7k-coder:cloud"
"#;

/// Parse and normalise a tier argument.
fn parse_tier(args: &str) -> Option<&'static str> {
    match args.trim().to_ascii_lowercase().as_str() {
        "simple" | "s" | "easy" => Some("simple"),
        "medium" | "m" | "med" => Some("medium"),
        "complex" | "c" | "hard" | "difficult" => Some("complex"),
        _ => None,
    }
}

/// Handle `/route <tier>`.
///
/// Validates the tier, resolves it to a model name, then reuses the
/// `/model` switch path so the same validation and background pull
/// behaviour apply.
pub async fn handle_route_command(
    args: &str,
    model_tx: &mpsc::UnboundedSender<String>,
    event_tx: &mpsc::Sender<TurnEvent>,
    state: &AppState,
) -> String {
    let Some(tier) = parse_tier(args) else {
        return USAGE.to_string();
    };

    let model = {
        let cfg = read_shared_config(&state.config);
        resolve_tier_model(&cfg, tier)
    };
    let Some(model) = model else {
        return NO_MODEL.to_string();
    };

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

    fn state_with_map(
        default_model: impl Into<String>,
        map: std::collections::HashMap<String, String>,
    ) -> AppState {
        let cfg = Config {
            default_model: default_model.into(),
            routing_model_map: map,
            ..Config::default()
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
        let state = state_with_map("kimi-2.7k-coder:cloud", std::collections::HashMap::new());
        assert_eq!(
            resolve_tier_model(&read_shared_config(&state.config), "simple"),
            Some("kimi-2.7k-coder:cloud".to_string())
        );
    }

    #[test]
    fn resolve_tier_model_uses_config_map() {
        let mut map = std::collections::HashMap::new();
        map.insert("simple".to_string(), "my-simple-model".to_string());
        map.insert("complex".to_string(), "my-big-model".to_string());
        let state = state_with_map("kimi-2.7k-coder:cloud", map);
        assert_eq!(
            resolve_tier_model(&read_shared_config(&state.config), "simple"),
            Some("my-simple-model".to_string())
        );
        assert_eq!(
            resolve_tier_model(&read_shared_config(&state.config), "medium"),
            Some("kimi-2.7k-coder:cloud".to_string())
        );
        assert_eq!(
            resolve_tier_model(&read_shared_config(&state.config), "complex"),
            Some("my-big-model".to_string())
        );
    }

    #[tokio::test]
    async fn empty_args_returns_usage() {
        let (tx, mut rx) = mpsc::unbounded_channel::<String>();
        let (_event_tx, _event_rx) = mpsc::channel::<TurnEvent>(10_000);
        let state = dummy_state();
        let out = handle_route_command("", &tx, &_event_tx, &state).await;
        assert!(out.starts_with("Usage"), "got: {out}");
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn unknown_tier_returns_usage() {
        let (tx, mut rx) = mpsc::unbounded_channel::<String>();
        let (_event_tx, _event_rx) = mpsc::channel::<TurnEvent>(10_000);
        let state = dummy_state();
        let out = handle_route_command("superuser", &tx, &_event_tx, &state).await;
        assert!(out.starts_with("Usage"), "got: {out}");
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn valid_tier_sends_resolved_model() {
        let (tx, mut rx) = mpsc::unbounded_channel::<String>();
        let (_event_tx, _event_rx) = mpsc::channel::<TurnEvent>(10_000);
        let state = state_with_map("kimi-2.7k-coder:cloud", std::collections::HashMap::new());
        let out = handle_route_command("complex", &tx, &_event_tx, &state).await;
        assert!(out.contains("complex"), "got: {out}");
        assert!(out.contains("kimi-2.7k-coder:cloud"), "got: {out}");
        let received = rx.try_recv().expect("channel should have a value");
        assert_eq!(received, "kimi-2.7k-coder:cloud");
    }

    #[tokio::test]
    async fn valid_tier_sends_config_override() {
        let mut map = std::collections::HashMap::new();
        map.insert("medium".to_string(), "custom-medium".to_string());
        let state = state_with_map("kimi-2.7k-coder:cloud", map);
        let (tx, mut rx) = mpsc::unbounded_channel::<String>();
        let (_event_tx, _event_rx) = mpsc::channel::<TurnEvent>(10_000);
        let out = handle_route_command("medium", &tx, &_event_tx, &state).await;
        assert!(out.contains("custom-medium"), "got: {out}");
        let received = rx.try_recv().expect("channel should have a value");
        assert_eq!(received, "custom-medium");
    }

    #[tokio::test]
    async fn unconfigured_tier_returns_help() {
        let (tx, _rx) = mpsc::unbounded_channel::<String>();
        let (_event_tx, _event_rx) = mpsc::channel::<TurnEvent>(10_000);
        let state = dummy_state();
        let out = handle_route_command("simple", &tx, &_event_tx, &state).await;
        assert!(out.contains("No model configured"), "got: {out}");
    }
}
