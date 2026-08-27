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
    use crate::shared::test_util::EnvGuard;

    // WO 47.21: these tests mutate process-global PROVIDER_API_KEY env
    // vars — run in parallel they race (one test's remove vs another's
    // set). Serialize the whole module on one lock; the tests are
    // micro-fast so the serialization costs nothing.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn config_key_wins_over_env() {
        let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _env = EnvGuard::set("ANTHROPIC_API_KEY", "env-key");
        let result = resolve_api_key("anthropic", Some("config-key"));
        assert_eq!(result, Some("config-key".to_string()));
    }

    #[test]
    fn env_key_used_when_no_config() {
        let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _env = EnvGuard::set("ANTHROPIC_API_KEY", "env-key");
        let result = resolve_api_key("anthropic", None);
        assert_eq!(result, Some("env-key".to_string()));
    }

    #[test]
    fn env_key_used_when_config_empty() {
        let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _env = EnvGuard::set("ANTHROPIC_API_KEY", "env-key");
        let result = resolve_api_key("anthropic", Some(""));
        assert_eq!(result, Some("env-key".to_string()));
    }

    #[test]
    fn none_when_both_missing() {
        let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _env = EnvGuard::remove("ANTHROPIC_API_KEY");
        let result = resolve_api_key("anthropic", None);
        assert!(result.is_none());
    }

    #[test]
    fn none_when_env_empty() {
        let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _env = EnvGuard::set("OPENAI_API_KEY", "");
        let result = resolve_api_key("openai", None);
        assert!(result.is_none());
    }

    #[test]
    fn provider_case_insensitive_env() {
        let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _env = EnvGuard::set("DEEPSEEK_API_KEY", "ds-key");
        let result = resolve_api_key("deepseek", None);
        assert_eq!(result, Some("ds-key".to_string()));
    }

    #[test]
    fn gemini_provider_resolves() {
        let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _env = EnvGuard::set("GEMINI_API_KEY", "gem-key");
        let result = resolve_api_key("gemini", None);
        assert_eq!(result, Some("gem-key".to_string()));
    }

    #[test]
    fn kimi_provider_resolves() {
        let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _env = EnvGuard::set("KIMI_API_KEY", "kimi-key");
        let result = resolve_api_key("kimi", None);
        assert_eq!(result, Some("kimi-key".to_string()));
    }
}
