//! Curated environment for plugin subprocesses.
//!
//! The plugin-host executors spawn external scripts. Without `env_clear()`,
//! those scripts inherit the full session environment, including sensitive
//! values such as API keys. This module builds a minimal allowlist: basic
//! user/locale/temp variables plus any variables explicitly supplied by the
//! caller (event-specific verifier/hook variables or tool-argument variables).

use std::collections::HashMap;

/// Environment variables that are safe to forward into plugin subprocesses.
const BASELINE_ENV_VARS: &[&str] = &[
    "PATH",
    "HOME",
    "USER",
    "SHELL",
    "TMPDIR",
    "TEMP",
    "TMP",
    "XDG_RUNTIME_DIR",
    "LANG",
    "LC_ALL",
    "LC_CTYPE",
    "LC_MESSAGES",
];

/// Build a curated environment map.
///
/// Starts with the baseline allowlist from the current process, then overlays
/// the caller-supplied `extra` entries. The caller is expected to use
/// `Command::env_clear()` followed by `Command::envs(curated_env(...))` so the
/// subprocess receives *only* these variables.
pub fn curated_env(extra: &HashMap<String, String>) -> Vec<(String, String)> {
    let mut env = Vec::new();
    for key in BASELINE_ENV_VARS {
        if let Ok(v) = std::env::var(key) {
            env.push(((*key).to_string(), v));
        }
    }
    env.extend(extra.iter().map(|(k, v)| (k.clone(), v.clone())));
    env
}
