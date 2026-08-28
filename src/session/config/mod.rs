/// Config bootstrap — layered config resolution with env var overrides.
///
/// Resolution order (highest to lowest priority):
/// 1. CLI arguments (handled in main.rs)
/// 2. Environment variables (`KF_CODE_*`)
/// 3. Config file (`~/.local/share/kf-code/config.toml`)
/// 4. Built-in defaults
///
/// Environment variable reference:
/// - `KF_CODE_MODEL` — default model name
/// - `KF_CODE_HOST` — Ollama host URL
/// - `KF_CODE_AUTO_APPROVE` — "true" to auto-approve destructive calls
/// - `KF_CODE_DRY_RUN` — "true" to make destructive tools report only
/// - `KF_CODE_SANDBOX_DIR` — sandbox directory path
/// - `KF_CODE_BLOCK_DOTFILES` — "true" to block dotfile writes
/// - `KF_CODE_BLOCK_GITIGNORED_DOTFILES` — "true" to block git-ignored dotfile writes
/// - `KF_CODE_MAX_READ_SIZE` — max file read size in bytes
/// - `KF_CODE_MAX_OVERWRITE_SIZE` — max existing file size that write tools may overwrite
/// - `KF_CODE_FOLLOW_SYMLINKS` — "true" to allow following symlinks
/// - `KF_CODE_BLOCK_BINARY` — "true" to block binary file reads
/// - `KF_CODE_MINIFY_WRITE_SIDE` — "true" to enable minified-envelope write-side expansion
/// - `KF_CODE_MINIFY_ABOVE_BYTES` — auto-minify `read_file` output above this byte threshold (default 4096)
/// - `KF_CODE_SCHEDULED_BASH_AUTO_APPROVE` — "true" to let scheduled bash jobs skip interactive approval
/// - `KF_CODE_MAX_CONCURRENT_SCHEDULED_JOBS` — max concurrent scheduled jobs (clamped to ≥1)
/// - `KF_CODE_MAX_BACKGROUND_TASKS` — max concurrent background tasks (clamped to 1..=64)
/// - `KF_CODE_TASK_CONCURRENCY_MODE` — "queue" (default) or "reject" for background task backpressure
/// - `KF_CODE_BASH_SANDBOX_WORKDIR` — "true"/"false" to force bash cwd into the sandbox
/// - `KF_CODE_BANG_REQUIRES_APPROVAL` — "true" to route `!` passthrough through approval gate
/// - `KF_CODE_JSON_MODE` — "true" to request JSON-formatted model responses
/// - `KF_CODE_REJECT_ON_EXCESS_PLUGIN_TRUST` — "true" to reject plugins above max trust
/// - `KF_CODE_PLUGIN_SIGNATURE_VALIDATION` — "true" to require `.kf-code.sig`
/// - `KF_CODE_PLUGIN_PUBLIC_KEY_PATH` — minisign public key for plugin signatures
/// - `KF_CODE_PLUGIN_ALLOWED_ENV_VARS` — comma-separated extra env vars for plugin tools
/// - `KF_CODE_PLUGIN_SOURCES` — comma-separated `name=path` workspace plugin sources
/// - `KF_CODE_ENABLED_PLUGINS` — comma-separated names from `plugin_sources` to load
/// - `KF_CODE_MEMORY_ENABLED` — "true"/"false" to enable or disable memory injection
/// - `KF_CODE_MEMORY_MAX_TOKENS` — token budget for injected memory facts
/// - `KF_CODE_MEMORY_TOP_N` — maximum number of facts to consider per turn
/// - `KF_CODE_MEMORY_AUTO_POPULATE` — "true"/"false" to enable or disable memory auto-extraction
/// - `KF_CODE_REQUEST_TIMEOUT_SECS` — model request timeout (clamped to ≥1 s)
/// - `KF_CODE_TOOL_TIMEOUT_SECS` — per-tool hard timeout (clamped to [1, 3600])
/// - `KF_CODE_CHECKPOINT_INTERVAL_MESSAGES` — write a checkpoint every N messages
/// - `KF_CODE_SUMMARIZE_MODEL` — fast model used by `/compact`
/// - `KF_CODE_ROUTING_ENABLED` — "true" to enable smart model routing
/// - `KF_CODE_ROUTER_MODEL` — model used for routing classification
/// - `KF_CODE_COMMIT_MAX_FILE_SIZE` — max file size allowed in `/commit`
/// - `KF_CODE_PRESERVE_RECENT_MESSAGES` — number of recent messages kept verbatim on compact
/// - `KF_CODE_MAX_TOOL_CALLS_PER_TURN` — cap on model↔tool iterations per turn
/// - `KF_CODE_MAX_PERSONA_TURNS` — cap on fork-isolated persona turns
/// - `KF_CODE_AUDIT_LOG_PATH` — path for the append-only JSONL audit log (empty disables)
/// - `KF_CODE_HOOKS_DIR` — directory containing lifecycle hook scripts
///
/// Boolean env vars accept `true`/`1`/`yes` (case-insensitive) for true and
/// `false`/`0`/`no` for false. Unrecognized values leave the prior layer
/// unchanged.
use crate::shared::Config;
use anyhow::Context;
use std::path::PathBuf;

mod diff;
mod env_overrides;
mod merge;

use diff::parse_plugin_sources_env;
use env_overrides::apply_env_overrides;
use merge::merge_toml_into_config;

pub use diff::config_diff_summary;

/// Expand a leading `~` in a path string using `$HOME` (or the equivalent
/// on Windows). Falls back to the original string if expansion fails.
fn expand_tilde_str(s: &str) -> String {
    shellexpand::tilde(s).into_owned()
}

/// Parse a boolean environment variable value consistently.
///
/// Treats "true", "1", "yes" (case-insensitive) as true,
/// "false", "0", "no" (case-insensitive) as false, and any other value as
/// `None` so the config default is preserved.
fn parse_bool_env(val: &str) -> Option<bool> {
    if val.eq_ignore_ascii_case("true")
        || val.eq_ignore_ascii_case("1")
        || val.eq_ignore_ascii_case("yes")
    {
        Some(true)
    } else if val.eq_ignore_ascii_case("false")
        || val.eq_ignore_ascii_case("0")
        || val.eq_ignore_ascii_case("no")
    {
        Some(false)
    } else {
        None
    }
}

/// Load config with full layered resolution.
///
/// 1. Start with defaults
/// 2. Override from config file (if exists)
/// 3. Override from environment variables
///
/// The config is NOT written to disk here — that's the caller's
/// responsibility (e.g., on first run or when CLI overrides are provided).
///
/// Returns the resolved config and an optional human-readable warning if
/// the config file existed but could not be fully parsed.
pub fn load_config() -> (Config, Option<String>) {
    let mut cfg = Config::default();
    let mut warning: Option<String> = None;

    // Layer 1: config file
    let path = super::config_path();
    if let Ok(content) = std::fs::read_to_string(&path) {
        // Parse as a TOML table and overlay field-by-field onto the
        // default config (WO 47.2: the overlay serializes the current
        // config, inserts each key, and decodes back via serde — the
        // flatten layout routes keys, types values, and fills missing
        // fields from Default). A minimal or partially-unknown config
        // file loads with user values preserved; a key with a bad value
        // type is skipped without touching other fields.
        match content.parse::<toml::Table>() {
            Ok(table) => {
                merge_toml_into_config(&mut cfg, table);
            }
            Err(e) => {
                let msg = format!("Failed to parse config ({e}), using defaults");
                tracing::warn!(%msg);
                warning = Some(msg);
            }
        }
    }

    // Layer 2: environment variables (`KF_CODE_*`). Documented in the module
    // header above. Only mutates fields whose env var is present.
    apply_env_overrides(&mut cfg);

    (cfg, warning)
}

// First-run onboarding banner text (WO 14.1). Printed to stderr in
// `load_or_create_config` when a new config file is written, so a new
// user gets a concrete next step instead of silent success. Routed to
// stderr (WO 38.10) so it cannot pollute machine-readable stdout when
// `--output stream-json` runs on a fresh data dir. Kept as a pure
// helper so the exact wording is unit-testable without capturing stdout.
fn first_run_banner(path: &std::path::Path) -> String {
    format!(
        "Config created at {}. Next: set a model — try `kf-code run -m qwen2.5:0.5b` \
         (Ollama) or edit `default_model` in the config file. See config.toml.example \
         for all options.",
        path.display()
    )
}

/// Compute the legacy pre-rename config path
/// (`~/.local/share/kirkforge/config.toml`).
///
/// Commit ae0e37d (2026-08-04) renamed the data dir from `kirkforge` to
/// `kf-code`. Existing users have their customized `config.toml` at the
/// old path; without migration, `load_or_create_config` writes fresh
/// defaults and the user's customizations vanish. This helper returns
/// the old path so `load_or_create_config` can migrate the file on
/// first run of the new binary.
///
/// Respects `KF_CODE_LEGACY_DATA_DIR` (mirrors `KF_CODE_DATA_DIR`) so
/// the migration is unit-testable without touching the real legacy dir.
fn legacy_config_path() -> Option<PathBuf> {
    if let Ok(dir) = std::env::var("KF_CODE_LEGACY_DATA_DIR") {
        return Some(PathBuf::from(dir).join("config.toml"));
    }
    directories::ProjectDirs::from("", "", "kirkforge").map(|p| p.data_dir().join("config.toml"))
}

/// Load config and write a default file on first run.
///
/// If the config file doesn't exist, creates it with default values
/// and prints a brief info message. If a config exists at the legacy
/// pre-rename path, it is migrated (copied) to the new location
/// instead of writing defaults — preserving user customizations across
/// the `kirkforge → kf-code` rename.
///
/// **Lenient on parse errors**: a hard TOML parse failure in an existing
/// config file is downgraded to a stderr warning and defaults are used
/// (the `warning` from `load_config`). Use [`load_or_create_config_strict`]
/// on the `run`/`bench` paths where a corrupt config must stop the
/// process with exit 5 instead of silently falling back to defaults.
pub fn load_or_create_config() -> Config {
    load_or_create_config_impl(false).unwrap_or_else(|e| {
        // Lenient path: a hard parse error is surfaced as a warning and
        // we fall back to defaults. This preserves the historical
        // behaviour for plugin/legacy callers that don't gate on config
        // validity.
        let path = super::config_path();
        eprintln!("Warning: {e} ({})", path.display());
        Config::default()
    })
}

/// Strict variant (WO 38.10): like [`load_or_create_config`] but returns
/// `Err` on a hard TOML parse failure in an existing config file. Used on
/// the `run` and `bench run` paths so a corrupt config stops the process
/// (the dispatcher classifies the error as `ConfigParse` → exit 5)
/// instead of silently running with defaults. First-run (no config file)
/// and unknown-key soft-merge warnings are NOT errors — only a TOML
/// syntax error in an existing file is.
pub fn load_or_create_config_strict() -> anyhow::Result<Config> {
    load_or_create_config_impl(true)
}

// Shared body for the lenient and strict variants. `strict` controls
// whether a hard parse error in an existing config file is returned as
// `Err` (strict) or downgraded to a warning + defaults (lenient).
fn load_or_create_config_impl(strict: bool) -> anyhow::Result<Config> {
    let path = super::config_path();
    let exists = path.exists();

    // Migration: if the config file is absent at the current path but
    // exists at the legacy pre-rename path (~/.local/share/kirkforge/),
    // copy it across instead of writing fresh defaults. Commit ae0e37d
    // renamed the data dir; without this, upgrading users lose their
    // customizations on first run of the new binary.
    if !exists {
        if let Some(legacy) = legacy_config_path() {
            if legacy.exists() {
                if let Some(parent) = path.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                if std::fs::copy(&legacy, &path).is_ok() {
                    tracing::info!(
                        from = %legacy.display(),
                        to = %path.display(),
                        "Migrated config from legacy kirkforge data dir"
                    );
                    let (cfg, warning) = load_config();
                    if let Some(w) = warning {
                        if strict {
                            return Err(anyhow::anyhow!("{w}"));
                        }
                        eprintln!("Warning: {w} ({})", path.display());
                    }
                    return Ok(cfg);
                }
            }
        }
    }

    let (cfg, warning) = load_config();
    if let Some(w) = &warning {
        if strict && exists {
            // Hard parse failure in an existing config: surface as Err so
            // the dispatcher exits 5. The message already says "Failed to
            // parse config (...)". Unknown-key soft-merge warnings never
            // reach here (merge_toml_into_config ignores unknown keys
            // silently), so only a real TOML syntax error trips this.
            return Err(anyhow::anyhow!("{w}"));
        }
        eprintln!("Warning: {w} ({})", path.display());
    }

    if !exists {
        // Write the default config to disk
        if let Some(parent) = path.parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                tracing::warn!(
                    error = %e,
                    dir = %parent.display(),
                    "Failed to create config directory"
                );
            }
        }
        if let Ok(content) = toml::to_string_pretty(&cfg) {
            if std::fs::write(&path, content).is_ok() {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    if let Err(e) =
                        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
                    {
                        tracing::warn!(
                            error = %e,
                            path = %path.display(),
                            "Failed to set restrictive config permissions"
                        );
                    }
                }
                eprintln!("{}", first_run_banner(&path));
                tracing::info!(
                    "Config file created at {}. Edit it to customize model, host, etc.",
                    path.display()
                );
            } else {
                tracing::warn!(path = %path.display(), "Failed to write default config file");
            }
        } else {
            tracing::warn!(path = %path.display(), "Failed to serialize default config");
        }
    }

    Ok(cfg)
}

/// Save config to disk.
///
/// Atomic: writes to a temp file, fsyncs, then renames over the target
/// (WO 38.6). A crash mid-write leaves the original config intact instead
/// of a truncated file that would load as all defaults and permanently
/// erase the user's config on the next save. WO 46.24: the shared
/// `atomic_write` uses O_EXCL + a random tmp name, closing the
/// predictable-`.toml.tmp` symlink race.
pub fn save_config(config: &Config) -> anyhow::Result<()> {
    let path = super::config_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let content = toml::to_string_pretty(config)?;
    crate::tools::atomic_write::atomic_write(&path, content.as_bytes())
        .with_context(|| format!("commit config to {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Err(e) = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)) {
            tracing::warn!(
                error = %e,
                path = %path.display(),
                "Failed to set restrictive config permissions"
            );
        }
    }
    Ok(())
}

/// Resolve the launch-time cwd and assign it to `config.security.sandbox_dir` if
/// the operator hasn't already set one explicitly.
///
/// Review.md arch concern #3: `Config::default()` previously called
/// `std::env::current_dir()` itself, which (a) ran before any
/// validation, and (b) silently dropped sandbox protection if the
/// cwd had been deleted before launch. This helper is the new single
/// resolution site: callers in `main.rs` call it once at startup,
/// freezing the value for the session lifetime.
///
/// Returns the resolved path (as a `String`) on success, or `None`
/// if `current_dir()` failed and we left `sandbox_dir` as `None` —
/// in which case the executor's `warn_if_unsandboxed` banner will
/// surface the situation to the user.
///
/// Honours the explicit-escape-hatch policy: an empty string in
/// `config.security.sandbox_dir` means "intentionally unsandboxed," and we
/// do not overwrite it. Only the `None` case (operator didn't set
/// the field) is filled in.
pub fn freeze_launch_sandbox(config: &mut Config) -> Option<String> {
    if config.security.sandbox_dir.is_some() {
        // Operator already set it (via config file, env var, or
        // an earlier `KF_CODE_SANDBOX_DIR` override). Respect
        // their choice — even if it's an explicit empty string
        // meaning "unsandboxed."
        return config.security.sandbox_dir.clone();
    }
    match std::env::current_dir() {
        Ok(cwd) => {
            let path = cwd.to_string_lossy().to_string();
            config.security.sandbox_dir = Some(path.clone());
            Some(path)
        }
        Err(_) => {
            // `current_dir()` failed (cwd deleted before launch).
            // Leave `sandbox_dir` as `None` so the executor's
            // `warn_if_unsandboxed` banner surfaces the situation.
            // The previous code also fell through to `None` in
            // this case, but did so via the `Default::default()`
            // path; the difference is that NOW the caller knows
            // we tried, and the next test asserts this behaviour
            // explicitly.
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // WO 28.1: moved from shared/access/mod.rs. This test exercises the
    // composition of `freeze_launch_sandbox` (defined here in session::config)
    // with `access_from_config` (now in shared::access). It reaches up to the
    // launch-freeze policy, so it lives with that policy — keeping shared free
    // of any session dependency (required for the kf-shared extraction goal).
    /// **Contract:** `Config::default()` now sandboxes to the current
    /// working directory. Operators who want unsandboxed operation must
    /// explicitly opt out via `sandbox_dir = ""` in the config file (or
    /// `KF_CODE_SANDBOX_DIR=""` env var); `access_from_config` treats
    /// the empty string as `None`.
    #[test]
    fn test_launch_path_sandboxes_to_cwd_by_default() {
        // Default Config has no sandbox_dir — operator didn't set it.
        let mut config = Config::default();
        assert!(
            config.security.sandbox_dir.is_none(),
            "Config::default() must NOT pre-resolve cwd — resolution happens \
             at launch time in `freeze_launch_sandbox`."
        );

        // Launch path. In the unit-test runtime, cwd is always present.
        freeze_launch_sandbox(&mut config);
        let (_deny, guard, _gate) = crate::shared::access::access_from_config(&config);
        assert!(
            guard.is_sandboxed(),
            "After freeze_launch_sandbox, the launch path must produce a \
             sandboxed guard."
        );

        // Explicit escape hatch: empty string in config = unsandboxed.
        let mut config_unsandboxed = Config::default();
        config_unsandboxed.security.sandbox_dir = Some(String::new());
        freeze_launch_sandbox(&mut config_unsandboxed);
        let (_deny, guard_unsandboxed, _gate) =
            crate::shared::access::access_from_config(&config_unsandboxed);
        assert!(
            !guard_unsandboxed.is_sandboxed(),
            "Setting sandbox_dir = Some(\"\") is the explicit escape hatch; \
             freeze_launch_sandbox must not overwrite it"
        );
        assert_eq!(
            config_unsandboxed.security.sandbox_dir.as_deref(),
            Some(""),
            "freeze_launch_sandbox must leave an explicit-empty sandbox_dir alone"
        );

        // `None` is also the escape hatch.
        let mut config_none = Config::default();
        freeze_launch_sandbox(&mut config_none);
        config_none.security.sandbox_dir = None;
        let (_deny, guard_none, _gate) = crate::shared::access::access_from_config(&config_none);
        assert!(
            !guard_none.is_sandboxed(),
            "sandbox_dir = None must produce an unsandboxed guard"
        );
    }

    /// Serialize tests that mutate process-wide environment variables.
    /// Rust unit tests run in parallel by default; `std::env::set_var` is
    /// process-wide, so concurrent env tests can observe each other's state
    /// and fail sporadically.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Helper to temporarily set an env var for a test. Must be called
    /// while `ENV_LOCK` is held.
    fn set_env(key: &str, val: Option<&str>) -> crate::shared::test_util::EnvGuard {
        match val {
            Some(v) => crate::shared::test_util::EnvGuard::set(key, v),
            None => crate::shared::test_util::EnvGuard::remove(key),
        }
    }

    #[test]
    fn test_env_overrides_model() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut cfg = Config::default();
        assert!(
            cfg.model.default_model.is_empty(),
            "default_model is empty by default; configure it explicitly"
        );

        let _env = set_env("KF_CODE_MODEL", Some("deepseek-v4:cloud"));
        apply_env_overrides(&mut cfg);
        assert_eq!(cfg.model.default_model, "deepseek-v4:cloud");
    }

    #[test]
    fn test_env_auto_approve_true() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut cfg = Config::default();
        assert!(!cfg.security.auto_approve);

        let _env = set_env("KF_CODE_AUTO_APPROVE", Some("true"));
        apply_env_overrides(&mut cfg);
        assert!(cfg.security.auto_approve);
    }

    #[test]
    fn test_env_auto_approve_false() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut cfg = Config::default();
        cfg.security.auto_approve = true;

        let _env = set_env("KF_CODE_AUTO_APPROVE", Some("false"));
        apply_env_overrides(&mut cfg);
        assert!(!cfg.security.auto_approve);
    }

    /// Pinning test for WO 43.32: `load_config` (the production path) must
    /// honor `KF_CODE_*` env vars. Before the fix, `apply_env_overrides`
    /// was `#[cfg(test)]`-only and `load_config` never called it, so every
    /// documented env override was silently ignored in the shipped binary.
    /// Uses a temp `KF_CODE_DATA_DIR` with no config.toml so the only layer
    /// above defaults is the env var.
    #[test]
    fn load_config_honors_env_overrides() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());

        let dir = std::env::temp_dir().join(format!(
            "kf_code_env_override_load_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let _data_env = set_env("KF_CODE_DATA_DIR", Some(dir.to_str().unwrap()));
        let _approve_env = set_env("KF_CODE_AUTO_APPROVE", Some("true"));

        assert!(
            !super::super::config_path().exists(),
            "precondition: no config.toml — env is the only override layer"
        );

        let (cfg, _warning) = load_config();
        assert!(
            cfg.security.auto_approve,
            "load_config must apply KF_CODE_AUTO_APPROVE (WO 43.32)"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_env_dry_run_true() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut cfg = Config::default();
        assert!(!cfg.tools.dry_run);

        let _env = set_env("KF_CODE_DRY_RUN", Some("true"));
        apply_env_overrides(&mut cfg);
        assert!(cfg.tools.dry_run);
    }

    #[test]
    fn test_env_dry_run_false() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut cfg = Config::default();
        cfg.tools.dry_run = true;

        let _env = set_env("KF_CODE_DRY_RUN", Some("false"));
        apply_env_overrides(&mut cfg);
        assert!(!cfg.tools.dry_run);
    }

    #[test]
    fn test_env_block_dotfiles() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut cfg = Config::default();
        let _env = set_env("KF_CODE_BLOCK_DOTFILES", Some("true"));
        apply_env_overrides(&mut cfg);
        assert!(cfg.security.block_dotfiles);
    }

    #[test]
    fn test_env_follow_symlinks() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut cfg = Config::default();
        let _env = set_env("KF_CODE_FOLLOW_SYMLINKS", Some("true"));
        apply_env_overrides(&mut cfg);
        assert!(cfg.tools.follow_symlinks);
    }

    #[test]
    fn test_env_block_binary() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut cfg = Config::default();
        let _env = set_env("KF_CODE_BLOCK_BINARY", Some("true"));
        apply_env_overrides(&mut cfg);
        assert!(cfg.tools.block_binary_reads);
    }

    #[test]
    fn test_env_minify_write_side() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut cfg = Config::default();
        assert!(
            cfg.tools.minify_write_side,
            "WO 46.5: serde and Default impl now agree on true"
        );
        let _env = set_env("KF_CODE_MINIFY_WRITE_SIDE", Some("false"));
        apply_env_overrides(&mut cfg);
        assert!(!cfg.tools.minify_write_side);
    }

    #[test]
    fn test_env_minify_above_bytes() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut cfg = Config::default();
        assert_eq!(cfg.tools.minify_above_bytes, 4096);
        let _env = set_env("KF_CODE_MINIFY_ABOVE_BYTES", Some("512"));
        apply_env_overrides(&mut cfg);
        assert_eq!(cfg.tools.minify_above_bytes, 512);
    }

    #[test]
    fn test_env_budget_ceiling() {
        // WO 14.7: KF_CODE_BUDGET_CEILING pins the token budget
        // ceiling for a single run (the Token Budget Challenge sets
        // this per ceiling level). Mirrors KF_CODE_MINIFY_ABOVE_BYTES.
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut cfg = Config::default();
        let default_ceiling = cfg.tools.budget_ceiling;
        {
            let _env = set_env("KF_CODE_BUDGET_CEILING", Some("32768"));
            apply_env_overrides(&mut cfg);
            assert_eq!(cfg.tools.budget_ceiling, 32_768);
        }
        // Confirm removal restores the default (no stale leak). Guard dropped above.
        let mut cfg2 = Config::default();
        apply_env_overrides(&mut cfg2);
        assert_eq!(cfg2.tools.budget_ceiling, default_ceiling);
    }

    #[test]
    fn test_env_budget_ceiling_bad_value_ignored() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut cfg = Config::default();
        let default_ceiling = cfg.tools.budget_ceiling;
        let _env = set_env("KF_CODE_BUDGET_CEILING", Some("not-a-number"));
        apply_env_overrides(&mut cfg);
        assert_eq!(cfg.tools.budget_ceiling, default_ceiling);
    }

    #[test]
    fn test_env_max_read_size() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut cfg = Config::default();
        let _env = set_env("KF_CODE_MAX_READ_SIZE", Some("65536"));
        apply_env_overrides(&mut cfg);
        assert_eq!(cfg.security.max_file_read_size, 65536);
    }

    #[test]
    fn test_env_bad_max_read_size_ignored() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut cfg = Config::default();
        let _env = set_env("KF_CODE_MAX_READ_SIZE", Some("not-a-number"));
        apply_env_overrides(&mut cfg);
        assert_eq!(cfg.security.max_file_read_size, 1024 * 1024);
    }

    #[test]
    fn test_env_misc_overrides() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut cfg = Config::default();

        let _env = set_env("KF_CODE_BANG_REQUIRES_APPROVAL", Some("true"));
        let _e2 = set_env("KF_CODE_JSON_MODE", Some("true"));
        let _e3 = set_env("KF_CODE_BASH_SANDBOX_WORKDIR", Some("false"));
        let _e4 = set_env("KF_CODE_BLOCK_GITIGNORED_DOTFILES", Some("false"));
        let _e5 = set_env("KF_CODE_MAX_OVERWRITE_SIZE", Some("2097152"));
        let _e6 = set_env("KF_CODE_SUMMARIZE_MODEL", Some("my-summarize-model"));
        let _e7 = set_env("KF_CODE_ROUTING_ENABLED", Some("true"));
        let _e8 = set_env("KF_CODE_ROUTER_MODEL", Some("my-router-model"));
        let _e9 = set_env("KF_CODE_COMMIT_MAX_FILE_SIZE", Some("1048576"));
        let _e10 = set_env("KF_CODE_PRESERVE_RECENT_MESSAGES", Some("5"));
        let _e11 = set_env("KF_CODE_MAX_TOOL_CALLS_PER_TURN", Some("25"));
        let _e12 = set_env("KF_CODE_MAX_PERSONA_TURNS", Some("3"));
        let _e13 = set_env("KF_CODE_TOOL_TIMEOUT_SECS", Some("60"));
        let _e14 = set_env("KF_CODE_AUDIT_LOG_PATH", Some("/tmp/kf-audit.ndjson"));
        let _e15 = set_env("KF_CODE_HOOKS_DIR", Some("/tmp/kf-hooks"));

        apply_env_overrides(&mut cfg);

        assert!(cfg.security.bang_requires_approval);
        assert!(cfg.model.json_mode);
        assert!(!cfg.security.bash_sandbox_workdir);
        assert!(!cfg.security.block_gitignored_dotfiles);
        assert_eq!(cfg.security.max_overwrite_size, 2_097_152);
        assert_eq!(cfg.model.summarize_model, "my-summarize-model");
        assert!(cfg.model.routing_enabled);
        assert_eq!(cfg.model.router_model, "my-router-model");
        assert_eq!(cfg.security.commit_max_file_size, 1_048_576);
        assert_eq!(cfg.session.preserve_recent_messages, 5);
        assert_eq!(cfg.tools.max_tool_calls_per_turn, 25);
        assert_eq!(cfg.tools.max_persona_turns, 3);
        assert_eq!(cfg.tools.tool_timeout_secs, Some(60));
        assert_eq!(
            cfg.security.audit_log_path,
            Some(PathBuf::from("/tmp/kf-audit.ndjson"))
        );
        assert_eq!(cfg.tools.hooks_dir, Some(PathBuf::from("/tmp/kf-hooks")));
    }

    #[test]
    fn test_env_tool_timeout_secs_is_clamped() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut cfg = Config::default();

        let _env = set_env("KF_CODE_TOOL_TIMEOUT_SECS", Some("0"));
        apply_env_overrides(&mut cfg);
        assert_eq!(cfg.tools.tool_timeout_secs, Some(1));

        let _e2 = set_env("KF_CODE_TOOL_TIMEOUT_SECS", Some("7200"));
        apply_env_overrides(&mut cfg);
        assert_eq!(cfg.tools.tool_timeout_secs, Some(3600));
    }

    /// `freeze_launch_sandbox` is the new launch-time cwd resolution
    /// site. It must fill in `sandbox_dir` with the resolved cwd when
    /// the operator hasn't set it explicitly, and must not overwrite
    /// an explicit (including intentionally-empty) value.
    ///
    /// Review.md arch concern #3: the previous code did this in
    /// `Config::default()`, which (a) ran before any validation and
    /// (b) silently dropped sandbox protection on a `current_dir()`
    /// failure. The new helper is a single, testable call site.
    #[test]
    fn test_freeze_launch_sandbox_fills_in_cwd() {
        let mut cfg = Config::default();
        assert!(cfg.security.sandbox_dir.is_none());
        let resolved = freeze_launch_sandbox(&mut cfg);
        // The test runner always has a cwd.
        assert!(resolved.is_some(), "test cwd is always present");
        let resolved = resolved.unwrap();
        assert_eq!(cfg.security.sandbox_dir.as_deref(), Some(resolved.as_str()));
    }

    /// The explicit-escape-hatch contract: if the operator set
    /// `sandbox_dir = Some("")` (or it was loaded from a config
    /// file that way), `freeze_launch_sandbox` must leave it alone.
    /// This is the policy that lets operators opt out of sandboxing.
    #[test]
    fn test_freeze_launch_sandbox_does_not_overwrite_explicit_empty() {
        let mut cfg = Config::default();
        cfg.security.sandbox_dir = Some(String::new());
        let resolved = freeze_launch_sandbox(&mut cfg);
        assert_eq!(resolved.as_deref(), Some(""));
        assert_eq!(cfg.security.sandbox_dir.as_deref(), Some(""));
    }

    /// If the operator set a real path (e.g. from a config file's
    /// `sandbox_dir = "/srv/project"`), the helper must not
    /// overwrite it with cwd. Operators win over defaults.
    #[test]
    fn test_freeze_launch_sandbox_does_not_overwrite_explicit_path() {
        let mut cfg = Config::default();
        cfg.security.sandbox_dir = Some("/srv/project".to_string());
        let resolved = freeze_launch_sandbox(&mut cfg);
        assert_eq!(resolved.as_deref(), Some("/srv/project"));
        assert_eq!(cfg.security.sandbox_dir.as_deref(), Some("/srv/project"));
    }

    #[test]
    fn test_env_reject_on_excess_plugin_trust() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut cfg = Config::default();
        assert!(cfg.tools.reject_on_excess_plugin_trust);

        let _env = set_env("KF_CODE_REJECT_ON_EXCESS_PLUGIN_TRUST", Some("false"));
        apply_env_overrides(&mut cfg);
        assert!(!cfg.tools.reject_on_excess_plugin_trust);
    }

    #[test]
    fn test_env_plugin_signature_validation() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut cfg = Config::default();
        assert!(cfg.tools.plugin_signature_validation);

        let _env = set_env("KF_CODE_PLUGIN_SIGNATURE_VALIDATION", Some("false"));
        apply_env_overrides(&mut cfg);
        assert!(!cfg.tools.plugin_signature_validation);
    }

    #[test]
    fn test_env_plugin_public_key_path() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut cfg = Config::default();
        let _env = set_env("KF_CODE_PLUGIN_PUBLIC_KEY_PATH", Some("/tmp/key.pub"));
        apply_env_overrides(&mut cfg);
        assert_eq!(
            cfg.tools.plugin_public_key_path.as_deref(),
            Some("/tmp/key.pub")
        );
    }

    #[test]
    fn test_env_plugin_allowed_env_vars() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut cfg = Config::default();
        let _env = set_env("KF_CODE_PLUGIN_ALLOWED_ENV_VARS", Some("FOO,BAR"));
        apply_env_overrides(&mut cfg);
        assert_eq!(cfg.tools.plugin_allowed_env_vars, vec!["FOO", "BAR"]);
    }

    #[test]
    fn test_env_memory_enabled() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut cfg = Config::default();
        assert!(cfg.display.memory_enabled);

        let _env = set_env("KF_CODE_MEMORY_ENABLED", Some("false"));
        apply_env_overrides(&mut cfg);
        assert!(!cfg.display.memory_enabled);
    }

    #[test]
    fn test_env_memory_max_tokens() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut cfg = Config::default();
        let _env = set_env("KF_CODE_MEMORY_MAX_TOKENS", Some("250"));
        apply_env_overrides(&mut cfg);
        assert_eq!(cfg.display.memory_max_tokens, 250);
    }

    #[test]
    fn test_env_memory_top_n() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut cfg = Config::default();
        let _env = set_env("KF_CODE_MEMORY_TOP_N", Some("5"));
        apply_env_overrides(&mut cfg);
        assert_eq!(cfg.display.memory_top_n, 5);
    }

    #[test]
    fn test_env_memory_auto_populate() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut cfg = Config::default();
        assert!(cfg.display.memory_auto_populate);
        let _env = set_env("KF_CODE_MEMORY_AUTO_POPULATE", Some("false"));
        apply_env_overrides(&mut cfg);
        assert!(!cfg.display.memory_auto_populate);
    }

    #[test]
    fn test_env_memory_show_in_status() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut cfg = Config::default();
        assert!(cfg.display.memory_show_in_status);
        let _env = set_env("KF_CODE_MEMORY_SHOW_IN_STATUS", Some("false"));
        apply_env_overrides(&mut cfg);
        assert!(!cfg.display.memory_show_in_status);
    }

    #[test]
    fn test_env_checkpoint_interval_messages() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut cfg = Config::default();
        let _env = set_env("KF_CODE_CHECKPOINT_INTERVAL_MESSAGES", Some("20"));
        apply_env_overrides(&mut cfg);
        assert_eq!(cfg.session.checkpoint_interval_messages, 20);
    }

    #[test]
    fn parse_bool_env_recognizes_true_and_false_variants() {
        assert_eq!(parse_bool_env("true"), Some(true));
        assert_eq!(parse_bool_env("True"), Some(true));
        assert_eq!(parse_bool_env("1"), Some(true));
        assert_eq!(parse_bool_env("yes"), Some(true));
        assert_eq!(parse_bool_env("false"), Some(false));
        assert_eq!(parse_bool_env("False"), Some(false));
        assert_eq!(parse_bool_env("0"), Some(false));
        assert_eq!(parse_bool_env("no"), Some(false));
        assert_eq!(parse_bool_env("maybe"), None);
        assert_eq!(parse_bool_env(""), None);
    }

    #[test]
    fn test_env_anthropic_cloud_and_computer_use() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut cfg = Config::default();

        let _env = set_env("KF_CODE_ANTHROPIC_PROVIDER", Some("vertex"));
        let _e2 = set_env("KF_CODE_AWS_REGION", Some("eu-west-1"));
        let _e3 = set_env("KF_CODE_GCP_PROJECT_ID", Some("p2"));
        let _e4 = set_env("KF_CODE_GCP_REGION", Some("europe-west1"));
        let _e5 = set_env("KF_CODE_GCP_SERVICE_ACCOUNT_PATH", Some("/tmp/p2.json"));
        let _e6 = set_env("KF_CODE_COMPUTER_USE_ENABLED", Some("true"));
        let _e7 = set_env("KF_CODE_COMPUTER_USE_WIDTH", Some("1366"));
        let _e8 = set_env("KF_CODE_COMPUTER_USE_HEIGHT", Some("768"));
        let _e9 = set_env("KF_CODE_COMPUTER_USE_STARTUP_TIMEOUT", Some("60"));
        let _e10 = set_env("KF_CODE_COMPUTER_USE_WAIT_TIMEOUT", Some("20"));
        let _e11 = set_env(
            "KF_CODE_ANTHROPIC_API_BASE",
            Some("https://proxy.example.com"),
        );

        apply_env_overrides(&mut cfg);

        assert_eq!(cfg.model.anthropic_provider, "vertex");
        assert_eq!(cfg.model.anthropic_api_base, "https://proxy.example.com");
        assert_eq!(cfg.model.aws_region, "eu-west-1");
        assert_eq!(cfg.model.gcp_project_id, "p2");
        assert_eq!(cfg.model.gcp_region, "europe-west1");
        assert_eq!(
            cfg.model.gcp_service_account_path,
            Some(PathBuf::from("/tmp/p2.json"))
        );
        assert!(cfg.security.computer_use.enabled);
        assert_eq!(cfg.security.computer_use.width, 1366);
        assert_eq!(cfg.security.computer_use.height, 768);
        assert_eq!(cfg.security.computer_use.startup_timeout_secs, 60);
        assert_eq!(cfg.security.computer_use.wait_timeout_secs, 20);
    }

    #[test]
    fn test_env_scheduled_bash_auto_approve() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut cfg = Config::default();
        assert!(!cfg.tools.scheduled_bash_auto_approve);
        let _env = set_env("KF_CODE_SCHEDULED_BASH_AUTO_APPROVE", Some("true"));
        apply_env_overrides(&mut cfg);
        assert!(cfg.tools.scheduled_bash_auto_approve);
    }

    #[test]
    fn test_env_max_concurrent_scheduled_jobs_is_clamped() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut cfg = Config::default();
        let _env = set_env("KF_CODE_MAX_CONCURRENT_SCHEDULED_JOBS", Some("0"));
        apply_env_overrides(&mut cfg);
        assert_eq!(cfg.tools.max_concurrent_scheduled_jobs, 1);
        let _e2 = set_env("KF_CODE_MAX_CONCURRENT_SCHEDULED_JOBS", Some("8"));
        apply_env_overrides(&mut cfg);
        assert_eq!(cfg.tools.max_concurrent_scheduled_jobs, 8);
    }

    #[test]
    fn test_env_max_background_tasks_is_clamped() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut cfg = Config::default();
        let _env = set_env("KF_CODE_MAX_BACKGROUND_TASKS", Some("0"));
        apply_env_overrides(&mut cfg);
        assert_eq!(cfg.tools.max_background_tasks, 1);
        let _e2 = set_env("KF_CODE_MAX_BACKGROUND_TASKS", Some("16"));
        apply_env_overrides(&mut cfg);
        assert_eq!(cfg.tools.max_background_tasks, 16);
        let _e3 = set_env("KF_CODE_MAX_BACKGROUND_TASKS", Some("100"));
        apply_env_overrides(&mut cfg);
        assert_eq!(cfg.tools.max_background_tasks, 64);
    }

    #[test]
    fn test_env_task_concurrency_mode() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut cfg = Config::default();
        assert_eq!(cfg.tools.task_concurrency_mode, "queue");
        let _env = set_env("KF_CODE_TASK_CONCURRENCY_MODE", Some("reject"));
        apply_env_overrides(&mut cfg);
        assert_eq!(cfg.tools.task_concurrency_mode, "reject");
        let _e2 = set_env("KF_CODE_TASK_CONCURRENCY_MODE", Some("QUEUE"));
        apply_env_overrides(&mut cfg);
        assert_eq!(cfg.tools.task_concurrency_mode, "queue");
        let _e3 = set_env("KF_CODE_TASK_CONCURRENCY_MODE", Some("invalid"));
        apply_env_overrides(&mut cfg);
        assert_eq!(
            cfg.tools.task_concurrency_mode, "queue",
            "invalid value should not change mode"
        );
    }

    #[test]
    fn test_env_request_timeout_override_and_clamp() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut cfg = Config::default();
        let _env = set_env("KF_CODE_REQUEST_TIMEOUT_SECS", Some("0"));
        apply_env_overrides(&mut cfg);
        assert_eq!(
            cfg.model.request_timeout_secs, 1,
            "env zero timeout must be clamped"
        );

        let _e2 = set_env("KF_CODE_REQUEST_TIMEOUT_SECS", Some("45"));
        apply_env_overrides(&mut cfg);
        assert_eq!(cfg.model.request_timeout_secs, 45);
    }

    #[test]
    fn test_expand_tilde_str_empty_string() {
        assert_eq!(expand_tilde_str(""), "");
    }

    #[test]
    fn test_expand_tilde_str_no_tilde() {
        assert_eq!(expand_tilde_str("/usr/local/bin"), "/usr/local/bin");
    }

    #[test]
    fn test_expand_tilde_str_expands_home() {
        let expanded = expand_tilde_str("~/projects");
        assert!(
            !expanded.starts_with('~'),
            "tilde should be expanded, got: {expanded}"
        );
    }

    #[test]
    fn test_expand_tilde_str_preserves_trailing_path() {
        let expanded = expand_tilde_str("~/a/b/c");
        assert!(expanded.ends_with("/a/b/c"), "got: {expanded}");
    }

    // First-run onboarding banner (WO 14.1). The banner text must name
    // the config path and give a concrete `-m` hint so a new user knows
    // what to do next. `first_run_banner` is the pure helper that
    // `load_or_create_config` prints to stdout on first run.
    #[test]
    fn test_env_adapter_routing_override() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut cfg = Config::default();
        assert!(
            cfg.model.adapter_routing.is_empty(),
            "adapter_routing defaults to empty"
        );

        let _env = set_env(
            "KF_CODE_ADAPTER_ROUTING",
            Some("grok-=OpenAiCompat,my-llm=Ollama"),
        );
        apply_env_overrides(&mut cfg);
        assert_eq!(cfg.model.adapter_routing.len(), 2);
        assert_eq!(
            cfg.model.adapter_routing.get("grok-"),
            Some(&"OpenAiCompat".to_string())
        );
        assert_eq!(
            cfg.model.adapter_routing.get("my-llm"),
            Some(&"Ollama".to_string())
        );
    }

    #[test]
    fn test_env_adapter_routing_empty_value_is_ignored() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut cfg = Config::default();

        let _env = set_env("KF_CODE_ADAPTER_ROUTING", Some(""));
        apply_env_overrides(&mut cfg);
        assert!(
            cfg.model.adapter_routing.is_empty(),
            "empty env value should not populate adapter_routing"
        );
    }

    /// WO 47.2: the env loader derives config keys from var names, so an
    /// unknown `KF_CODE_*` var must be a no-op (serde drops the unknown
    /// key on the overlay decode), never a load failure or a phantom
    /// field.
    #[test]
    fn test_env_unknown_var_is_ignored() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut cfg = Config::default();
        let _env = set_env("KF_CODE_NO_SUCH_KNOB", Some("42"));
        apply_env_overrides(&mut cfg);
        assert!(
            cfg.model.default_model.is_empty(),
            "unknown var must not leak into any field"
        );
        assert_eq!(cfg.model.request_timeout_secs, 120);
    }

    /// WO 47.2: derived vars make previously env-less fields settable —
    /// a `Vec<String>` field takes a comma-separated list. Pin the
    /// derivation with deny_urls, which had no env var before.
    #[test]
    fn test_env_derived_list_var() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut cfg = Config::default();
        let _env = set_env(
            "KF_CODE_DENY_URLS",
            Some("https://a.example, https://b.example"),
        );
        apply_env_overrides(&mut cfg);
        assert_eq!(
            cfg.security.deny_urls,
            vec!["https://a.example", "https://b.example"]
        );
    }

    /// WO 47.2: `KF_CODE_COMPACTION_USE_HEURISTIC` (new name) wins over
    /// the legacy `KF_CODE_COMPACTION_USE_LLM` when both are set — the
    /// precedence must not depend on env iteration order.
    #[test]
    fn test_env_compaction_alias_new_name_wins() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut cfg = Config::default();
        let _legacy = set_env("KF_CODE_COMPACTION_USE_LLM", Some("false"));
        let _new = set_env("KF_CODE_COMPACTION_USE_HEURISTIC", Some("true"));
        apply_env_overrides(&mut cfg);
        assert!(cfg.session.compaction_use_heuristic);

        let mut cfg2 = Config::default();
        let _legacy_only = set_env("KF_CODE_COMPACTION_USE_HEURISTIC", None);
        apply_env_overrides(&mut cfg2);
        assert!(
            !cfg2.session.compaction_use_heuristic,
            "legacy var alone still works"
        );
    }

    /// WO 47.2: `KF_CODE_SANDBOX_DIR=""` is the documented unsandboxed
    /// opt-out (matches `sandbox_dir = ""` in the config file and the
    /// WO 28.1 contract: "Operators who want unsandboxed operation must
    /// explicitly opt out via ... `KF_CODE_SANDBOX_DIR=\"\"` env var").
    /// The old hand-parsed loader mapped "" to `None`, which
    /// `freeze_launch_sandbox` silently re-sandboxed to cwd — the env
    /// opt-out was broken. It now lands as `Some("")`.
    #[test]
    fn test_env_sandbox_dir_empty_is_explicit_opt_out() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut cfg = Config::default();
        let _env = set_env("KF_CODE_SANDBOX_DIR", Some(""));
        apply_env_overrides(&mut cfg);
        assert_eq!(
            cfg.security.sandbox_dir.as_deref(),
            Some(""),
            "empty KF_CODE_SANDBOX_DIR is the documented unsandboxed opt-out"
        );
    }

    /// WO 47.2: the env layer overlays nested tables by deep merge — an
    /// env var for `computer_use.width` must not wipe a file-layer
    /// `computer_use.headful` (the overlay serializes the current
    /// config, merges the key in, and decodes back).
    #[test]
    fn env_computer_use_merges_with_file_layer() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut cfg = Config::default();
        // simulate the file layer having set headful
        cfg.security.computer_use.headful = true;

        let _env = set_env("KF_CODE_COMPUTER_USE_WIDTH", Some("1366"));
        apply_env_overrides(&mut cfg);
        assert!(
            cfg.security.computer_use.headful,
            "file-layer sibling survives"
        );
        assert_eq!(cfg.security.computer_use.width, 1366);
    }

    #[test]
    fn first_run_banner_printed_to_stderr() {
        let path = std::path::PathBuf::from("/tmp/kf-code/config.toml");
        let banner = first_run_banner(&path);
        // The banner is what `load_or_create_config` prints via `eprintln!`
        // on a first run (no config file present). Routed to stderr (WO
        // 38.10) so `--output stream-json` on a fresh data dir keeps stdout
        // byte-clean. Asserting on the helper output avoids fragile
        // in-process stdout capture while pinning the exact user-visible
        // wording.
        assert!(banner.contains("Config created"), "got: {banner}");
        assert!(banner.contains("/tmp/kf-code/config.toml"), "got: {banner}");
        assert!(banner.contains("-m qwen2.5:0.5b"), "got: {banner}");
    }

    // The banner must fire exactly once: a second `load_or_create_config`
    // with the file already present must NOT re-print. The gating
    // condition in `load_or_create_config` is `!exists`, so once the
    // first call writes the file, the banner branch is skipped. This
    // uses a process-unique temp `KF_CODE_DATA_DIR` per WO 10.6 so
    // the first-run detection is deterministic and does not race with
    // other config tests.
    #[test]
    fn first_run_banner_silent_on_second_run() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());

        let dir = std::env::temp_dir().join(format!("kf_code_first_run_{}_0", std::process::id(),));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let _env =
            crate::shared::test_util::EnvGuard::set("KF_CODE_DATA_DIR", dir.to_str().unwrap());
        let config_path = super::super::config_path();
        assert!(!config_path.exists(), "precondition: no config yet");

        // First run creates the file (banner would print).
        let _cfg = load_or_create_config();
        assert!(config_path.exists(), "first run created the config file");

        // Second run: the file now exists, so `!exists` is false and the
        // banner branch in `load_or_create_config` is skipped. We assert
        // the gating condition holds — the file is present before the
        // second call, which is exactly what suppresses the banner.
        assert!(
            config_path.exists(),
            "second run must see the file present (banner suppressed)"
        );
        let _cfg2 = load_or_create_config();

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// WO 38.10: a hard TOML parse failure in an existing config.toml must
    /// surface as `Err` from the strict variant (→ exit 5 in the
    /// dispatcher) instead of silently falling back to defaults. The
    /// lenient `load_or_create_config` keeps the historical warn+defaults
    /// behaviour. Unknown-key soft-merge warnings are NOT errors — only a
    /// real TOML syntax error trips the strict path.
    #[test]
    fn strict_config_load_fails_on_hard_parse_error() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());

        let dir = std::env::temp_dir().join(format!(
            "kf_code_strict_parse_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let _env =
            crate::shared::test_util::EnvGuard::set("KF_CODE_DATA_DIR", dir.to_str().unwrap());
        let config_path = super::super::config_path();

        // First run: writes a valid default config (no error).
        let _cfg = load_or_create_config_strict().expect("first run must succeed");
        assert!(config_path.exists(), "first run created the config file");

        // Corrupt the config with a TOML syntax error (unterminated
        // string / bad table). This is a hard parse failure, not an
        // unknown-key warning.
        std::fs::write(&config_path, "default_model = \"qwen\n[bad =").unwrap();

        // Strict variant must return Err so the dispatcher exits 5.
        let res = load_or_create_config_strict();
        assert!(
            res.is_err(),
            "strict load must fail on a hard TOML parse error, got Ok"
        );
        let err = res.unwrap_err().to_string();
        assert!(
            err.contains("Failed to parse config"),
            "strict load error must mention the parse failure, got: {err}"
        );

        // Lenient variant keeps the historical behaviour: warn + defaults.
        let cfg = load_or_create_config();
        assert!(
            cfg.model.default_model.is_empty(),
            "lenient load falls back to defaults on parse error"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// WO 38.10: unknown keys in an otherwise-valid config are a soft
    /// merge (ignored), NOT a hard parse error. The strict variant must
    /// return `Ok` for an unknown-key config — only a real TOML syntax
    /// error trips exit 5.
    #[test]
    fn strict_config_load_ignores_unknown_keys() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());

        let dir = std::env::temp_dir().join(format!(
            "kf_code_strict_unknown_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let _env =
            crate::shared::test_util::EnvGuard::set("KF_CODE_DATA_DIR", dir.to_str().unwrap());
        let config_path = super::super::config_path();

        // First run: writes a valid default config.
        let _cfg = load_or_create_config_strict().expect("first run must succeed");
        assert!(config_path.exists());

        // Write a valid TOML file with an unknown key. This parses fine
        // (merge_toml_into_config ignores unknown keys), so the strict
        // variant must NOT error.
        std::fs::write(
            &config_path,
            "default_model = \"qwen2.5:0.5b\"\nunknown_key = 42\n",
        )
        .unwrap();

        let cfg = load_or_create_config_strict().expect(
            "strict load must succeed for a valid TOML file with unknown keys (soft merge)",
        );
        assert_eq!(cfg.model.default_model, "qwen2.5:0.5b");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Drift-guard: when a new field is added to any Config sub-struct
    /// (ModelConfig, SecurityConfig, ToolConfig, SessionConfig, DisplayConfig)
    /// the author must update `CONFIG_FIELD_COUNT` in `shared::config`.
    ///
    /// WO 47.2: the file/env override layers are a generic serde overlay,
    /// so a new field is reachable from config.toml and `KF_CODE_<FIELD>`
    /// automatically — no per-field loader edits, no literal tables here.
    /// The remaining drift surface is `env_overrides::KEY_MAP` (irregular
    /// var names): check 2 pins that every KEY_MAP path resolves against
    /// the serialized config, and check 3 that every env-var literal in
    /// the loader is accounted for.
    //
    // WO 27.2-R2: un-ignored after recomputing the expected literals.
    // The const itself had drifted (+5 over the real struct count) and
    // the env-var assertion was tautological (`assert_eq!(80, 80)`).
    #[test]
    fn config_field_count_drift_guard() {
        use crate::shared::config::CONFIG_FIELD_COUNT;

        // ── 1. Total struct-level fields ──────────────────────────
        // ModelConfig=34 (33 direct + subagent_provider sub-struct handle),
        // SecurityConfig=22, ToolConfig=35, SessionConfig=8,
        // DisplayConfig=8 → 108 total pub fields.
        // (WO 45.37: SessionConfig 9 → 8 — worktree_enabled +
        // auto_apply_patch replaced by artifact_policy enum.)
        // (WO 47.13: DisplayConfig 7 → 8 — added extra_commands.)
        assert_eq!(
            CONFIG_FIELD_COUNT, 108,
            "CONFIG_FIELD_COUNT has drifted — did you add/remove a config field?"
        );

<<<<<<< HEAD
        // ── 2. KEY_MAP path integrity ─────────────────────────────
        // WO 47.2: the file and env layers are a generic serde overlay
        // (merge.rs + env_overrides.rs), so every Config field is
        // reachable from both layers by construction — the TOML/env
        // literal-count tripwires this test used to carry are gone.
        // What CAN drift now is KEY_MAP (the env vars whose config key
        // isn't derivable from the var name): every intermediate path
        // segment must resolve to a table in the serialized default
        // config, and every leaf must be a known key — present in the
        // serialized form, or an Option that serializes to nothing
        // (tracked in ABSENT_KEY_MAP_LEAVES).
        let serialized =
            toml::Value::try_from(Config::default()).expect("serialize default config");
        let serialized_table = serialized.as_table().expect("flat top-level table");
        const ABSENT_KEY_MAP_LEAVES: &[&str] = &[
            "computer_use.chrome_path",
            "subagent_provider.model",
            "subagent_provider.ollama_host",
            "subagent_provider.anthropic_api_key",
            "subagent_provider.openai_api_key",
            "subagent_provider.deepseek_api_key",
            "subagent_provider.gemini_api_key",
            "subagent_provider.kimi_api_key",
            "anthropic_api_key",
            "openai_api_key",
            "deepseek_api_key",
            "gemini_api_key",
            "kimi_api_key",
        ];
        for (_, key) in env_overrides::KEY_MAP {
            let mut node = serialized_table;
            let mut segments = key.split('.');
            let last = segments.next_back().unwrap();
            for seg in segments {
                node = node
                    .get(seg)
                    .and_then(|v| v.as_table())
                    .unwrap_or_else(|| panic!("KEY_MAP path {key}: segment {seg} is not a table"));
||||||| 15ad6877
        // ── 2. merge_toml_into_config field coverage ──────────────
        // Build a TOML table with every key that merge_toml_into_config
        // processes and count the entries. The number must stay in sync
        // with the function body.
        let merge_toml_source = r#"
            default_model = "x"
            ollama_host = "x"
            auto_approve = true
            sandbox_dir = "x"
            block_dotfiles = true
            max_file_read_size = 999
            request_timeout_secs = 999
            streaming_timeout_secs = 999
            follow_symlinks = true
            block_binary_reads = true
            minify_write_side = true
            minify_above_bytes = 999
            scheduled_bash_auto_approve = true
            max_concurrent_scheduled_jobs = 999
            carryover_enabled = true
            compaction_use_heuristic = true
            compaction_use_llm = true
            compaction_drop_threshold = 0.5
            stem_file_cap = 999
            shutdown_timeout_secs = 999
            dry_run = true
            cache_enabled = true
            cache_dir = "x"
            bang_requires_approval = true
            json_mode = true
            extended_thinking = true
            budget_tokens = 999
            max_tokens = 999
            bash_sandbox_workdir = true
            bash_require_allowlist = true
            bash_allowlist = ["ls", "echo"]
            block_gitignored_dotfiles = true
            max_overwrite_size = 999
            summarize_model = "x"
            routing_enabled = true
            router_model = "x"
            commit_max_file_size = 999
            preserve_recent_messages = 999
            max_tool_calls_per_turn = 999
            max_persona_turns = 999
            max_continuation_rounds = 5
            max_background_tasks = 4
            task_concurrency_mode = "queue"
            doom_loop_max_hits = 3
            doom_loop_action = "x"
            load_project_mcp_json = false
            plugin_consent_ledger = true
            tool_timeout_secs = 999
            audit_log_path = "x"
            diff_review = false
            hooks_dir = "x"
            reject_on_excess_plugin_trust = true
            plugin_signature_validation = true
            plugin_trust_workspace = true
            plugin_public_key_path = "x"
            memory_enabled = true
            memory_max_tokens = 999
            memory_top_n = 999
            memory_auto_populate = true
            memory_show_in_status = true
            theme = "x"
            mouse_enabled = true
            checkpoint_interval_messages = 999
            anthropic_provider = "x"
            anthropic_api_base = "x"
            aws_region = "x"
            gcp_project_id = "x"
            gcp_region = "x"
            gcp_service_account_path = "x"
            anthropic_api_key = "x"
            openai_api_key = "x"
            deepseek_api_key = "x"
            gemini_api_key = "x"
            kimi_api_key = "x"
            deny_paths = ["/x"]
            deny_urls = ["x"]
            deny_extensions = [".x"]
            allowed_write_dirs = ["/x"]
            landlock_extra_paths = ["/x"]
            plugin_allowed_env_vars = ["x"]
            plugin_sources = { x = "/x" }
            enabled_plugins = ["x"]
            disabled_plugins = ["x"]
            routing_model_map = { x = "x" }
            adapter_routing = { x = "x" }

            [computer_use]
            enabled = true
            chrome_path = "x"
            headful = true
            width = 999
            height = 999
            startup_timeout_secs = 999
            wait_timeout_secs = 999
            hosted = true

            [subagent_provider]
            model = "x"
            ollama_host = "x"
            anthropic_api_key = "x"
            openai_api_key = "x"
            deepseek_api_key = "x"
            gemini_api_key = "x"
            kimi_api_key = "x"
        "#;

        let table: toml::Table = merge_toml_source.parse().unwrap();
        let mut toml_key_count: usize = 0;
        for (_key, value) in &table {
            if let Some(sub) = value.as_table() {
                toml_key_count += sub.len();
            } else {
                toml_key_count += 1;
=======
        // ── 2. merge_toml_into_config field coverage ──────────────
        // Build a TOML table with every key that merge_toml_into_config
        // processes and count the entries. The number must stay in sync
        // with the function body.
        let merge_toml_source = r#"
            default_model = "x"
            ollama_host = "x"
            auto_approve = true
            sandbox_dir = "x"
            block_dotfiles = true
            max_file_read_size = 999
            request_timeout_secs = 999
            streaming_timeout_secs = 999
            follow_symlinks = true
            block_binary_reads = true
            minify_write_side = true
            minify_above_bytes = 999
            scheduled_bash_auto_approve = true
            max_concurrent_scheduled_jobs = 999
            carryover_enabled = true
            compaction_use_heuristic = true
            compaction_use_llm = true
            compaction_drop_threshold = 0.5
            stem_file_cap = 999
            shutdown_timeout_secs = 999
            dry_run = true
            cache_enabled = true
            cache_dir = "x"
            bang_requires_approval = true
            json_mode = true
            extended_thinking = true
            budget_tokens = 999
            max_tokens = 999
            bash_sandbox_workdir = true
            bash_require_allowlist = true
            bash_allowlist = ["ls", "echo"]
            block_gitignored_dotfiles = true
            max_overwrite_size = 999
            summarize_model = "x"
            routing_enabled = true
            router_model = "x"
            commit_max_file_size = 999
            preserve_recent_messages = 999
            max_tool_calls_per_turn = 999
            max_persona_turns = 999
            max_continuation_rounds = 5
            max_background_tasks = 4
            task_concurrency_mode = "queue"
            doom_loop_max_hits = 3
            doom_loop_action = "x"
            load_project_mcp_json = false
            plugin_consent_ledger = true
            tool_timeout_secs = 999
            audit_log_path = "x"
            diff_review = false
            hooks_dir = "x"
            reject_on_excess_plugin_trust = true
            plugin_signature_validation = true
            plugin_trust_workspace = true
            plugin_public_key_path = "x"
            memory_enabled = true
            memory_max_tokens = 999
            memory_top_n = 999
            memory_auto_populate = true
            memory_show_in_status = true
            theme = "x"
            mouse_enabled = true
            extra_commands = ["gh"]
            checkpoint_interval_messages = 999
            anthropic_provider = "x"
            anthropic_api_base = "x"
            aws_region = "x"
            gcp_project_id = "x"
            gcp_region = "x"
            gcp_service_account_path = "x"
            anthropic_api_key = "x"
            openai_api_key = "x"
            deepseek_api_key = "x"
            gemini_api_key = "x"
            kimi_api_key = "x"
            deny_paths = ["/x"]
            deny_urls = ["x"]
            deny_extensions = [".x"]
            allowed_write_dirs = ["/x"]
            landlock_extra_paths = ["/x"]
            plugin_allowed_env_vars = ["x"]
            plugin_sources = { x = "/x" }
            enabled_plugins = ["x"]
            disabled_plugins = ["x"]
            routing_model_map = { x = "x" }
            adapter_routing = { x = "x" }

            [computer_use]
            enabled = true
            chrome_path = "x"
            headful = true
            width = 999
            height = 999
            startup_timeout_secs = 999
            wait_timeout_secs = 999
            hosted = true

            [subagent_provider]
            model = "x"
            ollama_host = "x"
            anthropic_api_key = "x"
            openai_api_key = "x"
            deepseek_api_key = "x"
            gemini_api_key = "x"
            kimi_api_key = "x"
        "#;

        let table: toml::Table = merge_toml_source.parse().unwrap();
        let mut toml_key_count: usize = 0;
        for (_key, value) in &table {
            if let Some(sub) = value.as_table() {
                toml_key_count += sub.len();
            } else {
                toml_key_count += 1;
>>>>>>> wo/wo47.13
            }
            assert!(
                node.contains_key(last) || ABSENT_KEY_MAP_LEAVES.contains(key),
                "KEY_MAP leaf {key} is not a serialized config key — typo, or a new \
                 Option leaf that must be tracked in ABSENT_KEY_MAP_LEAVES"
            );
        }
<<<<<<< HEAD
||||||| 15ad6877
        // 70 top-level leaf keys + 9 array keys + 3 single-key inline
        // tables + 8 computer_use sub-keys + 7 subagent_provider sub-keys = 97
        // WO 39.2: +1 (load_project_mcp_json) = 98
        // WO 43.17: +1 (plugin_consent_ledger) = 99
        // WO 44.22: +1 (anthropic_api_base) = 100
        const MERGE_TOML_EXPECTED: usize = 100;
        assert_eq!(
            toml_key_count, MERGE_TOML_EXPECTED,
            "merge_toml_into_config key count changed — did you add/remove a handled field?"
        );
=======
        // 70 top-level leaf keys + 9 array keys + 3 single-key inline
        // tables + 8 computer_use sub-keys + 7 subagent_provider sub-keys = 97
        // WO 39.2: +1 (load_project_mcp_json) = 98
        // WO 43.17: +1 (plugin_consent_ledger) = 99
        // WO 44.22: +1 (anthropic_api_base) = 100
        // WO 47.13: +1 (extra_commands) = 101
        const MERGE_TOML_EXPECTED: usize = 101;
        assert_eq!(
            toml_key_count, MERGE_TOML_EXPECTED,
            "merge_toml_into_config key count changed — did you add/remove a handled field?"
        );
>>>>>>> wo/wo47.13

        // ── 3. env_overrides literal tripwire ─────────────────────
        // Every env-var string literal in env_overrides.rs must be
        // accounted for: KEY_MAP entries (19 KF_CODE_* + 5 provider
        // keys) + the prefix strip (1) + the compaction fixup (1) +
        // the three post-block vars (exclusion in config_key + the
        // post-block reads = 6) + the custom-value path-field arm (7)
        // + colon lists (2) + the compaction legacy alias (1) =
        // KEY_MAP + 18 KF_CODE_* literals. Bumping this number means
        // env coverage changed — do it deliberately and document why
        // in the WO.
        let env_overrides_src = include_str!("env_overrides.rs");
        let env_var_count = env_overrides_src.matches("\"KF_CODE_").count()
            + env_overrides_src.matches("\"ANTHROPIC_API_KEY\"").count()
            + env_overrides_src.matches("\"OPENAI_API_KEY\"").count()
            + env_overrides_src.matches("\"DEEPSEEK_API_KEY\"").count()
            + env_overrides_src.matches("\"GEMINI_API_KEY\"").count()
            + env_overrides_src.matches("\"KIMI_API_KEY\"").count();
        let key_map_kf_count = env_overrides::KEY_MAP
            .iter()
            .filter(|(v, _)| v.starts_with("KF_CODE_"))
            .count();
        assert_eq!(
            env_var_count,
            key_map_kf_count + 18 + 5,
            "apply_env_overrides env-var literal count changed — did you add/remove a KF_CODE_* var?"
        );

        // ── 4. Relationship to total field count ──────────────────
<<<<<<< HEAD
        // Derived vars (name → key by stripping the prefix) reach every
        // field automatically; KEY_MAP only covers irregular names, so
        // its size is no longer coupled to CONFIG_FIELD_COUNT. The
        // invariants that remain: every KEY_MAP path resolves (check 2)
        // and every env-var literal in the loader is accounted for
        // (check 3).
||||||| 15ad6877
        // When CONFIG_FIELD_COUNT changes, verify that the difference
        // between it and the TOML/env counts is still intentional.
        // merge_toml expands sub-structs (e.g. computer_use has 7 sub-keys)
        // and skips some struct fields entirely, so the gap is NOT simply
        // CONFIG_FIELD_COUNT - MERGE_TOML_EXPECTED. The important invariant
        // is: every struct field is EITHER handled by merge_toml OR
        // intentionally skipped. The same applies to apply_env_overrides.
        //
        // Intentionally skipped by merge_toml (14 struct-level fields):
        //   ModelConfig:  summarize_enabled, subagent_allowed_models,
        //                 opencode_zen_api_key, opencode_zen_endpoint, seed
        //   SecurityConfig: permission_rules, docker (4 sub-fields),
        //                   sandbox (4 sub-fields), computer_use.max_steps
        //   ToolConfig:  max_tool_result_chars,
        //                mcp_servers, lsp_servers, max_plugin_trust,
        //                stratum_mode, budget_ceiling, budget_approaching_ratio
        //   SessionConfig: artifact_policy
        //
        // Additionally skipped by apply_env_overrides (4 more, beyond the 15):
        //   SecurityConfig: deny_paths, deny_urls, deny_extensions,
        //                   allowed_write_dirs
        //   (Arrays/Vec fields without env-var representations.)
        //
        // The expansion of computer_use (1 struct field → 7 TOML keys)
        // means MERGE_TOML_EXPECTED = top-level leaf keys + 7 expansion
        // keys.
        let _ = (
            CONFIG_FIELD_COUNT,
            MERGE_TOML_EXPECTED,
            ENV_OVERRIDE_EXPECTED,
        );
=======
        // When CONFIG_FIELD_COUNT changes, verify that the difference
        // between it and the TOML/env counts is still intentional.
        // merge_toml expands sub-structs (e.g. computer_use has 7 sub-keys)
        // and skips some struct fields entirely, so the gap is NOT simply
        // CONFIG_FIELD_COUNT - MERGE_TOML_EXPECTED. The important invariant
        // is: every struct field is EITHER handled by merge_toml OR
        // intentionally skipped. The same applies to apply_env_overrides.
        //
        // Intentionally skipped by merge_toml (14 struct-level fields):
        //   ModelConfig:  summarize_enabled, subagent_allowed_models,
        //                 opencode_zen_api_key, opencode_zen_endpoint, seed
        //   SecurityConfig: permission_rules, docker (4 sub-fields),
        //                   sandbox (4 sub-fields), computer_use.max_steps
        //   ToolConfig:  max_tool_result_chars,
        //                mcp_servers, lsp_servers, max_plugin_trust,
        //                stratum_mode, budget_ceiling, budget_approaching_ratio
        //   SessionConfig: artifact_policy
        //
        // Additionally skipped by apply_env_overrides (4 more, beyond the 15):
        //   SecurityConfig: deny_paths, deny_urls, deny_extensions,
        //                   allowed_write_dirs
        //   DisplayConfig: extra_commands
        //   (Arrays/Vec fields without env-var representations.)
        //
        // The expansion of computer_use (1 struct field → 7 TOML keys)
        // means MERGE_TOML_EXPECTED = top-level leaf keys + 7 expansion
        // keys.
        let _ = (
            CONFIG_FIELD_COUNT,
            MERGE_TOML_EXPECTED,
            ENV_OVERRIDE_EXPECTED,
        );
>>>>>>> wo/wo47.13

        // ── 5. Serde field count vs CONFIG_FIELD_COUNT ──────────
        // Serialize a default Config to JSON and count top-level keys.
        // This catches the case where someone adds a field to a sub-struct
        // but forgets to update CONFIG_FIELD_COUNT. Fields with
        // #[serde(skip_serializing)] are not counted by serde but ARE
        // config fields, so we add them back.
        let default_config = crate::shared::config::Config::default();
        let json = serde_json::to_value(&default_config).unwrap();
        let obj = json.as_object().unwrap();
        // Fields that exist in the struct but are skipped during serialization.
        const SKIP_SERIALIZING_FIELDS: usize = 1; // seed (ModelConfig)
                                                  // Flattened-name collisions: ToolConfig.memory_auto_populate and
                                                  // DisplayConfig.memory_auto_populate both serialize to the same
                                                  // JSON key, so serde produces 1 key for 2 struct fields. Document
                                                  // here so the count stays honest.
        const FLATTEN_COLLISIONS: usize = 1; // memory_auto_populate
        let serde_field_count = obj.len() + SKIP_SERIALIZING_FIELDS + FLATTEN_COLLISIONS;
        assert_eq!(
            CONFIG_FIELD_COUNT, serde_field_count,
            "CONFIG_FIELD_COUNT ({CONFIG_FIELD_COUNT}) != serde field count ({serde_field_count}) \
             — did you add/remove a config field without updating CONFIG_FIELD_COUNT?"
        );
    }

    /// **Contract:** a config.toml written by an older build (or
    /// hand-edited to a subset of keys) must load via the PRIMARY serde
    /// path — user values preserved, missing fields filled from
    /// `Default`. The struct-level `#[serde(default)]` on the Config
    /// sub-structs is what guarantees this: without it, any field the
    /// file lacks that has no field-level serde default makes
    /// `toml::from_str::<Config>` fail, and the load falls into the
    /// `merge_toml_into_config` fallback — which silently resets every
    /// field it doesn't handle (budget_ceiling, summarize_enabled,
    /// docker, sandbox, permission_rules, …). The next `save_config`
    /// then persists the wipe. This test simulates schema drift with a
    /// file that predates `default_model` (and sets two
    /// fallback-skipped canaries); if the load ever regresses to the
    /// lossy fallback, the canaries come back as defaults.
    #[test]
    fn schema_drift_preserves_user_values() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());

        let dir = std::env::temp_dir().join(format!("kf_code_drift_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let _env =
            crate::shared::test_util::EnvGuard::set("KF_CODE_DATA_DIR", dir.to_str().unwrap());
        let path = super::super::config_path();
        std::fs::write(
            &path,
            "ollama_host = \"http://my-host:1234\"\n\
             auto_approve = true\n\
             budget_ceiling = 50000\n\
             summarize_enabled = true\n",
        )
        .unwrap();

        let (cfg, warning) = load_config();
        assert!(
            warning.is_none(),
            "drifted config must parse via the primary serde path, got: {warning:?}"
        );
        // User-set values survive.
        assert_eq!(cfg.model.ollama_host, "http://my-host:1234");
        assert!(cfg.security.auto_approve);
        // Canaries for fields merge_toml_into_config does NOT handle:
        // only the primary serde path preserves them.
        assert_eq!(cfg.tools.budget_ceiling, 50000);
        assert!(cfg.model.summarize_enabled);
        // Missing fields are filled from defaults, not errors.
        assert!(cfg.model.default_model.is_empty());
        assert_eq!(cfg.model.request_timeout_secs, 120);

        // The merged result must round-trip: saving (e.g. a plugin
        // enable or "always allow" persisting a permission rule) writes
        // the user's values back, never a fresh Config::default().
        save_config(&cfg).unwrap();
        let on_disk = std::fs::read_to_string(&path).unwrap();
        assert!(
            on_disk.contains("http://my-host:1234"),
            "user's ollama_host wiped on save:\n{on_disk}"
        );
        assert!(
            on_disk.contains("budget_ceiling = 50000"),
            "user's budget_ceiling wiped on save:\n{on_disk}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// **Contract:** an existing `config.toml` at the current data-dir
    /// path must NOT be overwritten by `load_or_create_config`. This is
    /// the core pin against the reported "config gets reset on install"
    /// bug: the function's `!exists` guard is what protects returning
    /// users. If this regresses, the banner prints on every run and the
    /// user's customizations are wiped.
    #[test]
    fn existing_config_not_overwritten_on_startup() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());

        let dir = std::env::temp_dir().join(format!(
            "kf_code_keep_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let _env1 =
            crate::shared::test_util::EnvGuard::set("KF_CODE_DATA_DIR", dir.to_str().unwrap());
        let _env2 = crate::shared::test_util::EnvGuard::remove("KF_CODE_LEGACY_DATA_DIR");
        let path = super::super::config_path();
        // Seed a config with a user customization.
        std::fs::write(
            &path,
            "ollama_host = \"http://user-custom:9999\"\n\
             auto_approve = true\n",
        )
        .unwrap();

        let cfg = load_or_create_config();
        assert_eq!(
            cfg.model.ollama_host, "http://user-custom:9999",
            "existing user config must not be overwritten"
        );
        assert!(cfg.security.auto_approve);
        // The file on disk must still contain the user's values, not defaults.
        let on_disk = std::fs::read_to_string(&path).unwrap();
        assert!(
            on_disk.contains("http://user-custom:9999"),
            "config file on disk was overwritten with defaults:\n{on_disk}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// **Contract:** when the config is absent at the current path but a
    /// legacy `kirkforge`-era config exists at the pre-rename path,
    /// `load_or_create_config` must migrate it (copy) to the new path
    /// instead of writing fresh defaults. This is the migration pin for
    /// the `kirkforge → kf-code` rename (commit ae0e37d). Without it,
    /// upgrading users lose every customization on first run of the new
    /// binary — the exact "dead on startup" report.
    #[test]
    fn legacy_kirkforge_config_migrated_not_overwritten() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());

        let new_dir = std::env::temp_dir().join(format!(
            "kf_code_mig_new_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let legacy_dir = std::env::temp_dir().join(format!(
            "kf_code_mig_legacy_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
                .wrapping_add(1)
        ));
        let _ = std::fs::remove_dir_all(&new_dir);
        let _ = std::fs::remove_dir_all(&legacy_dir);
        std::fs::create_dir_all(&legacy_dir).unwrap();

        // Simulate a pre-rename user: config lives at the legacy path
        // with a customization. The new path must not exist yet.
        std::fs::write(
            legacy_dir.join("config.toml"),
            "ollama_host = \"http://user-custom:9999\"\n\
             auto_approve = true\n",
        )
        .unwrap();

        let _env1 =
            crate::shared::test_util::EnvGuard::set("KF_CODE_DATA_DIR", new_dir.to_str().unwrap());
        let _env2 = crate::shared::test_util::EnvGuard::set(
            "KF_CODE_LEGACY_DATA_DIR",
            legacy_dir.to_str().unwrap(),
        );
        let new_path = super::super::config_path();
        assert!(!new_path.exists(), "precondition: new config absent");

        let cfg = load_or_create_config();
        // User values migrated, not wiped to defaults.
        assert_eq!(
            cfg.model.ollama_host, "http://user-custom:9999",
            "legacy config must be migrated, not overwritten with defaults"
        );
        assert!(cfg.security.auto_approve);
        // The migration wrote the user's file to the new path verbatim.
        assert!(new_path.exists(), "migrated config written to new path");
        let on_disk = std::fs::read_to_string(&new_path).unwrap();
        assert!(
            on_disk.contains("http://user-custom:9999"),
            "migrated file must contain user values, got:\n{on_disk}"
        );

        let _ = std::fs::remove_dir_all(&new_dir);
        let _ = std::fs::remove_dir_all(&legacy_dir);
    }

    /// WO 38.6: `save_config` must be atomic — write to a temp file, fsync,
    /// then rename over the target. A crash mid-write leaves the original
    /// intact. WO 46.24: the shared `atomic_write` uses O_EXCL + a random
    /// tmp name, so a stale predictable `.toml.tmp` left by a prior crash
    /// is now harmless orphan litter (never opened, never followed) — the
    /// new save writes to its own random temp and renames over the target.
    /// This test verifies a successful save produces complete content at
    /// the target and atomically replaces the original.
    #[test]
    fn save_config_atomic_leaves_no_temp_and_cleans_stale_tmp() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());

        let dir = std::env::temp_dir().join(format!(
            "kf_code_atomic_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let _env =
            crate::shared::test_util::EnvGuard::set("KF_CODE_DATA_DIR", dir.to_str().unwrap());
        let path = super::super::config_path();

        // Seed an original config so we can prove a successful save replaces it
        // atomically (not by truncating it mid-write).
        let original = "ollama_host = \"http://original:1111\"\n";
        std::fs::write(&path, original).unwrap();

        // A stale predictable .toml.tmp from a pre-46.24 crash. The new
        // atomic_write uses a random temp name + O_EXCL, so this file is
        // never opened or followed — it's orphan litter. Seed it to prove
        // the save no longer depends on cleaning it up.
        let stale_tmp = path.with_extension("toml.tmp");
        std::fs::write(&stale_tmp, "PARTIAL TORN WRITE").unwrap();

        let mut cfg = Config::default();
        cfg.model.ollama_host = "http://new-host:2222".into();
        save_config(&cfg).unwrap();

        // The target must now hold the new complete content.
        let on_disk = std::fs::read_to_string(&path).unwrap();
        assert!(
            on_disk.contains("http://new-host:2222"),
            "save_config must write the new config, got:\n{on_disk}"
        );
        assert!(
            !on_disk.contains("PARTIAL TORN WRITE"),
            "stale temp content must not leak into the target"
        );

        // No new temp file must linger after a successful save. The stale
        // predictable .toml.tmp is harmless orphan litter (not opened by
        // the random-name path); we leave it in place rather than pretend
        // the save cleans it up.
        let leftovers: Vec<_> = std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect();
        assert!(
            leftovers
                .iter()
                .all(|n| n == path.file_name().unwrap().to_str().unwrap()
                    || n == "config.toml.tmp"),
            "only the target and the pre-existing stale .toml.tmp should remain, got: {leftovers:?}"
        );

        // The original content is gone (replaced atomically), confirming the
        // rename happened — if save had used std::fs::write directly, a crash
        // between truncate and write would have left an empty/partial file.
        assert!(
            !on_disk.contains("http://original:1111"),
            "original content must be replaced, got:\n{on_disk}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
