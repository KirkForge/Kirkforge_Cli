//! `plugin3 config {show,validate}` — print effective config
//! paths or write-test them (ADR-0015 § Validate).

use plugin3_core::{budget::ConfigFile, budget::TokenBudget, Paths};

use crate::exit::exit_config_err;
use crate::precedence::{resolve_config_path, OsEnv};

pub(crate) fn show(show_sources: bool, as_json: bool) {
    let p = Paths::resolve();
    // ponytail: one source for the path list. JSON and human-readable
    // branches iterate the same (label, path) pairs — adding a new
    // derived path only requires editing this Vec. Bind the owned
    // PathBufs first so the Vec borrows live values, not temporaries.
    // The config_file line routes through the precedence chain
    // (ADR-0015) so a `--config` flag in future Cli wiring takes
    // effect here without further plumbing.
    let config_file = resolve_config_path(None, &OsEnv, &p.config_dir);
    let budget_file = p.budget_file();
    let slices_dir = p.slices_dir();
    let usage_log = p.usage_log();
    let recent_outputs = p.recent_outputs();
    let pairs: Vec<(&str, &std::path::Path)> = vec![
        ("config_dir", &p.config_dir),
        ("data_dir", &p.data_dir),
        ("runtime_dir", &p.runtime_dir),
        ("config_file", &config_file),
        ("budget_file", &budget_file),
        ("slices_dir", &slices_dir),
        ("usage_log", &usage_log),
        ("recent_outputs", &recent_outputs),
    ];
    if as_json {
        let mut resp = serde_json::Map::new();
        for (k, v) in &pairs {
            resp.insert((*k).to_string(), serde_json::json!(v));
        }
        // ponytail: --show-sources used to be silently dropped on the
        // JSON branch (early `return` before the env-source block).
        // Include the sources block here when asked so the flag
        // does what was asked on both output modes. Without
        // --show-sources, the JSON shape stays at 8 top-level keys;
        // with it, a 9th `sources` key joins the envelope.
        if show_sources {
            let src = |var: &str| {
                std::env::var(var)
                    .ok()
                    .map_or_else(|| "XDG default".into(), |v| format!("env {var}={v}"))
            };
            resp.insert(
                "sources".into(),
                serde_json::json!({
                    "config_dir": src("PLUGIN3_CONFIG_DIR"),
                    "data_dir": src("PLUGIN3_DATA_DIR"),
                    "runtime_dir": src("PLUGIN3_RUNTIME_DIR"),
                }),
            );
        }
        crate::json_out::print_json(&resp);
        return;
    }
    for (k, v) in &pairs {
        println!("{k:<16} {}", v.display());
    }
    if show_sources {
        let src = |var: &str| {
            std::env::var(var)
                .ok()
                .map_or_else(|| "XDG default".into(), |v| format!("env {var}={v}"))
        };
        println!("---");
        println!("config_dir:    {}", src("PLUGIN3_CONFIG_DIR"));
        println!("data_dir:      {}", src("PLUGIN3_DATA_DIR"));
        println!("runtime_dir:   {}", src("PLUGIN3_RUNTIME_DIR"));
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CheckStatus {
    Ok,
    Fail,
}

// ponytail: minimal check result. `detail` is free-form so we can
// record "createable", "exists+parseable", or a specific error
// message without bloating the enum.
#[derive(Debug)]
pub(crate) struct PathCheck {
    pub(crate) label: &'static str,
    pub(crate) path: std::path::PathBuf,
    pub(crate) status: CheckStatus,
    pub(crate) detail: String,
}

/// Write-test each path ADR-0014 / ADR-0015 cares about.
/// ponytail: validates the write surface, not the contents. A
/// parseable existing file gets bonus reporting; absence is OK
/// (e.g. config.toml before the first `budget set --default`,
/// or budget.toml before the first hook fires).
fn run_path_checks(p: &Paths) -> Vec<PathCheck> {
    let mut out = Vec::new();
    // ponytail: directories are checked by create_dir_all + a
    // NamedTempFile probe (auto-cleaned via RAII). No permanent
    // files left behind.
    for (label, path) in [
        ("config_dir", p.config_dir.clone()),
        ("data_dir", p.data_dir.clone()),
        ("runtime_dir", p.runtime_dir.clone()),
    ] {
        out.push(check_dir(label, &path));
    }
    // ponytail: file paths are checked by ensuring the parent dir
    // is writable and that any existing file parses. We do NOT
    // create the file — the runtime hooks do that on demand. An
    // empty file is "fresh", not a parse error.
    out.push(check_file("config_file", &p.config_file(), parse_config_at));
    out.push(check_file("budget_file", &p.budget_file(), parse_budget_at));
    out.push(check_file_parent("slices_dir", &p.slices_dir()));
    out.push(check_file_parent("usage_log", &p.usage_log()));
    out.push(check_file_parent("recent_outputs", &p.recent_outputs()));
    out
}

fn check_dir(label: &'static str, path: &std::path::Path) -> PathCheck {
    match std::fs::create_dir_all(path) {
        Ok(()) => match tempfile::NamedTempFile::new_in(path) {
            Ok(_) => PathCheck {
                label,
                path: path.to_path_buf(),
                status: CheckStatus::Ok,
                detail: "createable+writable".into(),
            },
            Err(e) => PathCheck {
                label,
                path: path.to_path_buf(),
                status: CheckStatus::Fail,
                detail: format!("createable but not writable: {e}"),
            },
        },
        Err(e) => PathCheck {
            label,
            path: path.to_path_buf(),
            status: CheckStatus::Fail,
            detail: e.to_string(),
        },
    }
}

fn check_file(
    label: &'static str,
    path: &std::path::Path,
    parse_existing: fn(&std::path::Path) -> Result<(), String>,
) -> PathCheck {
    if !path.exists() {
        // ponytail: absence is OK — runtime hooks will create the
        // file lazily. We only verify the parent dir would accept it.
        return check_file_parent(label, path);
    }
    let parent = path.parent().unwrap_or(std::path::Path::new("."));
    if let Err(e) = std::fs::create_dir_all(parent) {
        return PathCheck {
            label,
            path: path.to_path_buf(),
            status: CheckStatus::Fail,
            detail: format!("parent create failed: {e}"),
        };
    }
    let mut detail = "exists".to_string();
    {
        let Ok(s) = std::fs::read_to_string(path) else {
            return PathCheck {
                label,
                path: path.to_path_buf(),
                status: CheckStatus::Fail,
                detail: "unreadable".into(),
            };
        };
        // ponytail: an empty file is the post-init "no state yet"
        // case (ADR-0014). It parses as nothing — distinct from a
        // partial-write disaster that should fail validation.
        if s.trim().is_empty() {
            detail.push_str("+empty");
        } else {
            if let Err(e) = parse_existing(path) {
                return PathCheck {
                    label,
                    path: path.to_path_buf(),
                    status: CheckStatus::Fail,
                    detail: format!("parse failed: {e}"),
                };
            }
            detail.push_str("+parseable");
        }
    }
    PathCheck {
        label,
        path: path.to_path_buf(),
        status: CheckStatus::Ok,
        detail,
    }
}

fn check_file_parent(label: &'static str, path: &std::path::Path) -> PathCheck {
    let parent = path.parent().unwrap_or(std::path::Path::new("."));
    match std::fs::create_dir_all(parent) {
        Ok(()) => match tempfile::NamedTempFile::new_in(parent) {
            Ok(_) => PathCheck {
                label,
                path: path.to_path_buf(),
                status: CheckStatus::Ok,
                detail: "parent writable".into(),
            },
            Err(e) => PathCheck {
                label,
                path: path.to_path_buf(),
                status: CheckStatus::Fail,
                detail: format!("parent unwritable: {e}"),
            },
        },
        Err(e) => PathCheck {
            label,
            path: path.to_path_buf(),
            status: CheckStatus::Fail,
            detail: format!("parent create failed: {e}"),
        },
    }
}

fn parse_config_at(path: &std::path::Path) -> Result<(), String> {
    let s = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    let _: ConfigFile = toml::from_str(&s).map_err(|e| e.to_string())?;
    Ok(())
}

fn parse_budget_at(path: &std::path::Path) -> Result<(), String> {
    let s = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    let _: TokenBudget = toml::from_str(&s).map_err(|e| e.to_string())?;
    Ok(())
}

pub(crate) fn validate(as_json: bool) {
    let p = Paths::resolve();
    let checks = run_path_checks(&p);
    let failures: usize = checks
        .iter()
        .filter(|c| c.status == CheckStatus::Fail)
        .count();
    if as_json {
        let arr: Vec<serde_json::Value> = checks.iter().map(|c| serde_json::json!({
            "label": c.label,
            "path": c.path,
            "status": match c.status { CheckStatus::Ok => "ok", CheckStatus::Fail => "fail" },
            "detail": c.detail,
        })).collect();
        let resp = serde_json::json!({
            "ok": failures == 0,
            "failures": failures,
            "checks": arr,
        });
        crate::json_out::print_json(&resp);
    } else {
        for c in &checks {
            let status = match c.status {
                CheckStatus::Ok => "OK  ",
                CheckStatus::Fail => "FAIL",
            };
            println!(
                "{status}  {:<22}  {}  ({})",
                c.label,
                c.path.display(),
                c.detail
            );
        }
        println!("---");
        if failures == 0 {
            println!("all {} checks passed", checks.len());
        } else {
            println!("{failures} of {} checks failed", checks.len());
        }
    }
    // ponytail: ADR-0015 § Exit codes — config parse or backend init
    // failure → 78 (EX_CONFIG). A host that polls `plugin3 config
    // --validate` in CI gets a meaningful signal. Routed through
    // `exit::exit_config_err` so the magic number lives in one place
    // and `eprintln!` formatting is uniform across callers.
    if failures > 0 {
        exit_config_err(&format!(
            "{failures} of {} path checks failed",
            checks.len()
        ));
    }
}

// ponytail: re-exported for tests in main.rs. The validate harness
// runs subprocesses against a tempdir; nothing else needs these.
#[cfg(test)]
pub(crate) fn run_path_checks_for(p: &Paths) -> Vec<PathCheck> {
    run_path_checks(p)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ponytail: pin the writable-dir Ok path. The `check_dir`
    // helper writes via `create_dir_all` then probes writability
    // with a NamedTempFile. A contributor who removes the
    // writability probe (keeping only create_dir_all) silently
    // accepts a read-only mount as Ok — users see a confusing
    // hook failure later. The detail string carries the
    // `createable+writable` contract.
    #[test]
    fn check_dir_on_writable_tempdir_returns_ok() {
        let dir = tempfile::tempdir().expect("tempdir");
        let c = check_dir("d", dir.path());
        assert_eq!(
            c.status,
            CheckStatus::Ok,
            "writable tempdir must be Ok; got {c:?}"
        );
        assert!(
            c.detail.contains("createable+writable"),
            "detail must report both createable AND writable; got {:?}",
            c.detail
        );
    }

    // ponytail: pin the missing-file → check_file_parent
    // delegation. `check_file` returns Ok with `parent writable`
    // when the file doesn't exist yet (the runtime creates it
    // lazily). A contributor who skips the delegation and
    // instead returns Fail on missing files blocks the very
    // first run before any hooks have fired.
    #[test]
    fn check_file_on_missing_path_routes_to_parent_check_ok() {
        let dir = tempfile::tempdir().expect("tempdir");
        let missing = dir.path().join("does-not-exist.toml");
        let c = check_file("f", &missing, parse_config_at);
        assert_eq!(
            c.status,
            CheckStatus::Ok,
            "missing file must route to check_file_parent and return Ok; \
             got {c:?}"
        );
        assert_eq!(
            c.detail, "parent writable",
            "missing file must report the parent-writable detail verbatim; \
             got {:?}",
            c.detail
        );
    }

    // ponytail: pin the parseable existing-file Ok path. After
    // a `budget set --default`, config.toml contains a [budget]
    // section; check_file must report +parseable. A contributor
    // who drops the parse_existing callback wiring silently
    // loses parse failure detection on stale config files.
    #[test]
    fn check_file_on_parseable_existing_file_returns_ok_parseable() {
        let dir = tempfile::tempdir().expect("tempdir");
        let cfg = dir.path().join("config.toml");
        std::fs::write(
            &cfg,
            "[budget]\nceiling = 100000\napproaching_ratio = 0.8\n",
        )
        .expect("write config.toml");
        let c = check_file("config_file", &cfg, parse_config_at);
        assert_eq!(c.status, CheckStatus::Ok, "got {c:?}");
        assert!(
            c.detail.contains("+parseable"),
            "existing-parseable file must report +parseable; got {:?}",
            c.detail
        );
    }

    // ponytail: pin the unparseable existing-file Fail path.
    // The whole point of `plugin3 config --validate` is to
    // catch a corrupted config.toml before a hook fires and
    // silently drops records. A contributor who swallows
    // `parse_existing` errors or wraps them as Ok breaks
    // validation — users get a green checkmark and a broken
    // hook. Pin the detail prefix so the failure mode is
    // observable in `--json` mode too.
    #[test]
    fn check_file_on_unparseable_existing_file_returns_fail() {
        let dir = tempfile::tempdir().expect("tempdir");
        let cfg = dir.path().join("config.toml");
        std::fs::write(&cfg, "this is = not [ valid toml").expect("write");
        let c = check_file("config_file", &cfg, parse_config_at);
        assert_eq!(
            c.status,
            CheckStatus::Fail,
            "unparseable config.toml must FAIL validation; got {c:?}"
        );
        assert!(
            c.detail.starts_with("parse failed:"),
            "detail must carry the parse-failed prefix; got {:?}",
            c.detail
        );
    }

    // ponytail: pin the empty-file Ok path. An empty config.toml
    // is the "fresh init, no state yet" case (ADR-0014 §
    // directory layout). It must NOT trip parse validation —
    // a user who runs `plugin3 config --validate` on a brand
    // new install expects Ok. A contributor who treats empty
    // as "parse failed" blocks the first-run UX.
    #[test]
    fn check_file_on_empty_existing_file_returns_ok_empty() {
        let dir = tempfile::tempdir().expect("tempdir");
        let cfg = dir.path().join("config.toml");
        std::fs::write(&cfg, "").expect("write empty");
        let c = check_file("config_file", &cfg, parse_config_at);
        assert_eq!(
            c.status,
            CheckStatus::Ok,
            "empty config.toml must Ok (post-init, no state yet); got {c:?}"
        );
        assert!(
            c.detail.contains("+empty"),
            "empty file must report +empty detail; got {:?}",
            c.detail
        );
    }

    // ponytail: pin the budget.toml parser. The parse_budget_at
    // callback is fed by check_file("budget_file", ...) — a
    // valid budget.toml must round-trip through TokenBudget.
    // A contributor who switches parse_budget_at to TokenBudget's
    // default-construction (no read) silently accepts stale
    // budget.toml files. Pin via round-trip.
    //
    // Fixture shape: `save_budget_at` writes a `TokenBudget` (full
    // struct with `used`), NOT a `BudgetConfig` (which omits `used`).
    // Distinct from config.toml, which carries the `BudgetConfig`
    // wrapper under a `[budget]` section. Pin the full TokenBudget
    // shape so a contributor who changes the writer to write
    // `BudgetConfig` instead surfaces here, not as a stale validation.
    #[test]
    fn parse_budget_at_round_trips_valid_budget_toml() {
        let dir = tempfile::tempdir().expect("tempdir");
        let p = dir.path().join("budget.toml");
        std::fs::write(
            &p,
            "ceiling = 150000\napproaching_ratio = 0.75\nused = 42\n",
        )
        .expect("write");
        assert!(
            parse_budget_at(&p).is_ok(),
            "valid budget.toml must parse; got {:?}",
            parse_budget_at(&p)
        );
    }

    // ponytail: pin the parse_budget_at failure shape. The Ok/Fail
    // status of check_file depends on `is_ok()` — a contributor
    // who wraps the error in Ok(()) (a known "let's be
    // permissive" temptation) breaks the validation contract.
    #[test]
    fn parse_budget_at_returns_err_on_unparseable_content() {
        let dir = tempfile::tempdir().expect("tempdir");
        let p = dir.path().join("budget.toml");
        std::fs::write(&p, "ceiling = \"not a usize\"\n").expect("write");
        assert!(
            parse_budget_at(&p).is_err(),
            "unparseable budget.toml must return Err; got {:?}",
            parse_budget_at(&p)
        );
    }

    // ponytail: end-to-end wiring pin for `run_path_checks`. The
    // helper-level tests above cover `check_dir`, `check_file`,
    // and the missing-file delegation, but the wiring (which
    // label maps to which parser, all 7 labels present) lives
    // ONLY here. A contributor who:
    //   - drops a label from the `check_dir` loop
    //   - wires `parse_budget_at` to `config_file` (typo under
    //     refactor)
    //   - skips a `check_file_parent` entry for `recent_outputs`
    // …surfaces here because the label set or the count diverges.
    // The setup is hermetic: a `Paths` whose three roots point
    // into the same tempdir, all checks must come back Ok on a
    // fresh install (no config.toml, no budget.toml — those are
    // lazy-created by the runtime).
    #[test]
    fn run_path_checks_end_to_end_wires_all_seven_labels() {
        let dir = tempfile::tempdir().expect("tempdir");
        let p = plugin3_core::Paths {
            config_dir: dir.path().join("config"),
            data_dir: dir.path().join("data"),
            runtime_dir: dir.path().join("runtime"),
        };
        let checks = run_path_checks_for(&p);
        // ADR-0014/0015 cares about exactly 8 derived paths
        // (3 dirs + config_file + budget_file + 3 file-parent
        // entries: slices_dir, usage_log, recent_outputs).
        assert_eq!(
            checks.len(),
            8,
            "run_path_checks must cover exactly 8 paths (3 dirs + 5 derived); got {}",
            checks.len()
        );
        // Every label must be present — pinning the wiring.
        let labels: Vec<&'static str> = checks.iter().map(|c| c.label).collect();
        for expected in [
            "config_dir",
            "data_dir",
            "runtime_dir",
            "config_file",
            "budget_file",
            "slices_dir",
            "usage_log",
            "recent_outputs",
        ] {
            assert!(
                labels.contains(&expected),
                "missing label `{expected}` in run_path_checks output: {labels:?}"
            );
        }
        // On a fresh install (no config.toml, no budget.toml yet)
        // every check must come back Ok — the helpers handle the
        // missing-file case via check_file_parent. A contributor
        // who hard-fails on absent files breaks first-run UX.
        assert!(
            checks.iter().all(|c| c.status == CheckStatus::Ok),
            "every fresh-install check must be Ok; got {checks:?}"
        );
    }

    // ponytail: pin that a populated config.toml parses through
    // the wiring — a contributor who breaks the parser-fn handoff
    // (`parse_config_at` swapped with `parse_budget_at` for the
    // config_file entry) surfaces here because budget.toml's
    // shape (`ceiling = N`) would no longer parse as ConfigFile.
    // Distinct from `check_file_on_parseable_existing_file_returns_ok_parseable`
    // (which tests the helper with a `[budget]` block) — this
    // tests the full pipeline end-to-end through the helpers.
    #[test]
    fn run_path_checks_picks_up_existing_parseable_config_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let cfg_dir = dir.path().join("config");
        let data_dir = dir.path().join("data");
        std::fs::create_dir_all(&cfg_dir).unwrap();
        // ADR-0005: config.toml carries a [budget] section wrapping
        // the BudgetConfig (ceiling + approaching_ratio).
        std::fs::write(
            cfg_dir.join("config.toml"),
            "[budget]\nceiling = 100000\napproaching_ratio = 0.8\n",
        )
        .unwrap();
        let p = plugin3_core::Paths {
            config_dir: cfg_dir,
            data_dir,
            runtime_dir: dir.path().join("runtime"),
        };
        let checks = run_path_checks_for(&p);
        let config_check = checks
            .iter()
            .find(|c| c.label == "config_file")
            .expect("config_file label present");
        assert_eq!(
            config_check.status,
            CheckStatus::Ok,
            "config_file must Ok; got {config_check:?}"
        );
        assert!(
            config_check.detail.contains("+parseable"),
            "config_file with valid [budget] block must report +parseable; got {:?}",
            config_check.detail
        );
    }

    // ponytail: pin that an UNPARSEABLE existing config.toml is
    // surfaced through the full pipeline (not silently dropped
    // at the helper boundary). The whole point of `plugin3 config
    // --validate` is to catch a corrupted config.toml before a
    // hook fires and silently drops records. If a contributor
    // moves the parse-exists check from `check_file` into
    // `validate` (skips the helper), the corruption slips
    // through silently.
    #[test]
    fn run_path_checks_surfaces_unparseable_config_through_full_pipeline() {
        let dir = tempfile::tempdir().expect("tempdir");
        let cfg_dir = dir.path().join("config");
        std::fs::create_dir_all(&cfg_dir).unwrap();
        std::fs::write(cfg_dir.join("config.toml"), "this is = not [ valid toml").unwrap();
        let p = plugin3_core::Paths {
            config_dir: cfg_dir,
            data_dir: dir.path().join("data"),
            runtime_dir: dir.path().join("runtime"),
        };
        let checks = run_path_checks_for(&p);
        let config_check = checks
            .iter()
            .find(|c| c.label == "config_file")
            .expect("config_file label present");
        assert_eq!(
            config_check.status,
            CheckStatus::Fail,
            "unparseable config.toml must FAIL through the full pipeline; \
             a contributor who moves parse-exists out of check_file \
             silently drops this failure mode; got {config_check:?}"
        );
    }
}
