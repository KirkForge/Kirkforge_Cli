//! ADR-0014 § Implementation notes prescribes three drift tests:
//!
//!   1. The default paths (`$XDG_*_HOME/plugin3/...`).
//!   2. The atomic write round-trip.
//!   3. The 32-entry recent-outputs bound (pinned in `main.rs`
//!      by `recent_bound_is_pinned_at_32`).
//!
//! The inline unit tests in `paths.rs` and `atomic_write.rs` cover
//! #1 and #2 in isolation. This integration test pins them under
//! the same `Paths::resolve()` a real CLI invocation uses, so a
//! contributor who breaks the env-var override precedence surfaces
//! here too.
//!
//! ponytail: #3 (the recent-outputs bound) is intentionally not
//! covered here — it lives next to the constant it pins in
//! `main.rs`. A `flock`-style lock is *not* on the drift list
//! (ADR-0017 defers concurrent hooks to a future build feature;
//! today hooks run serially per session so the lock has no race
//! to guard). Add `with_lock` + tests when concurrent
//! `PostToolUse` invocations land.

use std::path::PathBuf;

use plugin3_core::atomic_write_text;
use plugin3_core::Paths;

#[test]
fn derived_paths_match_adr_directory_layout() {
    // ponytail: ADR-0014 § Directory layout is the spec. The
    // drift test in `paths.rs` covers one constructor; this one
    // exercises `Paths::resolve()` + the derived accessors
    // together, which is the path the CLI actually walks.
    //
    // Skip if the env override is set so we don't depend on a
    // developer's shell state.
    if std::env::var("PLUGIN3_CONFIG_DIR").is_ok()
        || std::env::var("PLUGIN3_DATA_DIR").is_ok()
        || std::env::var("PLUGIN3_RUNTIME_DIR").is_ok()
    {
        eprintln!("skipping: PLUGIN3_*_DIR set in this environment");
        return;
    }

    let p = Paths::resolve();
    // budget.toml lives in runtime_dir (ADR-0014 § B2 — session-local).
    assert_eq!(p.budget_file(), p.runtime_dir.join("budget.toml"));
    // usage.jsonl lives under data_dir/logs/.
    assert_eq!(p.usage_log(), p.data_dir.join("logs").join("usage.jsonl"));
    // recent_outputs.jsonl lives in data_dir.
    assert_eq!(p.recent_outputs(), p.data_dir.join("recent_outputs.jsonl"));
    // config.toml lives in config_dir (user-editable).
    assert_eq!(p.config_file(), p.config_dir.join("config.toml"));
}

#[test]
fn env_overrides_take_precedence_over_xdg_defaults() {
    // ponytail: ADR-0014 § Path resolution says PLUGIN3_*_DIR
    // wins over the directories-crate defaults. The inline test
    // in `paths.rs` uses `set_var`/`remove_var` directly; this
    // integration test composes with `Paths::resolve()` so the
    // precedence path is exercised end-to-end.
    if std::env::var("PLUGIN3_CONFIG_DIR").is_ok()
        || std::env::var("PLUGIN3_DATA_DIR").is_ok()
        || std::env::var("PLUGIN3_RUNTIME_DIR").is_ok()
    {
        eprintln!("skipping: PLUGIN3_*_DIR set in this environment");
        return;
    }

    // SAFETY: skip-guard above ensures no parallel writer holds
    // these vars. The test restores the original (unset) state
    // before returning so subsequent tests see a clean env.
    unsafe {
        std::env::set_var("PLUGIN3_CONFIG_DIR", "/tmp/p3-cfg");
        std::env::set_var("PLUGIN3_DATA_DIR", "/tmp/p3-data");
        std::env::set_var("PLUGIN3_RUNTIME_DIR", "/tmp/p3-run");
    }
    let p = Paths::resolve();
    unsafe {
        std::env::remove_var("PLUGIN3_CONFIG_DIR");
        std::env::remove_var("PLUGIN3_DATA_DIR");
        std::env::remove_var("PLUGIN3_RUNTIME_DIR");
    }
    assert_eq!(p.config_dir, PathBuf::from("/tmp/p3-cfg"));
    assert_eq!(p.data_dir, PathBuf::from("/tmp/p3-data"));
    assert_eq!(p.runtime_dir, PathBuf::from("/tmp/p3-run"));
    // Derived paths also follow the override.
    assert_eq!(p.budget_file(), PathBuf::from("/tmp/p3-run/budget.toml"));
}

#[test]
fn atomic_write_then_read_round_trips_real_paths_struct() {
    // ponytail: ADR-0014 § Atomic flag file for budget. The
    // inline tests in `atomic_write.rs` use raw bytes; this
    // integration test composes with the real Paths layout so a
    // contributor who changes either the layout OR the writer
    // surfaces here, not in two unrelated test files.
    let dir = tempfile::tempdir().expect("tempdir");
    let paths = Paths {
        config_dir: dir.path().join("cfg"),
        data_dir: dir.path().join("data"),
        runtime_dir: dir.path().join("run"),
    };
    // budget.toml under runtime_dir, nested like a real XDG layout.
    let budget_path = paths.budget_file();

    // Round-trip a real TokenBudget shape (ADR-0005).
    let body = "ceiling = 100\napproaching_ratio = 0.8\nused = 42\n";
    atomic_write_text(&budget_path, "budget", body);

    let read_back =
        std::fs::read_to_string(&budget_path).expect("budget file readable after atomic write");
    assert_eq!(read_back, body, "round-trip body mismatch");

    // Parent dir was created (atomic_write_text creates nested dirs).
    assert!(paths.runtime_dir.is_dir(), "runtime_dir not created");
}
