// ADR-0015 § Exit codes — integration tests split into submodules.
// Each submodule tests one surface; the helpers here are shared.
use super::*;

// ponytail: tempdirs MUST outlive the subprocess — the child copies
// the path strings into its env at spawn time, but the on-disk
// directory is owned by the TempDir guard. Drop the guard after
// wait_with_output returns. Same pattern as run_hook_subprocess.
fn run_cli_subprocess(
    args: &[&str],
) -> (
    std::process::Output,
    tempfile::TempDir,
    tempfile::TempDir,
    tempfile::TempDir,
) {
    let cfg_dir = tempfile::tempdir().unwrap();
    let data_dir = tempfile::tempdir().unwrap();
    let runtime_dir = tempfile::tempdir().unwrap();
    let out = std::process::Command::new(kf_budget_binary_path())
        .args(args)
        .env("PLUGIN3_CONFIG_DIR", cfg_dir.path())
        .env("PLUGIN3_DATA_DIR", data_dir.path())
        .env("PLUGIN3_RUNTIME_DIR", runtime_dir.path())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .expect("spawn kf-budget");
    (out, cfg_dir, data_dir, runtime_dir)
}

// ponytail: write the corruption before spawning so we only run
// the binary once. run_cli_subprocess returns the tempdir guard
// so we can mutate a file between env-var copy and child exec.
// `dir` is the tempdir to write into ("config" → cfg_dir,
// "data" → data_dir, "runtime" → runtime_dir); `filename` is the
// on-disk name relative to that dir. All three corrupt-* paths
// share this helper so a contributor who breaks one surface
// (e.g. drops `parse_budget_at` from `run_path_checks`) is
// caught by the other tests.
fn run_cli_subprocess_with_corrupt_file(
    args: &[&str],
    body: &[u8],
    dir: &str,
    filename: &str,
) -> (std::process::Output, tempfile::TempDir) {
    let cfg_dir = tempfile::tempdir().unwrap();
    let data_dir = tempfile::tempdir().unwrap();
    let runtime_dir = tempfile::tempdir().unwrap();
    let target = match dir {
        "config" => cfg_dir.path().join(filename),
        "data" => data_dir.path().join(filename),
        "runtime" => runtime_dir.path().join(filename),
        other => panic!("unknown dir slot: {other}"),
    };
    std::fs::write(&target, body).unwrap();
    let out = std::process::Command::new(kf_budget_binary_path())
        .args(args)
        .env("PLUGIN3_CONFIG_DIR", cfg_dir.path())
        .env("PLUGIN3_DATA_DIR", data_dir.path())
        .env("PLUGIN3_RUNTIME_DIR", runtime_dir.path())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .expect("spawn kf-budget");
    (out, cfg_dir)
}

#[cfg(test)]
mod budget;
#[cfg(test)]
mod budget_compact;
#[cfg(test)]
mod config;
#[cfg(test)]
mod report;
#[cfg(test)]
mod report_filters;
#[cfg(test)]
mod report_summary;
