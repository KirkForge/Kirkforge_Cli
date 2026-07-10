//! Controlled environment for plugin subprocesses.
//!
//! Plugin hooks, verifiers, and tools must not inherit the host process
//! environment, because that leaks secrets (API keys, tokens, `HOME`, etc.)
//! to arbitrary plugin code. This module builds a minimal, explicit
//! environment instead.

use std::collections::HashMap;
use std::path::Path;

/// Default minimal `PATH` so shell scripts can find common utilities.
const DEFAULT_PATH: &str = "/usr/local/bin:/usr/bin:/bin";

/// Build a safe environment for a plugin subprocess.
///
/// The returned map contains only:
/// - `KIRKFORGE_PLUGIN_ROOT`
/// - `KIRKFORGE_PLUGIN_NAME`
/// - `PATH` (a minimal default)
///
/// Callers may merge in additional allowed variables via `extra`.
pub fn build_plugin_env(
    plugin_root: &Path,
    plugin_name: &str,
    extra: &HashMap<String, String>,
) -> HashMap<String, String> {
    let mut env = HashMap::new();
    env.insert(
        "KIRKFORGE_PLUGIN_ROOT".into(),
        plugin_root.to_string_lossy().into_owned(),
    );
    env.insert("KIRKFORGE_PLUGIN_NAME".into(), plugin_name.into());
    env.insert("PATH".into(), DEFAULT_PATH.into());
    env.extend(extra.iter().map(|(k, v)| (k.clone(), v.clone())));
    env
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_env_is_minimal() {
        let env = build_plugin_env(Path::new("/plugins/x"), "x", &HashMap::new());
        assert!(env.contains_key("KIRKFORGE_PLUGIN_ROOT"));
        assert!(env.contains_key("KIRKFORGE_PLUGIN_NAME"));
        assert!(env.contains_key("PATH"));
        assert!(!env.contains_key("HOME"));
        assert!(!env.contains_key("API_KEY"));
    }

    #[test]
    fn extra_vars_are_included() {
        let mut extra = HashMap::new();
        extra.insert("CUSTOM".into(), "value".into());
        let env = build_plugin_env(Path::new("/plugins/x"), "x", &extra);
        assert_eq!(env.get("CUSTOM"), Some(&"value".into()));
    }
}
