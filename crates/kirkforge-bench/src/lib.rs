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
