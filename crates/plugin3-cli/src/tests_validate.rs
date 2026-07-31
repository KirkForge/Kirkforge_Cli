use super::*;

fn fake_paths_in(dir: &std::path::Path) -> Paths {
    Paths {
        config_dir: dir.join("cfg"),
        data_dir: dir.join("data"),
        runtime_dir: dir.join("run"),
    }
}

#[test]
fn run_path_checks_passes_on_fresh_tempdir() {
    let dir = tempfile::tempdir().expect("tempdir");
    let p = fake_paths_in(dir.path());
    let checks = commands::config::run_path_checks_for(&p);
    assert!(
        checks
            .iter()
            .all(|c| c.status == commands::config::CheckStatus::Ok),
        "fresh tempdir should pass; failures: {:?}",
        checks
            .iter()
            .filter(|c| c.status == commands::config::CheckStatus::Fail)
            .collect::<Vec<_>>()
    );
    assert_eq!(checks.len(), 8);
}

#[test]
fn run_path_checks_flags_corrupt_config_toml() {
    let dir = tempfile::tempdir().expect("tempdir");
    let p = fake_paths_in(dir.path());
    std::fs::create_dir_all(&p.config_dir).unwrap();
    std::fs::write(p.config_file(), b"this is not valid toml = = =").unwrap();
    let checks = commands::config::run_path_checks_for(&p);
    let cfg_check = checks
        .iter()
        .find(|c| c.label == "config_file")
        .expect("config_file check present");
    assert_eq!(cfg_check.status, commands::config::CheckStatus::Fail);
    assert!(
        cfg_check.detail.contains("parse failed"),
        "detail should explain the parse failure: {}",
        cfg_check.detail
    );
}

#[test]
fn run_path_checks_treats_empty_budget_toml_as_fresh() {
    // ponytail: an empty budget.toml is the post-init state per
    // ADR-0014 (the file exists but no record has landed yet).
    // Treating it as a parse error would make every validate
    // call after `plugin3 init` red.
    let dir = tempfile::tempdir().expect("tempdir");
    let p = fake_paths_in(dir.path());
    std::fs::create_dir_all(p.data_dir.join("logs")).unwrap();
    // B2: budget.toml lives in runtime_dir; create it before seeding.
    std::fs::create_dir_all(&p.runtime_dir).unwrap();
    std::fs::write(p.budget_file(), b"").unwrap();
    let checks = commands::config::run_path_checks_for(&p);
    let budget_check = checks
        .iter()
        .find(|c| c.label == "budget_file")
        .expect("budget_file check present");
    assert_eq!(budget_check.status, commands::config::CheckStatus::Ok);
    assert_eq!(budget_check.detail, "exists+empty");
}

#[test]
fn run_path_checks_leaves_no_permanent_files() {
    // ponytail: the dir probes use NamedTempFile so the validate
    // command is idempotent. A regression that switched to a
    // non-cleaning probe would surface here.
    let dir = tempfile::tempdir().expect("tempdir");
    let p = fake_paths_in(dir.path());
    let _ = commands::config::run_path_checks_for(&p);
    // Only directory probes may create their target dir; no
    // stray files inside any of those dirs.
    for sub in ["cfg", "data", "run"] {
        let entries: Vec<_> = std::fs::read_dir(dir.path().join(sub))
            .map(|it| it.filter_map(Result::ok).collect())
            .unwrap_or_default();
        assert!(
            entries
                .iter()
                .all(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false)),
            "non-dir entry left in {sub}: {entries:?}"
        );
    }
}
