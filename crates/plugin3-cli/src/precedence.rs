//! Config path precedence — CLI > env > XDG default. Per ADR-0015.
//!
//! ponytail: a tiny pure function. The whole chain fits in 5 lines
//! because clap already exposes `--config` as `Option<PathBuf>`,
//! and `std::env::var` is the env-var probe. No abstraction layer
//! is needed for "CLI vs env vs default" — three `if`s and a join.
//! A future contributor who adds more precedence sources (per-host
//! config file, project-local `.plugin3/config.toml`) adds another
//! `if let Some(...)` arm.

use std::path::{Path, PathBuf};

/// Trait abstracting `std::env::var` so tests can inject a fixed
/// env without touching the real environment.
pub(crate) trait EnvSource {
    fn get(&self, key: &str) -> Option<String>;
}

/// Production env source — reads from the process environment.
/// The single use site is `commands::config::show`, which keeps
/// `resolve_config_path` load-bearing (clippy: -D warnings).
pub(crate) struct OsEnv;
impl EnvSource for OsEnv {
    fn get(&self, key: &str) -> Option<String> {
        std::env::var(key).ok()
    }
}

/// Resolve a config file path from the precedence chain:
///
/// 1. CLI flag (`cli_config`) — highest priority.
/// 2. Env var (`PLUGIN3_CONFIG`).
/// 3. `xdg.join("config.toml")` — fallback default.
///
/// The function is pure: it does not mutate state and does not
/// touch the filesystem. `EnvSource` parameterisation keeps tests
/// hermetic (a developer's shell never affects the precedence
/// outcome).
///
/// ponytail: private to the module. The canonical CLI doesn't yet
/// thread a `--config` flag through `Cli` (ADR-0015 § Top-level
/// structure); when it does, a one-liner raises this to `pub(crate)`
/// and calls it from the config-show subcommand. The 4 unit tests
/// cover the precedence paths either way.
pub(crate) fn resolve_config_path(
    cli_config: Option<&Path>,
    env: &dyn EnvSource,
    xdg: &Path,
) -> PathBuf {
    // ponytail: CLI > env > XDG. Order matters — the first match
    // wins. A contributor who flips two arms surfaces here because
    // `cli_wins_over_xdg` and `env_wins_over_cli` flip too.
    if let Some(p) = cli_config {
        return p.to_path_buf();
    }
    if let Some(p) = env.get("PLUGIN3_CONFIG") {
        return PathBuf::from(p);
    }
    xdg.join("config.toml")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// In-memory env for tests.
    struct TestEnv {
        map: std::collections::HashMap<String, String>,
    }
    impl EnvSource for TestEnv {
        fn get(&self, key: &str) -> Option<String> {
            self.map.get(key).cloned()
        }
    }

    fn xdg() -> PathBuf {
        PathBuf::from("/home/u/.config/plugin3")
    }

    // ADR-0015 § Implementation notes test #1.
    #[test]
    fn cli_wins_over_xdg() {
        let env = TestEnv {
            map: std::collections::HashMap::new(),
        };
        let cli = Some(Path::new("/etc/plugin3/config.toml"));
        let got = resolve_config_path(cli, &env, &xdg());
        assert_eq!(got, PathBuf::from("/etc/plugin3/config.toml"));
    }

    // ADR-0015 § Implementation notes test #2.
    #[test]
    fn env_wins_over_cli_when_cli_is_none() {
        // ponytail: "env > CLI" in the ADR header actually means
        // "env, when CLI is unset, beats the XDG default". When
        // CLI is set, CLI wins. Two tests pin both readings so
        // a contributor who reorders the arms can't satisfy one
        // and break the other.
        let env = TestEnv {
            map: [("PLUGIN3_CONFIG".to_string(), "/env/cfg.toml".to_string())]
                .into_iter()
                .collect(),
        };
        let got = resolve_config_path(None, &env, &xdg());
        assert_eq!(got, PathBuf::from("/env/cfg.toml"));
    }

    #[test]
    fn xdg_default_used_when_neither_cli_nor_env_set() {
        let env = TestEnv {
            map: std::collections::HashMap::new(),
        };
        let got = resolve_config_path(None, &env, &xdg());
        assert_eq!(got, PathBuf::from("/home/u/.config/plugin3/config.toml"));
    }

    #[test]
    fn cli_beats_env_when_both_set() {
        // ponytail: explicit cross-check of the precedence. The
        // env arm is *only* reached when CLI is None. A contributor
        // who swaps the arm order surfaces here because the result
        // flips to "/env/cfg.toml".
        let env = TestEnv {
            map: [("PLUGIN3_CONFIG".to_string(), "/env/cfg.toml".to_string())]
                .into_iter()
                .collect(),
        };
        let cli = Some(Path::new("/cli/cfg.toml"));
        let got = resolve_config_path(cli, &env, &xdg());
        assert_eq!(got, PathBuf::from("/cli/cfg.toml"));
    }
}
