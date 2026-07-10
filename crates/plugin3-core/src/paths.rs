//! XDG path resolution. Per ADR-0014.

use std::path::PathBuf;

pub struct Paths {
    pub config_dir: PathBuf,
    pub data_dir: PathBuf,
    pub runtime_dir: PathBuf,
}

impl Paths {
    pub fn resolve() -> Self {
        let proj = directories::ProjectDirs::from("dev", "kirkforge", "plugin3");
        let (cfg_default, data_default, run_default) = match proj {
            Some(p) => (
                p.config_dir().to_path_buf(),
                p.data_dir().to_path_buf(),
                p.runtime_dir()
                    .map_or_else(|| p.data_dir().to_path_buf(), std::path::Path::to_path_buf),
            ),
            None => (PathBuf::from("."), PathBuf::from("."), PathBuf::from(".")),
        };
        Self {
            config_dir: std::env::var("PLUGIN3_CONFIG_DIR").map_or(cfg_default, PathBuf::from),
            data_dir: std::env::var("PLUGIN3_DATA_DIR").map_or(data_default, PathBuf::from),
            runtime_dir: std::env::var("PLUGIN3_RUNTIME_DIR").map_or(run_default, PathBuf::from),
        }
    }

    // Derived paths (ADR-0014 § directory layout). One source of truth
    // so the CLI and `cost::usage_path` stop computing them inline.
    #[must_use]
    pub fn budget_file(&self) -> PathBuf {
        // ponytail: B2 fix — `used` is session-local. Keep budget.toml in
        // runtime_dir so yesterday's session counter does not bleed into
        // today's first hook invocation. `ceiling`/`approaching_ratio`
        // defaults persist in config.toml (ADR-0005) and overlay at load.
        self.runtime_dir.join("budget.toml")
    }
    #[must_use]
    pub fn slices_dir(&self) -> PathBuf {
        self.data_dir.join("slices")
    }
    #[must_use]
    pub fn usage_log(&self) -> PathBuf {
        self.data_dir.join("logs").join("usage.jsonl")
    }
    #[must_use]
    pub fn recent_outputs(&self) -> PathBuf {
        self.data_dir.join("recent_outputs.jsonl")
    }
    // User-editable defaults (ADR-0005 + ADR-0014). Lives in config_dir
    // alongside any future config files; written by `plugin3 budget set --default`.
    #[must_use]
    pub fn config_file(&self) -> PathBuf {
        self.config_dir.join("config.toml")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::EnvGuard;

    #[test]
    fn resolve_does_not_panic() {
        // ponytail: env override may be unset; resolve must still return sane defaults.
        let _ = Paths::resolve();
    }

    // ponytail: env vars that the test sets must be restored on
    // every exit path — including a failed assertion or a panic in
    // `Paths::resolve()`. The earlier shape (manual `set_var` then
    // `remove_var` at the bottom) leaked the override env into any
    // test that ran after this one if an assertion failed, and into
    // the user's shell if the panic happened to be the last thing
    // cargo did before reporting the failure. The `EnvGuard` Drop
    // restores the prior value (or unsets if there was none) — same
    // pattern as `cost.rs::tests::EnvGuard`.
    //
    // The skip-if-conflict precondition still stands: if a developer
    // has PLUGIN3_*_DIR set in their shell, the test cannot
    // distinguish "test override" from "shell override" and we skip
    // rather than corrupt the developer's environment. Parallel
    // tests that don't touch these env vars are unaffected.
    #[test]
    fn env_overrides_take_precedence_over_xdg() {
        if std::env::var("PLUGIN3_CONFIG_DIR").is_ok()
            || std::env::var("PLUGIN3_DATA_DIR").is_ok()
            || std::env::var("PLUGIN3_RUNTIME_DIR").is_ok()
        {
            eprintln!("skipping: PLUGIN3_*_DIR already set in this environment");
            return;
        }
        let _g_cfg = EnvGuard::set("PLUGIN3_CONFIG_DIR", "/tmp/cfg");
        let _g_data = EnvGuard::set("PLUGIN3_DATA_DIR", "/tmp/data");
        let _g_run = EnvGuard::set("PLUGIN3_RUNTIME_DIR", "/tmp/run");
        let p = Paths::resolve();
        assert_eq!(p.config_dir, PathBuf::from("/tmp/cfg"));
        assert_eq!(p.data_dir, PathBuf::from("/tmp/data"));
        assert_eq!(p.runtime_dir, PathBuf::from("/tmp/run"));
        // The guards restore on drop at end of scope — even if an
        // assertion above panicked.
    }

    // ponytail: each PLUGIN3_*_DIR var is read independently
    // (paths.rs:23-25). The `env_overrides_take_precedence_over_xdg`
    // test above sets all three at once; nothing pins the partial
    // case where a host or developer overrides one var but not the
    // others. A contributor who collapses the three lookups into a
    // shared "all-or-nothing" helper (e.g. reads each into an Option,
    // then requires all-or-none) would silently break partial
    // overrides — a user who exports only PLUGIN3_DATA_DIR would get
    // the XDG default for data_dir too.
    //
    // Sequential single-test layout (not three separate `#[test]`
    // functions): the env-var writers are process-global, so a
    // parallel `#[test]` running the `env_guard_restores_prior_value_on_panic`
    // neighbour races with a partial-override test on the same key
    // and reads a leaked prior value. Doing all three overrides
    // inside one test serialises the env writes. Cost: one test
    // function, three assertions — same code path, no race.
    #[test]
    fn partial_env_override_takes_effect_independently_per_var() {
        if std::env::var("PLUGIN3_CONFIG_DIR").is_ok()
            || std::env::var("PLUGIN3_DATA_DIR").is_ok()
            || std::env::var("PLUGIN3_RUNTIME_DIR").is_ok()
        {
            eprintln!("skipping: PLUGIN3_*_DIR already set in this environment");
            return;
        }

        // 1) Only CONFIG_DIR set.
        let g1 = EnvGuard::set("PLUGIN3_CONFIG_DIR", "/tmp/cfg-only");
        let p1 = Paths::resolve();
        assert_eq!(
            p1.config_dir,
            PathBuf::from("/tmp/cfg-only"),
            "PLUGIN3_CONFIG_DIR alone must override config_dir"
        );
        assert_ne!(
            p1.data_dir,
            PathBuf::from("/tmp/cfg-only"),
            "with PLUGIN3_DATA_DIR unset, data_dir must NOT collapse to the \
             config_dir override — it must come from the XDG default; got {:?}",
            p1.data_dir
        );
        assert_ne!(
            p1.runtime_dir,
            PathBuf::from("/tmp/cfg-only"),
            "with PLUGIN3_RUNTIME_DIR unset, runtime_dir must NOT collapse to the \
             config_dir override; got {:?}",
            p1.runtime_dir
        );
        drop(g1);

        // 2) Only DATA_DIR set. Derived data paths must track the
        // override; budget_file does NOT (it is session-local under
        // runtime_dir per B2).
        let g2 = EnvGuard::set("PLUGIN3_DATA_DIR", "/tmp/data-only");
        let p2 = Paths::resolve();
        assert_eq!(
            p2.data_dir,
            PathBuf::from("/tmp/data-only"),
            "PLUGIN3_DATA_DIR alone must override data_dir"
        );
        // slices_dir, usage_log, and recent_outputs sit under data_dir.
        assert_eq!(p2.slices_dir(), PathBuf::from("/tmp/data-only/slices"));
        assert_eq!(
            p2.usage_log(),
            PathBuf::from("/tmp/data-only/logs/usage.jsonl")
        );
        assert_eq!(
            p2.recent_outputs(),
            PathBuf::from("/tmp/data-only/recent_outputs.jsonl")
        );
        // budget_file is runtime-local; with DATA_DIR overridden but
        // RUNTIME_DIR unset, it must come from the XDG default runtime
        // dir, NOT from /tmp/data-only.
        assert_ne!(
            p2.budget_file(),
            PathBuf::from("/tmp/data-only/budget.toml"),
            "budget.toml must NOT follow the data_dir override; it lives in runtime_dir"
        );
        assert_ne!(
            p2.config_dir,
            PathBuf::from("/tmp/data-only"),
            "config_dir must NOT pick up the data_dir override"
        );
        drop(g2);

        // 3) Only RUNTIME_DIR set. Derived runtime paths (budget_file)
        // must track the override.
        let _g3 = EnvGuard::set("PLUGIN3_RUNTIME_DIR", "/tmp/run-only");
        let p3 = Paths::resolve();
        assert_eq!(
            p3.runtime_dir,
            PathBuf::from("/tmp/run-only"),
            "PLUGIN3_RUNTIME_DIR alone must override runtime_dir"
        );
        assert_eq!(
            p3.budget_file(),
            PathBuf::from("/tmp/run-only/budget.toml"),
            "budget.toml must follow the runtime_dir override"
        );
        assert_ne!(
            p3.data_dir,
            PathBuf::from("/tmp/run-only"),
            "with PLUGIN3_DATA_DIR unset, data_dir must NOT pick up the \
             runtime_dir override; got {:?}",
            p3.data_dir
        );
        assert_ne!(
            p3.config_dir,
            PathBuf::from("/tmp/run-only"),
            "with PLUGIN3_CONFIG_DIR unset, config_dir must NOT pick up the \
             runtime_dir override; got {:?}",
            p3.config_dir
        );
        // EnvGuards restore on drop at end of scope.
    }

    // ponytail: env-var guard lives in `crate::test_support` now.
    // It uses a process-global reentrant mutex so parallel tests that
    // touch PLUGIN3_*_DIR cannot race, and nested guards in the same
    // thread do not deadlock. See test_support.rs for the
    // `ReentrantMutex` implementation and the B8 fix note.

    #[test]
    fn derived_paths_match_adr_directory_layout() {
        // ponytail: one source of truth for the on-disk layout
        // (ADR-0014). If a future contributor renames a file or moves
        // it, the test surfaces the change.
        let p = Paths {
            config_dir: PathBuf::from("/c"),
            data_dir: PathBuf::from("/d"),
            runtime_dir: PathBuf::from("/r"),
        };
        // B2: budget.toml is session-local, so it lives in runtime_dir.
        assert_eq!(p.budget_file(), PathBuf::from("/r/budget.toml"));
        assert_eq!(p.slices_dir(), PathBuf::from("/d/slices"));
        assert_eq!(p.usage_log(), PathBuf::from("/d/logs/usage.jsonl"));
        assert_eq!(p.recent_outputs(), PathBuf::from("/d/recent_outputs.jsonl"));
        assert_eq!(p.config_file(), PathBuf::from("/c/config.toml"));
    }

    // ponytail: pin the EnvGuard contract. A contributor who drops
    // the Drop impl (or moves the restore to a manual `remove_var`
    // call at the end of a function) breaks here — the prior value
    // would survive the panic and leak into the next test. We exercise
    // the panic path by spawning a closure that panics inside
    // `catch_unwind`; the `EnvGuard` is dropped during unwind, and
    // the assertion runs after the catch.
    //
    // The use of `std::panic::catch_unwind` is the explicit "recover
    // from a panic" hook — a test author would only reach for it
    // when the test is *about* panic behaviour. Asserting the prior
    // value is restored after the catch is the regression check.
    #[test]
    fn env_guard_restores_prior_value_on_panic() {
        if std::env::var("PLUGIN3_CONFIG_DIR").is_ok() {
            eprintln!("skipping: PLUGIN3_CONFIG_DIR already set in this environment");
            return;
        }
        let prior = std::env::var("PLUGIN3_CONFIG_DIR").ok();
        // Inner closure sets the override, then panics. The
        // EnvGuard's Drop runs during the unwind.
        let result = std::panic::catch_unwind(|| {
            let _g = EnvGuard::set("PLUGIN3_CONFIG_DIR", "/tmp/cfg-from-guard");
            // While the guard is live, the env var must be set.
            assert_eq!(
                std::env::var("PLUGIN3_CONFIG_DIR").as_deref(),
                Ok("/tmp/cfg-from-guard"),
                "guard must set the env var while it is alive",
            );
            panic!("forced unwind to exercise Drop");
        });
        assert!(result.is_err(), "inner closure must have panicked");
        // After the unwind completes and the guard's Drop ran, the
        // env var must be restored to its prior state. The whole
        // point of the guard: no leak.
        let now = std::env::var("PLUGIN3_CONFIG_DIR").ok();
        assert_eq!(
            now, prior,
            "EnvGuard Drop must restore the prior value (or unset if there was none); \
             got {now:?}, expected {prior:?}. A leak here means a failed assertion \
             in env_overrides_take_precedence_over_xdg would pollute the next test."
        );
    }

    // ponytail: pin the OTHER branch of EnvGuard::Drop — when prior
    // is Some(v), Drop must call `set_var(self.key, v)`, NOT
    // `remove_var(self.key)`. The panic test above only exercises
    // the prior=None branch (because it skips if the var is already
    // set). Without this test, a contributor who "simplifies" the
    // Drop to always `remove_var` passes the panic test (prior=None
    // means unset-after-Drop is correct either way) but silently
    // deletes the var the developer's shell set. Layout: sequential
    // single test, like the partial-override test, because the
    // env writes are process-global and races with the panic test
    // produce false positives.
    #[test]
    fn env_guard_restores_prior_value_some_branch() {
        if std::env::var("PLUGIN3_CONFIG_DIR").is_ok() {
            eprintln!("skipping: PLUGIN3_CONFIG_DIR already set in this environment");
            return;
        }
        // First guard: seed PLUGIN3_CONFIG_DIR with a known prior
        // value. Drop it so the next guard sees prior=Some(...).
        {
            let _g_seed = EnvGuard::set("PLUGIN3_CONFIG_DIR", "/tmp/cfg-seed");
            assert_eq!(
                std::env::var("PLUGIN3_CONFIG_DIR").as_deref(),
                Ok("/tmp/cfg-seed"),
            );
        }
        // After the seed guard dropped, the env var must be gone
        // (prior was None). Confirm the seed round-trip so a future
        // contributor who breaks the None branch surfaces here too.
        assert!(
            std::env::var("PLUGIN3_CONFIG_DIR").is_err(),
            "seed EnvGuard (prior=None) must unset the env var on drop; \
             found {:?}",
            std::env::var("PLUGIN3_CONFIG_DIR").ok()
        );

        // Second guard: now prior=None. Inner guard inside a
        // catch_unwind sets prior=Some("/tmp/cfg-prior"), then
        // a third guard sets/clears on top. The point of this test
        // is the OUTER Some branch: set, then drop, and the value
        // must come back to /tmp/cfg-prior (not unset).
        let outer_prior = "/tmp/cfg-prior";
        {
            let _g_outer = EnvGuard::set("PLUGIN3_CONFIG_DIR", outer_prior);
            assert_eq!(
                std::env::var("PLUGIN3_CONFIG_DIR").as_deref(),
                Ok(outer_prior),
            );
            // Inner guard: prior is now Some(outer_prior). Drop it,
            // and the env var must come back to outer_prior — not
            // be removed.
            {
                let _g_inner = EnvGuard::set("PLUGIN3_CONFIG_DIR", "/tmp/cfg-inner");
                assert_eq!(
                    std::env::var("PLUGIN3_CONFIG_DIR").as_deref(),
                    Ok("/tmp/cfg-inner"),
                );
            }
            // Inner guard dropped; prior was Some(outer_prior),
            // so the env var must be restored to outer_prior.
            assert_eq!(
                std::env::var("PLUGIN3_CONFIG_DIR").as_deref(),
                Ok(outer_prior),
                "EnvGuard Drop with prior=Some(v) must call set_var(key, v), \
                 NOT remove_var(key). Got {:?}, expected {:?}",
                std::env::var("PLUGIN3_CONFIG_DIR").ok(),
                Some(outer_prior),
            );
        }
        // Outer guard dropped; prior was None, so the env var
        // must be unset.
        assert!(
            std::env::var("PLUGIN3_CONFIG_DIR").is_err(),
            "outer EnvGuard (prior=None) must unset the env var on drop; \
             found {:?}",
            std::env::var("PLUGIN3_CONFIG_DIR").ok()
        );
    }
}
