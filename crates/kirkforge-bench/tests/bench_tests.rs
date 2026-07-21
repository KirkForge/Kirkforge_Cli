use kirkforge_bench::*;
use std::collections::HashMap;
use tempfile::TempDir;

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
    };
    assert!(!verify_task(&task, dir.path()).unwrap());
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
