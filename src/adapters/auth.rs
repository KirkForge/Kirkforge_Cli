//! Per-provider API key resolution.
//!
//! Resolves the first non-empty value from:
//!   1. Config field (`[model].<provider>_api_key`)
//!   2. Environment variable (`<PROVIDER>_API_KEY`)
//!   3. Keychain (not yet implemented)

/// Resolve an API key for the given provider.
///
/// `provider` is a lowercase string like `"anthropic"`, `"openai"`, etc.
/// `config_key` is the value from the corresponding TOML config field
/// (already loaded, may be `None`).
///
/// Resolution order: config field → `<PROVIDER>_API_KEY` env → keychain (not yet implemented).
pub fn resolve_api_key(provider: &str, config_key: Option<&str>) -> Option<String> {
    // 1. Config field
    if let Some(key) = config_key {
        if !key.is_empty() {
            return Some(key.to_string());
        }
    }

    // 2. Environment variable: <PROVIDER>_API_KEY
    let env_var = format!("{}_API_KEY", provider.to_uppercase());
    if let Ok(val) = std::env::var(&env_var) {
        if !val.is_empty() {
            return Some(val);
        }
    }

    // 3. Keychain: not yet implemented. Resolution stops here.
    // ponytail: wire keyring::Entry when a provider needs headless auth;
    // env vars cover all current use-cases.
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_key_wins_over_env() {
        std::env::set_var("ANTHROPIC_API_KEY", "env-key");
        let result = resolve_api_key("anthropic", Some("config-key"));
        assert_eq!(result, Some("config-key".to_string()));
        std::env::remove_var("ANTHROPIC_API_KEY");
    }

    #[test]
    fn env_key_used_when_no_config() {
        std::env::set_var("ANTHROPIC_API_KEY", "env-key");
        let result = resolve_api_key("anthropic", None);
        assert_eq!(result, Some("env-key".to_string()));
        std::env::remove_var("ANTHROPIC_API_KEY");
    }

    #[test]
    fn env_key_used_when_config_empty() {
        std::env::set_var("ANTHROPIC_API_KEY", "env-key");
        let result = resolve_api_key("anthropic", Some(""));
        assert_eq!(result, Some("env-key".to_string()));
        std::env::remove_var("ANTHROPIC_API_KEY");
    }

    #[test]
    fn none_when_both_missing() {
        std::env::remove_var("ANTHROPIC_API_KEY");
        let result = resolve_api_key("anthropic", None);
        assert!(result.is_none());
    }

    #[test]
    fn none_when_env_empty() {
        std::env::set_var("OPENAI_API_KEY", "");
        let result = resolve_api_key("openai", None);
        assert!(result.is_none());
        std::env::remove_var("OPENAI_API_KEY");
    }

    #[test]
    fn provider_case_insensitive_env() {
        std::env::set_var("DEEPSEEK_API_KEY", "ds-key");
        let result = resolve_api_key("deepseek", None);
        assert_eq!(result, Some("ds-key".to_string()));
        std::env::remove_var("DEEPSEEK_API_KEY");
    }

    #[test]
    fn gemini_provider_resolves() {
        std::env::set_var("GEMINI_API_KEY", "gem-key");
        let result = resolve_api_key("gemini", None);
        assert_eq!(result, Some("gem-key".to_string()));
        std::env::remove_var("GEMINI_API_KEY");
    }

    #[test]
    fn kimi_provider_resolves() {
        std::env::set_var("KIMI_API_KEY", "kimi-key");
        let result = resolve_api_key("kimi", None);
        assert_eq!(result, Some("kimi-key".to_string()));
        std::env::remove_var("KIMI_API_KEY");
    }
}
