use kf_bench::*;
use std::collections::HashMap;
use tempfile::TempDir;

/// RAII guard: removes `key` on construction, restores the prior value
/// (or unsets) on Drop. Local to this test file — kf-bench has no shared
/// test util module and this is the only env-mutation site.
struct EnvGuard {
    key: &'static str,
    old: Option<String>,
}

impl EnvGuard {
    fn remove(key: &'static str) -> Self {
        let old = std::env::var(key).ok();
        std::env::remove_var(key);
        Self { key, old }
    }

    fn set(key: &'static str, val: &str) -> Self {
        let old = std::env::var(key).ok();
        std::env::set_var(key, val);
        Self { key, old }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        if let Some(v) = &self.old {
            std::env::set_var(self.key, v);
        } else {
            std::env::remove_var(self.key);
        }
    }
}

#[test]
fn load_tasks_parses_toml() {
    let dir = TempDir::new().unwrap();
    let task_path = dir.path().join("simple_task.toml");
    std::fs::write(
        &task_path,
        r#"
name = "test_task"
difficulty = "easy"
prompt = "Do something simple"

[setup]
"src/main.rs" = "fn main() {}"

[verify]
type = "command_exits_zero"
command = "true"
"#,
    )
    .unwrap();

    let tasks = load_tasks(dir.path()).unwrap();
    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0].name, "test_task");
    assert_eq!(tasks[0].difficulty, Difficulty::Easy);
    assert_eq!(tasks[0].prompt, "Do something simple");
    assert!(matches!(
        &tasks[0].verify,
        VerifySpec::CommandExitsZero { command } if command == "true"
    ));
    assert_eq!(tasks[0].setup.len(), 1);
    assert_eq!(tasks[0].setup.get("src/main.rs").unwrap(), "fn main() {}");
}

#[test]
fn load_tasks_empty_dir() {
    let dir = TempDir::new().unwrap();
    let tasks = load_tasks(dir.path()).unwrap();
    assert!(tasks.is_empty());
}

#[test]
fn load_tasks_nonexistent_dir() {
    let result = load_tasks(std::path::Path::new("/nonexistent/path/tasks"));
    assert!(result.is_err());
}

#[test]
fn load_tasks_single_file() {
    // WO 14.7: load_tasks accepts a single .toml file so
    // `bench verify-only --tasks <file>` targets one task.
    let dir = TempDir::new().unwrap();
    let task_path = dir.path().join("only_task.toml");
    std::fs::write(
        &task_path,
        r#"
name = "single"
difficulty = "easy"
prompt = "one task"

[verify]
type = "command_exits_zero"
command = "true"
"#,
    )
    .unwrap();
    let tasks = load_tasks(&task_path).unwrap();
    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0].name, "single");
}

#[test]
fn load_tasks_multiple_files() {
    let dir = TempDir::new().unwrap();
    std::fs::write(
        dir.path().join("a_task.toml"),
        r#"
name = "alpha"
difficulty = "easy"
prompt = "First task"

[verify]
type = "command_exits_zero"
command = "echo alpha"
"#,
    )
    .unwrap();
    std::fs::write(
        dir.path().join("b_task.toml"),
        r#"
name = "beta"
difficulty = "hard"
prompt = "Second task"

[verify]
type = "test_passes"
command = "cargo test"
"#,
    )
    .unwrap();

    let tasks = load_tasks(dir.path()).unwrap();
    assert_eq!(tasks.len(), 2);
    assert_eq!(tasks[0].name, "alpha");
    assert_eq!(tasks[1].name, "beta");
}

#[test]
fn verify_command_exits_zero() {
    let dir = TempDir::new().unwrap();
    let task = BenchTask {
        name: "test".into(),
        difficulty: Difficulty::Easy,
        prompt: String::new(),
        setup: HashMap::new(),
        verify: VerifySpec::CommandExitsZero {
            command: "true".into(),
        },
        requires_model: false,
        budget_ceiling: None,
        kf_only: false,
    };
    assert!(verify_task(&task, dir.path()).unwrap());
}

#[test]
fn verify_command_fails() {
    let dir = TempDir::new().unwrap();
    let task = BenchTask {
        name: "test".into(),
        difficulty: Difficulty::Easy,
        prompt: String::new(),
        setup: HashMap::new(),
        verify: VerifySpec::CommandExitsZero {
            command: "false".into(),
        },
        requires_model: false,
        budget_ceiling: None,
        kf_only: false,
    };
    assert!(!verify_task(&task, dir.path()).unwrap());
}

#[test]
fn verify_file_contains() {
    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join("output.txt"), "hello world").unwrap();
    let task = BenchTask {
        name: "test".into(),
        difficulty: Difficulty::Easy,
        prompt: String::new(),
        setup: HashMap::new(),
        verify: VerifySpec::FileContains {
            path: "output.txt".into(),
            contains: "hello".into(),
        },
        requires_model: false,
        budget_ceiling: None,
        kf_only: false,
    };
    assert!(verify_task(&task, dir.path()).unwrap());
}

#[test]
fn verify_file_contains_missing_file() {
    let dir = TempDir::new().unwrap();
    let task = BenchTask {
        name: "test".into(),
        difficulty: Difficulty::Easy,
        prompt: String::new(),
        setup: HashMap::new(),
        verify: VerifySpec::FileContains {
            path: "nonexistent.txt".into(),
            contains: "hello".into(),
        },
        requires_model: false,
        budget_ceiling: None,
        kf_only: false,
    };
    assert!(!verify_task(&task, dir.path()).unwrap());
}

#[test]
fn verify_task_inherits_curated_budget_env() {
    // WO 46.38 phase 1: a leaked parent KF_CODE_BUDGET_CEILING must NOT
    // reach the verify command when the task pins no ceiling — verify_task
    // env_remove()s curated keys before applying the task's own env.
    {
        let dir = TempDir::new().unwrap();
        let _leak = EnvGuard::set(BUDGET_CEILING_ENV, "999999");
        let task = BenchTask {
            name: "leaked-env".into(),
            difficulty: Difficulty::Easy,
            prompt: String::new(),
            setup: HashMap::new(),
            verify: VerifySpec::CommandExitsZero {
                command: format!("test -z \"${BUDGET_CEILING_ENV}\""),
            },
            requires_model: false,
            budget_ceiling: None,
            kf_only: false,
        };
        assert!(
            verify_task(&task, dir.path()).unwrap(),
            "verify command must not see a leaked parent budget ceiling"
        );
    }
    // Phase 2: a task with a budget ceiling must export
    // KF_CODE_BUDGET_CEILING to the verify command. The verify command
    // prints the var; FileContains would need a file, so use
    // CommandExitsZero with a shell test that succeeds only when the env
    // var matches the curated value.
    let dir = TempDir::new().unwrap();
    let _env = EnvGuard::remove(BUDGET_CEILING_ENV);
    let task = BenchTask {
        name: "curated-env".into(),
        difficulty: Difficulty::Easy,
        prompt: String::new(),
        setup: HashMap::new(),
        verify: VerifySpec::CommandExitsZero {
            command: format!("test \"${BUDGET_CEILING_ENV}\" = 4096"),
        },
        requires_model: false,
        budget_ceiling: Some(4096),
        kf_only: false,
    };
    assert!(
        verify_task(&task, dir.path()).unwrap(),
        "verify command should see the curated budget ceiling env"
    );
    assert!(
        std::env::var(BUDGET_CEILING_ENV).is_err(),
        "verify_task must not leak the curated env into the process"
    );
}

#[test]
fn write_report_and_summary() {
    let dir = TempDir::new().unwrap();
    let report = BenchReport {
        model: "test-model".into(),
        timestamp: "2026-01-01T00:00:00Z".into(),
        results: vec![TaskResult {
            task_name: "add_test".into(),
            difficulty: Difficulty::Easy,
            success: true,
            tokens_in: 100,
            tokens_out: 50,
            duration_secs: 12.3,
            cost_usd: 0.001,
            tool_calls: 3,
            compression_passes: 0,
            error: None,
        }],
        summary: BenchSummary {
            success_rate: 1.0,
            total_tokens_in: 100,
            total_tokens_out: 50,
            total_cost_usd: 0.001,
            total_duration_secs: 12.3,
            total_tool_calls: 3,
            tasks_run: 1,
            tasks_passed: 1,
        },
    };

    let json_path = dir.path().join("report.json");
    let md_path = dir.path().join("summary.md");

    write_report(&report, &json_path).unwrap();
    write_markdown_summary(&report, &md_path).unwrap();

    let json_str = std::fs::read_to_string(&json_path).unwrap();
    let parsed: BenchReport = serde_json::from_str(&json_str).unwrap();
    assert_eq!(parsed.model, "test-model");
    assert_eq!(parsed.results.len(), 1);
    assert!(parsed.results[0].success);

    let md_str = std::fs::read_to_string(&md_path).unwrap();
    assert!(md_str.contains("add_test"));
    assert!(md_str.contains("easy"));
    assert!(md_str.contains("Yes"));
    assert!(md_str.contains("1/1"));
}

#[test]
fn bench_summary_from_results() {
    let results = vec![
        TaskResult {
            task_name: "a".into(),
            difficulty: Difficulty::Easy,
            success: true,
            tokens_in: 100,
            tokens_out: 50,
            duration_secs: 10.0,
            cost_usd: 0.01,
            tool_calls: 2,
            compression_passes: 0,
            error: None,
        },
        TaskResult {
            task_name: "b".into(),
            difficulty: Difficulty::Medium,
            success: false,
            tokens_in: 200,
            tokens_out: 100,
            duration_secs: 20.0,
            cost_usd: 0.02,
            tool_calls: 5,
            compression_passes: 0,
            error: Some("timeout".into()),
        },
    ];
    let summary = BenchSummary::from_results(&results);
    assert_eq!(summary.tasks_run, 2);
    assert_eq!(summary.tasks_passed, 1);
    assert!((summary.success_rate - 0.5).abs() < 0.001);
    assert_eq!(summary.total_tokens_in, 300);
    assert_eq!(summary.total_tokens_out, 150);
    assert!((summary.total_cost_usd - 0.03).abs() < 0.001);
    assert_eq!(summary.total_tool_calls, 7);
}
