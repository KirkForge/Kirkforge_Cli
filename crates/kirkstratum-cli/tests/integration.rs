//! Integration tests for the `stratum` CLI binary.
//!
//! These tests exercise the compiled binary end-to-end. They are kept separate
//! from the library unit tests so `assert_cmd` can rely on `CARGO_BIN_EXE_stratum`.

use assert_cmd::Command as AssertCommand;
use predicates::str;
use std::io::Write;
use tempfile::NamedTempFile;

#[test]
fn default_run_reaches_orchestrator() {
    AssertCommand::cargo_bin("stratum")
        .unwrap()
        .write_stdin("hello world")
        .arg("run")
        .assert()
        .success()
        .stdout("hello world");
}

#[test]
fn config_validate_exits_ok_on_default() {
    AssertCommand::cargo_bin("stratum")
        .unwrap()
        .arg("config")
        .arg("--validate")
        .assert()
        .success();
}

#[test]
fn config_validate_exits_78_on_unknown_field() {
    let mut file = NamedTempFile::new().unwrap();
    writeln!(file, "bloat_threashold = 0.1").unwrap();

    AssertCommand::cargo_bin("stratum")
        .unwrap()
        .arg("--config")
        .arg(file.path())
        .arg("config")
        .arg("--validate")
        .assert()
        .failure()
        .code(78);
}

#[test]
fn config_validate_exits_78_on_out_of_range_ratio() {
    let mut file = NamedTempFile::new().unwrap();
    writeln!(file, "bloat_threshold = 1.5").unwrap();

    AssertCommand::cargo_bin("stratum")
        .unwrap()
        .arg("--config")
        .arg(file.path())
        .arg("config")
        .arg("--validate")
        .assert()
        .failure()
        .code(78);
}

#[test]
fn config_validate_json_reports_invalid_config() {
    let mut file = NamedTempFile::new().unwrap();
    writeln!(file, "bloat_threshold = 1.5").unwrap();

    AssertCommand::cargo_bin("stratum")
        .unwrap()
        .arg("--config")
        .arg(file.path())
        .arg("--json")
        .arg("config")
        .arg("--validate")
        .assert()
        .failure()
        .code(78)
        .stdout(str::contains("\"valid\": false"))
        .stdout(str::contains("\"error\""))
        .stdout(str::contains("1.5"))
        .stdout(str::contains(file.path().to_string_lossy().as_ref()));
}

#[test]
fn mode_emits_json_when_json_flag_set() {
    AssertCommand::cargo_bin("stratum")
        .unwrap()
        .arg("--json")
        .arg("mode")
        .assert()
        .success()
        .stdout(str::contains("\"mode\""));
}

#[test]
fn version_emits_json_when_json_flag_set() {
    AssertCommand::cargo_bin("stratum")
        .unwrap()
        .arg("--json")
        .arg("version")
        .assert()
        .success()
        .stdout(str::contains("\"version\""));
}

#[test]
fn rules_emits_json_when_json_flag_set() {
    AssertCommand::cargo_bin("stratum")
        .unwrap()
        .arg("--json")
        .arg("rules")
        .assert()
        .success()
        .stdout(str::contains("\"rules\""));
}

#[test]
fn config_emits_json_when_json_flag_set() {
    AssertCommand::cargo_bin("stratum")
        .unwrap()
        .arg("--json")
        .arg("config")
        .assert()
        .success()
        .stdout(str::contains("\"bloat_threshold\""));
}

#[test]
fn config_dump_roundtrips_through_toml() {
    use kirkstratum_core::config::PipelineConfig;

    let out = AssertCommand::cargo_bin("stratum")
        .unwrap()
        .arg("config")
        .assert()
        .success()
        .stdout(str::contains("bloat_threshold"))
        .get_output()
        .stdout
        .clone();
    let text = String::from_utf8(out).expect("config output must be UTF-8");
    let roundtripped = PipelineConfig::from_toml(&text).expect("dumped config must parse");
    assert_eq!(roundtripped, PipelineConfig::default());
}

#[test]
fn config_validate_emits_json_when_json_flag_set() {
    AssertCommand::cargo_bin("stratum")
        .unwrap()
        .arg("--json")
        .arg("config")
        .arg("--validate")
        .assert()
        .success()
        .stdout(str::contains("\"valid\": true"));
}

#[test]
fn config_sources_lists_default() {
    AssertCommand::cargo_bin("stratum")
        .unwrap()
        .arg("config")
        .arg("--sources")
        .assert()
        .success()
        .stdout(str::contains("embedded default"));
}

#[test]
fn config_sources_json_lists_sources() {
    AssertCommand::cargo_bin("stratum")
        .unwrap()
        .arg("--json")
        .arg("config")
        .arg("--sources")
        .assert()
        .success()
        .stdout(str::contains("\"kind\""))
        .stdout(str::contains("\"description\""))
        .stdout(str::contains("\"embedded\""));
}

#[test]
fn config_validate_and_sources_are_mutually_exclusive() {
    AssertCommand::cargo_bin("stratum")
        .unwrap()
        .arg("config")
        .arg("--validate")
        .arg("--sources")
        .assert()
        .failure()
        .code(64)
        .stderr(str::contains("--validate"))
        .stderr(str::contains("--sources"));
}

#[test]
fn oversized_input_exits_65() {
    let mut big = String::with_capacity(1024);
    big.push('{');
    big.push_str(&"x".repeat(1022));
    big.push('}');

    AssertCommand::cargo_bin("stratum")
        .unwrap()
        .arg("--max-input-size")
        .arg("16")
        .arg("run")
        .write_stdin(big)
        .assert()
        .failure()
        .code(65);
}

#[test]
fn missing_file_exits_66() {
    AssertCommand::cargo_bin("stratum")
        .unwrap()
        .arg("apply")
        .arg("/no/such/file.txt")
        .assert()
        .failure()
        .code(66);
}

#[test]
fn help_prints_usage_and_exits_ok() {
    AssertCommand::cargo_bin("stratum")
        .unwrap()
        .arg("--help")
        .assert()
        .success()
        .stdout(str::contains("stratum"))
        .stdout(str::contains("Commands:"));
}

#[test]
fn completion_emits_bash_script() {
    AssertCommand::cargo_bin("stratum")
        .unwrap()
        .arg("completion")
        .arg("bash")
        .assert()
        .success()
        .stdout(str::contains("stratum"));
}

#[test]
fn completion_rejects_unknown_shell() {
    AssertCommand::cargo_bin("stratum")
        .unwrap()
        .arg("completion")
        .arg("foo")
        .assert()
        .failure()
        .code(64);
}

#[test]
fn dry_run_reports_pipeline_plan() {
    AssertCommand::cargo_bin("stratum")
        .unwrap()
        .arg("--dry-run")
        .arg("run")
        .write_stdin("hello world")
        .assert()
        .success()
        .stdout(str::contains("content_type:"))
        .stdout(str::contains("content_type_label:"));
}

#[test]
fn dry_run_json_reports_pipeline_plan() {
    AssertCommand::cargo_bin("stratum")
        .unwrap()
        .arg("--dry-run")
        .arg("--json")
        .arg("run")
        .write_stdin("hello world")
        .assert()
        .success()
        .stdout(str::contains("\"would_offload\""))
        .stdout(str::contains("\"content_type_label\""));
}

#[test]
fn global_mode_flag_emitted_by_mode_subcommand() {
    AssertCommand::cargo_bin("stratum")
        .unwrap()
        .arg("--mode")
        .arg("ultra")
        .arg("mode")
        .assert()
        .success()
        .stdout("ultra\n");
}

#[test]
fn global_mode_flag_emitted_by_rules_subcommand() {
    AssertCommand::cargo_bin("stratum")
        .unwrap()
        .arg("--json")
        .arg("--mode")
        .arg("off")
        .arg("rules")
        .assert()
        .success()
        .stdout(str::contains("\"mode\": \"off\""));
}

#[test]
fn apply_subcommand_mode_wins_over_global_flag() {
    AssertCommand::cargo_bin("stratum")
        .unwrap()
        .arg("--dry-run")
        .arg("--json")
        .arg("--mode")
        .arg("full")
        .arg("apply")
        .arg("--mode")
        .arg("off")
        .write_stdin("hello")
        .assert()
        .success()
        .stdout(str::contains("\"mode\": \"off\""));
}

#[test]
fn run_respects_global_mode_flag_in_dry_run() {
    AssertCommand::cargo_bin("stratum")
        .unwrap()
        .arg("--dry-run")
        .arg("--json")
        .arg("--mode")
        .arg("off")
        .arg("run")
        .write_stdin("hello")
        .assert()
        .success()
        .stdout(str::contains("\"mode\": \"off\""));
}

#[test]
fn unknown_global_mode_returns_usage_error() {
    AssertCommand::cargo_bin("stratum")
        .unwrap()
        .arg("--mode")
        .arg("turbo")
        .arg("run")
        .assert()
        .failure()
        .code(64)
        .stderr(str::contains("turbo"));
}

#[test]
fn unknown_apply_content_type_returns_usage_error() {
    AssertCommand::cargo_bin("stratum")
        .unwrap()
        .arg("apply")
        .arg("--content-type")
        .arg("xml")
        .assert()
        .failure()
        .code(64)
        .stderr(str::contains("xml"));
}

#[test]
fn zero_max_input_size_returns_usage_error() {
    AssertCommand::cargo_bin("stratum")
        .unwrap()
        .arg("--max-input-size")
        .arg("0")
        .arg("run")
        .assert()
        .failure()
        .code(64);
}

#[test]
fn zero_token_budget_returns_usage_error() {
    AssertCommand::cargo_bin("stratum")
        .unwrap()
        .arg("--token-budget")
        .arg("0")
        .arg("run")
        .assert()
        .failure()
        .code(64);
}

#[test]
#[cfg(unix)]
fn run_exits_cleanly_on_broken_pipe() {
    use std::io::Write;
    use std::process::{Command, Stdio};

    let manifest_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let fallback = manifest_dir
        .join("../../target/debug/stratum")
        .canonicalize()
        .ok();
    let candidates = [
        std::env::var("CARGO_BIN_EXE_stratum").ok(),
        fallback.as_ref().map(|p| p.to_string_lossy().into_owned()),
    ];
    let bin = candidates
        .into_iter()
        .flatten()
        .find(|p| std::path::Path::new(p).exists())
        .expect("stratum binary not found in expected locations");
    let mut child = Command::new(bin)
        .arg("run")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn stratum run");

    // Close the read end early so any write to stdout gets BrokenPipe.
    drop(child.stdout.take());

    let mut stdin = child.stdin.take().expect("stdin handle");
    stdin.write_all(b"hello world").unwrap();
    drop(stdin);

    let status = child.wait().expect("wait for child");
    assert!(
        status.success(),
        "expected clean exit on broken pipe, got {status:?}"
    );
}

#[test]
fn hook_session_start_emits_rules() {
    AssertCommand::cargo_bin("stratum")
        .unwrap()
        .arg("rules")
        .assert()
        .success()
        .stdout(str::contains("canonical"));
}

#[test]
fn hook_pre_tool_use_validates_config() {
    AssertCommand::cargo_bin("stratum")
        .unwrap()
        .arg("config")
        .arg("--validate")
        .assert()
        .success();
}

#[test]
fn hook_user_prompt_submit_runs_pipeline() {
    AssertCommand::cargo_bin("stratum")
        .unwrap()
        .arg("run")
        .write_stdin("hello world")
        .assert()
        .success()
        .stdout("hello world");
}

#[test]
fn hook_pre_tool_use_fails_when_config_is_invalid() {
    let mut file = NamedTempFile::new().unwrap();
    writeln!(file, "bloat_threashold = 0.1").unwrap();

    AssertCommand::cargo_bin("stratum")
        .unwrap()
        .arg("--config")
        .arg(file.path())
        .arg("config")
        .arg("--validate")
        .assert()
        .failure()
        .code(78);
}

#[test]
#[cfg(unix)]
fn init_creates_config_and_refuses_overwrite_without_force() {
    use std::fs;

    let dir = tempfile::TempDir::new().unwrap();
    let config_dir = dir.path().join("stratum");

    let mut cmd = AssertCommand::cargo_bin("stratum").unwrap();
    cmd.arg("init")
        .arg("--config-dir")
        .arg(&config_dir)
        .env("HOME", dir.path())
        .env_remove("XDG_CONFIG_HOME");
    cmd.assert().success();

    let path = config_dir.join("pipeline.toml");
    assert!(path.exists());
    let contents = fs::read_to_string(&path).unwrap();
    assert!(contents.contains("bloat_threshold"));

    // Second init without --force should fail.
    let mut cmd = AssertCommand::cargo_bin("stratum").unwrap();
    cmd.arg("init")
        .arg("--config-dir")
        .arg(&config_dir)
        .env("HOME", dir.path())
        .env_remove("XDG_CONFIG_HOME");
    cmd.assert().failure().code(70);

    // With --force it should overwrite.
    fs::write(&path, "bloat_threshold = 0.99").unwrap();
    let mut cmd = AssertCommand::cargo_bin("stratum").unwrap();
    cmd.arg("init")
        .arg("--config-dir")
        .arg(&config_dir)
        .arg("--force")
        .env("HOME", dir.path())
        .env_remove("XDG_CONFIG_HOME");
    cmd.assert().success();
    let contents = fs::read_to_string(&path).unwrap();
    assert!(!contents.contains("0.99"));
}

#[test]
fn missing_explicit_config_exits_config_error() {
    AssertCommand::cargo_bin("stratum")
        .unwrap()
        .arg("--config")
        .arg("/no/such/pipeline.toml")
        .arg("config")
        .arg("--validate")
        .assert()
        .failure()
        .code(78);
}
