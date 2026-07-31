//! Budget persistence — `budget.toml` (runtime, session-local `used`)
//! and `config.toml` (`ceiling`/`approaching_ratio` defaults).
//! Per ADR-0002 § Crate layout (`crates/plugin3-cli/src/`).
//! ADR-0014: atomic write lives in plugin3-core. The CLI just calls it.

use plugin3_core::{
    atomic_write_text,
    budget::{BudgetConfig, ConfigFile, TokenBudget, UsageConfig},
    Paths,
};

pub(crate) fn budget_path() -> std::path::PathBuf {
    Paths::resolve().budget_file()
}

pub(crate) fn config_path() -> std::path::PathBuf {
    Paths::resolve().config_file()
}

pub(crate) fn load_budget() -> TokenBudget {
    load_budget_with_config(&budget_path(), &config_path())
}

pub(crate) fn save_budget(b: &TokenBudget) {
    save_budget_at(b, &budget_path());
}

// ponytail: removed `load_budget_at` — `load_budget_with_config` is the
// single entry point now. Splitting them again would invite drift
// between "what `load_budget` does" and "what tests of a single file do".

pub(crate) fn save_budget_at(b: &TokenBudget, path: &std::path::Path) {
    let Ok(s) = toml::to_string(b) else { return };
    atomic_write_text(path, "budget", &s);
}

// ---- config.toml (ADR-0005 § Defaults, ADR-0015 § budget set --default) -

// ponytail: `load_budget_config_at` returns Option rather than
// defaulting inside the parser. That way a missing file is
// distinguishable from "user wrote ceiling=0" — important when the
// runtime merge wants to skip override cleanly. The on-disk file
// is a `ConfigFile` wrapper (ADR-0005 § Defaults) so the
// `[budget]` section header is preserved.
pub(crate) fn load_budget_config_at(path: &std::path::Path) -> Option<BudgetConfig> {
    let s = std::fs::read_to_string(path).ok()?;
    let file: ConfigFile = toml::from_str(&s).ok()?;
    Some(file.budget)
}

// ponytail: wraps the `BudgetConfig` in `ConfigFile` to emit the
// `[budget]` section header (ADR-0005 § Defaults). Same atomic-write
// helper as `save_budget_at`.
pub(crate) fn save_budget_config_at(cfg: &BudgetConfig, path: &std::path::Path) {
    let file = ConfigFile {
        budget: *cfg,
        usage: UsageConfig::default(),
    };
    let Ok(s) = toml::to_string(&file) else {
        return;
    };
    atomic_write_text(path, "config", &s);
}

// Precedence: runtime budget.toml (used) > config.toml (ceiling/ratio) >
// TokenBudget::default(). The runtime file is per-session and never
// carries user defaults; config.toml is the persistence layer for
// `plugin3 budget set --default`.
pub(crate) fn load_budget_with_config(
    runtime_path: &std::path::Path,
    config_path: &std::path::Path,
) -> TokenBudget {
    let mut b = TokenBudget::default();
    if let Ok(s) = std::fs::read_to_string(runtime_path) {
        if let Ok(runtime) = toml::from_str::<TokenBudget>(&s) {
            b = runtime;
        }
    }
    // ponytail: config.toml always overrides ceiling/ratio when present,
    // even if the runtime file disagrees. The `used` counter is
    // session-local and intentionally NOT taken from config.
    if let Some(cfg) = load_budget_config_at(config_path) {
        b.ceiling = cfg.ceiling;
        b.approaching_ratio = cfg.approaching_ratio;
    }
    b
}
