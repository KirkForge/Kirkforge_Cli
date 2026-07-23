//! Task-benchmark harness for measuring agent capability.
//!
//! Runs representative coding tasks end-to-end against a headless kirkforge
//! session and collects metrics: success rate, tokens, time, cost, tool calls.
//!
//! This crate contains the data types, TOML task parsing, verification, and
//! report writing. The headless session execution lives in the main kirkforge
//! crate (src/session/bench.rs) because it depends on the executor.
//!
//! ponytail: TOML task definitions + headless session execution. The upgrade
//! path is a leaderboard, multi-model comparison, and CI benchmark deltas.

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

// ── Task format ──

/// Difficulty level for a benchmark task.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Difficulty {
    Easy,
    Medium,
    Hard,
}

/// How to verify a task completed successfully.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum VerifySpec {
    /// Run a test command and check exit 0.
    TestPasses { command: String },
    /// Check a file contains a string.
    FileContains { path: String, contains: String },
    /// Run a command and check exit 0.
    CommandExitsZero { command: String },
}

/// A single benchmark task definition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BenchTask {
    pub name: String,
    pub difficulty: Difficulty,
    pub prompt: String,
    #[serde(default)]
    pub setup: HashMap<String, String>,
    pub verify: VerifySpec,
}

/// Result of running a single benchmark task.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskResult {
    pub task_name: String,
    pub difficulty: Difficulty,
    pub success: bool,
    pub tokens_in: u64,
    pub tokens_out: u64,
    pub duration_secs: f64,
    pub cost_usd: f64,
    pub tool_calls: u32,
    pub error: Option<String>,
}

/// Summary statistics across all task results.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BenchSummary {
    pub success_rate: f64,
    pub total_tokens_in: u64,
    pub total_tokens_out: u64,
    pub total_cost_usd: f64,
    pub total_duration_secs: f64,
    pub total_tool_calls: u32,
    pub tasks_run: usize,
    pub tasks_passed: usize,
}

/// Full benchmark report.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BenchReport {
    pub model: String,
    pub timestamp: String,
    pub results: Vec<TaskResult>,
    pub summary: BenchSummary,
}

// ── Task loading ──

/// Parse all `.toml` task files in a directory.
pub fn load_tasks(dir: &Path) -> Result<Vec<BenchTask>> {
    let mut tasks = Vec::new();
    if !dir.is_dir() {
        anyhow::bail!("task directory does not exist: {}", dir.display());
    }
    let mut entries: Vec<_> = std::fs::read_dir(dir)?
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|ext| ext == "toml"))
        .collect();
    entries.sort_by_key(|e| e.file_name());

    for entry in entries {
        let content = std::fs::read_to_string(entry.path())?;
        let task: BenchTask = toml::from_str(&content)?;
        tasks.push(task);
    }
    Ok(tasks)
}

// ── Verification ──

/// Verify a task completed successfully.
pub fn verify_task(task: &BenchTask, sandbox: &Path) -> Result<bool> {
    match &task.verify {
        VerifySpec::TestPasses { command } => {
            let status = std::process::Command::new("sh")
                .arg("-c")
                .arg(command)
                .current_dir(sandbox)
                .env("CARGO_TERM_COLOR", "never")
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()?;
            Ok(status.success())
        }
        VerifySpec::FileContains { path, contains } => {
            let full_path = sandbox.join(path);
            if !full_path.exists() {
                return Ok(false);
            }
            let content = std::fs::read_to_string(&full_path)?;
            Ok(content.contains(contains))
        }
        VerifySpec::CommandExitsZero { command } => {
            let status = std::process::Command::new("sh")
                .arg("-c")
                .arg(command)
                .current_dir(sandbox)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()?;
            Ok(status.success())
        }
    }
}

// ── Summary/reports ──

impl BenchSummary {
    pub fn from_results(results: &[TaskResult]) -> Self {
        let tasks_run = results.len();
        let tasks_passed = results.iter().filter(|r| r.success).count();
        let success_rate = if tasks_run > 0 {
            tasks_passed as f64 / tasks_run as f64
        } else {
            0.0
        };
        Self {
            success_rate,
            total_tokens_in: results.iter().map(|r| r.tokens_in).sum(),
            total_tokens_out: results.iter().map(|r| r.tokens_out).sum(),
            total_cost_usd: results.iter().map(|r| r.cost_usd).sum(),
            total_duration_secs: results.iter().map(|r| r.duration_secs).sum(),
            total_tool_calls: results.iter().map(|r| r.tool_calls).sum(),
            tasks_run,
            tasks_passed,
        }
    }
}

/// Write a JSON report to disk.
pub fn write_report(report: &BenchReport, path: &Path) -> Result<()> {
    let json = serde_json::to_string_pretty(report)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, json)?;
    Ok(())
}

/// Delta for a single task between baseline and current.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskDelta {
    pub name: String,
    pub difficulty: Difficulty,
    pub baseline_success: bool,
    pub current_success: bool,
    pub delta_tokens_in: i64,
    pub delta_tokens_out: i64,
    pub delta_duration_secs: f64,
    pub delta_cost_usd: f64,
}

/// Aggregate delta report comparing two bench runs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeltaReport {
    pub baseline_model: String,
    pub current_model: String,
    pub baseline_timestamp: String,
    pub current_timestamp: String,
    pub tasks: Vec<TaskDelta>,
    pub success_rate_delta: f64,
    pub total_tokens_in_delta: i64,
    pub total_tokens_out_delta: i64,
    pub total_cost_delta_usd: f64,
}

/// Task metadata for listing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskInfo {
    pub name: String,
    pub difficulty: Difficulty,
    pub verify_type: String,
}

// ── Comparison ──

/// Compare two bench reports, producing a delta report.
pub fn compare_reports(baseline: &BenchReport, current: &BenchReport) -> DeltaReport {
    let baseline_map: HashMap<String, &TaskResult> = baseline
        .results
        .iter()
        .map(|r| (r.task_name.clone(), r))
        .collect();
    let current_map: HashMap<String, &TaskResult> = current
        .results
        .iter()
        .map(|r| (r.task_name.clone(), r))
        .collect();

    let mut all_names: Vec<String> = baseline_map
        .keys()
        .chain(current_map.keys())
        .cloned()
        .collect();
    all_names.sort();
    all_names.dedup();

    let mut tasks = Vec::new();
    for name in &all_names {
        let b = baseline_map.get(name);
        let c = current_map.get(name);
        let (b_success, b_in, b_out, b_dur, b_cost) = match b {
            Some(r) => (
                r.success,
                r.tokens_in as i64,
                r.tokens_out as i64,
                r.duration_secs,
                r.cost_usd,
            ),
            None => (false, 0, 0, 0.0, 0.0),
        };
        let (c_success, c_in, c_out, c_dur, c_cost) = match c {
            Some(r) => (
                r.success,
                r.tokens_in as i64,
                r.tokens_out as i64,
                r.duration_secs,
                r.cost_usd,
            ),
            None => (false, 0, 0, 0.0, 0.0),
        };
        tasks.push(TaskDelta {
            name: name.clone(),
            difficulty: c
                .map(|r| r.difficulty)
                .or(b.map(|r| r.difficulty))
                .unwrap_or(Difficulty::Easy),
            baseline_success: b_success,
            current_success: c_success,
            delta_tokens_in: c_in - b_in,
            delta_tokens_out: c_out - b_out,
            delta_duration_secs: c_dur - b_dur,
            delta_cost_usd: c_cost - b_cost,
        });
    }

    let baseline_passed = baseline.results.iter().filter(|r| r.success).count();
    let current_passed = current.results.iter().filter(|r| r.success).count();
    let baseline_rate = if baseline.summary.tasks_run > 0 {
        baseline_passed as f64 / baseline.summary.tasks_run as f64
    } else {
        0.0
    };
    let current_rate = if current.summary.tasks_run > 0 {
        current_passed as f64 / current.summary.tasks_run as f64
    } else {
        0.0
    };

    DeltaReport {
        baseline_model: baseline.model.clone(),
        current_model: current.model.clone(),
        baseline_timestamp: baseline.timestamp.clone(),
        current_timestamp: current.timestamp.clone(),
        tasks,
        success_rate_delta: current_rate - baseline_rate,
        total_tokens_in_delta: current.summary.total_tokens_in as i64
            - baseline.summary.total_tokens_in as i64,
        total_tokens_out_delta: current.summary.total_tokens_out as i64
            - baseline.summary.total_tokens_out as i64,
        total_cost_delta_usd: current.summary.total_cost_usd - baseline.summary.total_cost_usd,
    }
}

/// Write a markdown delta table to disk.
pub fn write_markdown_delta(delta: &DeltaReport, path: &Path) -> Result<()> {
    let baseline_rate = delta.success_rate_delta
        + if !delta.tasks.is_empty() {
            let baseline_passed = delta.tasks.iter().filter(|t| t.baseline_success).count();
            baseline_passed as f64 / delta.tasks.len() as f64
        } else {
            0.0
        };
    let current_rate = baseline_rate + delta.success_rate_delta;

    let mut md = String::new();
    md.push_str(&format!(
        "# Benchmark Delta: {} → {}\n\n",
        delta.baseline_model, delta.current_model
    ));
    md.push_str(&format!(
        "**Baseline:** {} | **Current:** {}\n\n",
        delta.baseline_timestamp, delta.current_timestamp
    ));
    md.push_str(&format!(
        "**Success rate:** {:.0}% → {:.0}% (Δ{:+.0}%)\n\n",
        baseline_rate * 100.0,
        current_rate * 100.0,
        delta.success_rate_delta * 100.0
    ));
    md.push_str(&format!(
        "- Δtokens in: {:+}\n- Δtokens out: {:+}\n- Δcost: ${:+.4}\n\n",
        delta.total_tokens_in_delta, delta.total_tokens_out_delta, delta.total_cost_delta_usd,
    ));
    md.push_str("| Task | Difficulty | Baseline | Current | Δtokens_in | Δduration | Δcost |\n");
    md.push_str("|------|-----------|----------|---------|------------|-----------|-------|\n");
    for t in &delta.tasks {
        let diff_str = match t.difficulty {
            Difficulty::Easy => "easy",
            Difficulty::Medium => "medium",
            Difficulty::Hard => "hard",
        };
        md.push_str(&format!(
            "| {} | {} | {} | {} | {:+} | {:+.1}s | {:+.4} |\n",
            t.name,
            diff_str,
            if t.baseline_success { "Yes" } else { "No" },
            if t.current_success { "Yes" } else { "No" },
            t.delta_tokens_in,
            t.delta_duration_secs,
            t.delta_cost_usd,
        ));
    }

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, md)?;
    Ok(())
}

// ── Listing and verification ──

/// List all tasks in a directory, returning metadata without running anything.
pub fn list_tasks(dir: &Path) -> Result<Vec<TaskInfo>> {
    let tasks = load_tasks(dir)?;
    Ok(tasks
        .iter()
        .map(|t| TaskInfo {
            name: t.name.clone(),
            difficulty: t.difficulty,
            verify_type: match &t.verify {
                VerifySpec::TestPasses { .. } => "test_passes".to_string(),
                VerifySpec::FileContains { .. } => "file_contains".to_string(),
                VerifySpec::CommandExitsZero { .. } => "command_exits_zero".to_string(),
            },
        })
        .collect())
}

/// Run verification only (no LLM) for a task. Returns the TaskResult.
pub fn verify_only(task: &BenchTask, sandbox_path: &Path) -> TaskResult {
    for (rel_path, content) in &task.setup {
        let file_path = sandbox_path.join(rel_path);
        if let Some(parent) = file_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(&file_path, content);
    }

    let success = verify_task(task, sandbox_path).unwrap_or(false);
    TaskResult {
        task_name: task.name.clone(),
        difficulty: task.difficulty,
        success,
        tokens_in: 0,
        tokens_out: 0,
        duration_secs: 0.0,
        cost_usd: 0.0,
        tool_calls: 0,
        error: if success {
            None
        } else {
            Some("verification failed".to_string())
        },
    }
}

/// Write a markdown summary table to disk.
pub fn write_markdown_summary(report: &BenchReport, path: &Path) -> Result<()> {
    let mut md = String::new();
    md.push_str(&format!("# Benchmark Report: {}\n\n", report.model));
    md.push_str(&format!("**Timestamp:** {}\n\n", report.timestamp));
    md.push_str(&format!(
        "**Summary:** {}/{} tasks passed ({:.0}% success rate)\n\n",
        report.summary.tasks_passed,
        report.summary.tasks_run,
        report.summary.success_rate * 100.0
    ));
    md.push_str(&format!(
        "- Total tokens in: {}\n- Total tokens out: {}\n- Total cost: ${:.4}\n- Total time: {:.1}s\n- Total tool calls: {}\n\n",
        report.summary.total_tokens_in,
        report.summary.total_tokens_out,
        report.summary.total_cost_usd,
        report.summary.total_duration_secs,
        report.summary.total_tool_calls,
    ));
    md.push_str("| Task | Difficulty | Success | Tokens In | Tokens Out | Time (s) | Cost ($) | Tool Calls |\n");
    md.push_str("|------|-----------|---------|-----------|------------|----------|---------|------------|\n");
    for r in &report.results {
        let success_str = if r.success { "Yes" } else { "No" };
        let diff_str = match r.difficulty {
            Difficulty::Easy => "easy",
            Difficulty::Medium => "medium",
            Difficulty::Hard => "hard",
        };
        md.push_str(&format!(
            "| {} | {} | {} | {} | {} | {:.1} | {:.4} | {} |\n",
            r.task_name,
            diff_str,
            success_str,
            r.tokens_in,
            r.tokens_out,
            r.duration_secs,
            r.cost_usd,
            r.tool_calls,
        ));
    }

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, md)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_report(success: bool, tokens_in: u64, tokens_out: u64, cost: f64) -> BenchReport {
        BenchReport {
            model: "test-model".to_string(),
            timestamp: "2025-01-01T00:00:00".to_string(),
            results: vec![TaskResult {
                task_name: "task1".to_string(),
                difficulty: Difficulty::Easy,
                success,
                tokens_in,
                tokens_out,
                duration_secs: 1.0,
                cost_usd: cost,
                tool_calls: 1,
                error: None,
            }],
            summary: BenchSummary {
                success_rate: if success { 1.0 } else { 0.0 },
                total_tokens_in: tokens_in,
                total_tokens_out: tokens_out,
                total_cost_usd: cost,
                total_duration_secs: 1.0,
                total_tool_calls: 1,
                tasks_run: 1,
                tasks_passed: if success { 1 } else { 0 },
            },
        }
    }

    #[test]
    fn test_compare_reports_regression() {
        let baseline = sample_report(true, 100, 50, 0.01);
        let current = sample_report(true, 100, 50, 0.01);
        let delta = compare_reports(&baseline, &current);
        assert_eq!(delta.tasks.len(), 1);
        assert_eq!(delta.tasks[0].delta_tokens_in, 0);
        assert_eq!(delta.tasks[0].delta_tokens_out, 0);
        assert!((delta.tasks[0].delta_cost_usd - 0.0).abs() < f64::EPSILON);
        assert!((delta.success_rate_delta - 0.0).abs() < f64::EPSILON);
        assert_eq!(delta.total_tokens_in_delta, 0);
        assert_eq!(delta.total_tokens_out_delta, 0);
        assert!((delta.total_cost_delta_usd - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_compare_reports_improvement() {
        let baseline = sample_report(false, 200, 100, 0.05);
        let mut current = sample_report(true, 150, 75, 0.03);
        current.results[0].success = true;
        current.results[0].tokens_in = 150;
        current.results[0].tokens_out = 75;
        current.results[0].cost_usd = 0.03;
        current.summary.total_tokens_in = 150;
        current.summary.total_tokens_out = 75;
        current.summary.total_cost_usd = 0.03;
        current.summary.success_rate = 1.0;
        current.summary.tasks_passed = 1;

        let delta = compare_reports(&baseline, &current);
        assert!(delta.tasks[0].current_success);
        assert!(!delta.tasks[0].baseline_success);
        assert_eq!(delta.tasks[0].delta_tokens_in, -50);
        assert_eq!(delta.tasks[0].delta_tokens_out, -25);
        assert!((delta.success_rate_delta - 1.0).abs() < f64::EPSILON);
        assert_eq!(delta.total_tokens_in_delta, -50);
    }

    #[test]
    fn test_compare_reports_new_task() {
        let baseline = sample_report(true, 100, 50, 0.01);
        let mut current = baseline.clone();
        current.results.push(TaskResult {
            task_name: "task2".to_string(),
            difficulty: Difficulty::Medium,
            success: true,
            tokens_in: 80,
            tokens_out: 40,
            duration_secs: 2.0,
            cost_usd: 0.02,
            tool_calls: 2,
            error: None,
        });
        current.summary.tasks_run = 2;
        current.summary.tasks_passed = 2;
        current.summary.success_rate = 1.0;
        current.summary.total_tokens_in = 180;
        current.summary.total_tokens_out = 90;
        current.summary.total_cost_usd = 0.03;

        let delta = compare_reports(&baseline, &current);
        assert_eq!(delta.tasks.len(), 2);
        let task2 = delta.tasks.iter().find(|t| t.name == "task2").unwrap();
        assert!(!task2.baseline_success);
        assert!(task2.current_success);
        assert_eq!(task2.difficulty, Difficulty::Medium);
        assert_eq!(task2.delta_tokens_in, 80);
    }

    #[test]
    fn test_list_tasks_loads_toml() {
        let dir = tempfile::tempdir().unwrap();
        let task_toml = r#"
            name = "test_task"
            difficulty = "easy"
            prompt = "do the thing"

            [verify]
            type = "command_exits_zero"
            command = "true"
        "#;
        std::fs::write(dir.path().join("test_task.toml"), task_toml).unwrap();
        let infos = list_tasks(dir.path()).unwrap();
        assert_eq!(infos.len(), 1);
        assert_eq!(infos[0].name, "test_task");
        assert_eq!(infos[0].difficulty, Difficulty::Easy);
        assert_eq!(infos[0].verify_type, "command_exits_zero");
    }

    #[test]
    fn test_verify_only_success() {
        let dir = tempfile::tempdir().unwrap();
        let task = BenchTask {
            name: "success_task".to_string(),
            difficulty: Difficulty::Easy,
            prompt: "unused".to_string(),
            setup: HashMap::new(),
            verify: VerifySpec::CommandExitsZero {
                command: "true".to_string(),
            },
        };
        let result = verify_only(&task, dir.path());
        assert!(result.success);
        assert!(result.error.is_none());
    }

    #[test]
    fn test_verify_only_failure() {
        let dir = tempfile::tempdir().unwrap();
        let task = BenchTask {
            name: "failure_task".to_string(),
            difficulty: Difficulty::Medium,
            prompt: "unused".to_string(),
            setup: HashMap::new(),
            verify: VerifySpec::CommandExitsZero {
                command: "false".to_string(),
            },
        };
        let result = verify_only(&task, dir.path());
        assert!(!result.success);
        assert_eq!(result.error, Some("verification failed".to_string()));
    }
}
