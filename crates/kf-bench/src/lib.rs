//! Task-benchmark harness for measuring agent capability.
//!
//! Runs representative coding tasks end-to-end against a headless kf-code
//! session and collects metrics: success rate, tokens, time, cost, tool calls.
//!
//! This crate contains the data types, TOML task parsing, verification, and
//! report writing. The headless session execution lives in the main kf-code
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

/// Environment variable exported to the agent process when a task
/// sets `budget_ceiling`. The bench runner reads this on the budget
/// guard (via `src/session/config/env_overrides.rs`) to pin the token
/// budget ceiling for a single run. See WO 14.7 / ADR-0066.
pub const BUDGET_CEILING_ENV: &str = "KF_CODE_BUDGET_CEILING";

/// A single benchmark task definition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BenchTask {
    pub name: String,
    pub difficulty: Difficulty,
    pub prompt: String,
    #[serde(default)]
    pub setup: HashMap<String, String>,
    pub verify: VerifySpec,
    /// When true, the verify spec can only be evaluated *after* the
    /// model has run (e.g., the setup intentionally has a failing test
    /// the model must fix, or the "verify" is the model's review
    /// output which no command can check). `verify_only` skips these
    /// tasks and reports them as SKIP; `bench run` runs them normally.
    /// Default false so existing tasks are unaffected. See WO 9.9.
    #[serde(default)]
    pub requires_model: bool,
    /// Optional token-budget ceiling exported to the agent as
    /// `KF_CODE_BUDGET_CEILING` for this run. `None` (default) leaves
    /// the budget at the config default. The Token Budget Challenge
    /// (WO 14.7) sets this and runs the task 5x under descending
    /// ceilings (128k/64k/32k/16k/8k). See ADR-0066.
    #[serde(default)]
    pub budget_ceiling: Option<usize>,
    /// When true, the task exercises a kf-code-only tool (workflow_run,
    /// lsp_query, stratum_run, budget_status) that no external agent can
    /// invoke. The cross-tool subset excludes these; `bench run` still
    /// runs them. See WO 39.1.
    #[serde(default)]
    pub kf_only: bool,
}

impl BenchTask {
    /// Return the env var assignment `(BUDGET_CEILING_ENV, n)` if the
    /// task pins a budget ceiling, else `None`. The runner applies
    /// this to the agent's environment before invoking the model.
    pub fn budget_env(&self) -> Option<(&'static str, usize)> {
        self.budget_ceiling.map(|n| (BUDGET_CEILING_ENV, n))
    }
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
    /// Number of Stratum/budget compression passes observed during the
    /// run (`TurnEvent::CompactionReport` count). The Token Budget
    /// Challenge records this per ceiling level (WO 14.7). Defaults to
    /// 0 so existing serialized reports parse without the field.
    #[serde(default)]
    pub compression_passes: u32,
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

/// Parse `.toml` task files from a directory, or a single `.toml` file.
///
/// When `path` is a directory, every `.toml` file inside is loaded
/// (sorted by filename). When `path` is a single `.toml` file, just
/// that file is loaded. This lets `bench verify-only --tasks <file>`
/// target one task without filtering the whole directory.
pub fn load_tasks(path: &Path) -> Result<Vec<BenchTask>> {
    let mut tasks = Vec::new();
    if path.is_file() {
        let content = std::fs::read_to_string(path)?;
        let task: BenchTask = toml::from_str(&content)?;
        tasks.push(task);
        return Ok(tasks);
    }
    if !path.is_dir() {
        anyhow::bail!("task directory does not exist: {}", path.display());
    }
    let mut entries: Vec<_> = std::fs::read_dir(path)?
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
///
/// The verify command inherits the calling process environment plus the
/// task's curated env (`budget_env()`, currently `KF_CODE_BUDGET_CEILING`
/// when set) so verification runs under the same env conditions the agent
/// operated under, regardless of process-env drift between run and verify.
/// Curated var names are scrubbed from the inherited env first — a leaked
/// parent value must not silently affect the gate (WO 46.38).
pub fn verify_task(task: &BenchTask, sandbox: &Path) -> Result<bool> {
    let curated = task.budget_env();
    match &task.verify {
        VerifySpec::TestPasses { command } => {
            let mut cmd = std::process::Command::new("sh");
            cmd.arg("-c")
                .arg(command)
                .current_dir(sandbox)
                .env("CARGO_TERM_COLOR", "never")
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null());
            // WO 46.38: scrub inherited values first so the gate doesn't
            // depend on parent-env state. Keys must match budget_env().
            cmd.env_remove(BUDGET_CEILING_ENV);
            if let Some((k, v)) = curated {
                cmd.env(k, v.to_string());
            }
            let status = cmd.status()?;
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
            let mut cmd = std::process::Command::new("sh");
            cmd.arg("-c")
                .arg(command)
                .current_dir(sandbox)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null());
            // WO 46.38: scrub inherited values first so the gate doesn't
            // depend on parent-env state. Keys must match budget_env().
            cmd.env_remove(BUDGET_CEILING_ENV);
            if let Some((k, v)) = curated {
                cmd.env(k, v.to_string());
            }
            let status = cmd.status()?;
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
    pub baseline_success_rate: f64,
    pub current_success_rate: f64,
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
    /// Whether the task uses a kf-code-only tool (excluded from
    /// cross-tool subsets). See WO 39.1.
    pub kf_only: bool,
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
            // A name appears in `all_names` only if it's in the baseline OR
            // current report (the list is their union), so at least one side
            // always carries the difficulty. The `Difficulty::Easy` fallback
            // is therefore a defensive unreachable default, not a real guess.
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
        baseline_success_rate: baseline_rate,
        current_success_rate: current_rate,
        success_rate_delta: current_rate - baseline_rate,
        total_tokens_in_delta: current.summary.total_tokens_in as i64
            - baseline.summary.total_tokens_in as i64,
        total_tokens_out_delta: current.summary.total_tokens_out as i64
            - baseline.summary.total_tokens_out as i64,
        total_cost_delta_usd: current.summary.total_cost_usd - baseline.summary.total_cost_usd,
    }
}

/// Result of comparing two reports with a regression threshold (WO 10.9).
///
/// `regression_detected` is `true` when the success rate dropped by more
/// than `threshold` percentage points (e.g. threshold=0.10 means a drop
/// from 80% to 69% is a regression, but 80%→71% is not). The delta
/// report is always included so the caller can print the details
/// regardless of the pass/fail outcome.
#[derive(Debug, Clone)]
pub struct CompareResult {
    pub delta: DeltaReport,
    pub regression_detected: bool,
    pub threshold: f64,
}

/// Compare two bench reports and flag a regression when the success
/// rate drops by more than `threshold` (a fraction: 0.10 = 10
/// percentage points). The CI regression gate (WO 10.9) uses this to
/// fail the `bench-pr-delta` job when a PR drops the bench success rate.
pub fn compare_with_threshold(
    baseline: &BenchReport,
    current: &BenchReport,
    threshold: f64,
) -> CompareResult {
    let delta = compare_reports(baseline, current);
    let regression_detected = delta.success_rate_delta < -threshold;
    CompareResult {
        delta,
        regression_detected,
        threshold,
    }
}

/// Write a markdown delta table to disk.
pub fn write_markdown_delta(delta: &DeltaReport, path: &Path) -> Result<()> {
    // The rates come from the per-report summaries (carried in
    // `DeltaReport` by `compare_reports`), NOT from recomputing over the
    // union task set. A task present only in current has no baseline
    // result, so counting it as a baseline failure would shift both
    // rendered rates away from the true summaries — and away from the
    // regression decision `compare_with_threshold` makes. See WO 43.39.
    let baseline_rate = delta.baseline_success_rate;
    let current_rate = delta.current_success_rate;

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
            kf_only: t.kf_only,
        })
        .collect())
}

/// Materialize each task's setup files into a subdirectory of `out_dir`
/// so an external agent (Codex, Claude Code, opencode) can run against
/// the same starting state as kf-code. One subdirectory per task, named
/// after the task. The prompt is written to `PROMPT.txt` inside each
/// subdir. Skips `kf_only` tasks when `include_kf_only` is false (the
/// cross-tool subset). Returns the count of exported task dirs. See
/// WO 39.1.
pub fn export_tasks(dir: &Path, out_dir: &Path, include_kf_only: bool) -> Result<usize> {
    let tasks = load_tasks(dir)?;
    let mut count = 0;
    for task in &tasks {
        if task.kf_only && !include_kf_only {
            continue;
        }
        let task_dir = out_dir.join(&task.name);
        std::fs::create_dir_all(&task_dir)?;
        for (rel_path, content) in &task.setup {
            let file_path = task_dir.join(rel_path);
            if let Some(parent) = file_path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&file_path, content)?;
        }
        std::fs::write(task_dir.join("PROMPT.txt"), &task.prompt)?;
        count += 1;
    }
    Ok(count)
}

/// Run verification only (no LLM) for a task. Returns the TaskResult.
pub fn verify_only(task: &BenchTask, sandbox_path: &Path) -> TaskResult {
    // A task that requires the model cannot be verified against the
    // unedited setup — report SKIP so the operator sees it was
    // intentionally skipped, not silently passed or failed.
    if task.requires_model {
        return TaskResult {
            task_name: task.name.clone(),
            difficulty: task.difficulty,
            success: true, // SKIP counts as "not broken", not as "verified"
            tokens_in: 0,
            tokens_out: 0,
            duration_secs: 0.0,
            cost_usd: 0.0,
            tool_calls: 0,
            compression_passes: 0,
            error: Some("skipped (requires model)".to_string()),
        };
    }

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
        compression_passes: 0,
        error: if success {
            None
        } else {
            Some("verification failed".to_string())
        },
    }
}

/// Format a token count as a compact human-readable string.
/// Values below 1000 are shown as-is; >=1000 use a `k` suffix with one decimal.
fn format_tokens(n: u64) -> String {
    if n >= 1000 {
        format!("{:.1}k", n as f64 / 1000.0)
    } else {
        n.to_string()
    }
}

/// Produce a markdown comparison table across multiple model reports.
///
/// Reports are sorted by success rate (descending). An empty slice yields
/// the literal string `"No reports to compare"`.
pub fn write_model_comparison(reports: &[BenchReport]) -> String {
    if reports.is_empty() {
        return "No reports to compare".to_string();
    }

    let mut sorted: Vec<&BenchReport> = reports.iter().collect();
    sorted.sort_by(|a, b| {
        b.summary
            .success_rate
            .partial_cmp(&a.summary.success_rate)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    let mut md = String::new();
    md.push_str("# Model Comparison\n\n");
    md.push_str("| Model | Tasks Passed | Success Rate | Avg Tokens In | Avg Tokens Out | Avg Duration | Total Cost |\n");
    md.push_str("|-------|-------------|-------------|---------------|----------------|-------------|------------|\n");
    for r in &sorted {
        let avg_in = if r.summary.tasks_run > 0 {
            r.summary.total_tokens_in / r.summary.tasks_run as u64
        } else {
            0
        };
        let avg_out = if r.summary.tasks_run > 0 {
            r.summary.total_tokens_out / r.summary.tasks_run as u64
        } else {
            0
        };
        let avg_dur = if r.summary.tasks_run > 0 {
            r.summary.total_duration_secs / r.summary.tasks_run as f64
        } else {
            0.0
        };
        md.push_str(&format!(
            "| {} | {}/{} | {:.0}% | {} | {} | {:.1}s | ${:.4} |\n",
            r.model,
            r.summary.tasks_passed,
            r.summary.tasks_run,
            r.summary.success_rate * 100.0,
            format_tokens(avg_in),
            format_tokens(avg_out),
            avg_dur,
            r.summary.total_cost_usd,
        ));
    }
    md
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

/// One row of the Token Budget Challenge report: the six metrics
/// recorded for a single ceiling level (WO 14.7 / ADR-0066). The
/// runner runs the same task 5x under descending ceilings and
/// collects one entry per run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BudgetChallengeEntry {
    pub ceiling: usize,
    pub success: bool,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub compression_passes: u32,
    pub cost_usd: f64,
}

/// Token Budget Challenge report: the task name, the model, and one
/// entry per ceiling level. `write_budget_challenge_report` emits
/// the markdown scoreboard table.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BudgetChallengeReport {
    pub task_name: String,
    pub model: String,
    pub entries: Vec<BudgetChallengeEntry>,
}

// ── Cross-tool comparison (WO 32.6) ──

/// Result of running a single benchmark task on an external tool
/// (Codex, Claude Code, etc.) for the cross-tool benchmark. Unlike
/// `TaskResult` (kf-code internal metrics), this captures only the
/// fields an external tool's report can reliably expose: the tool
/// name, the task, the context budget pinned for the run, the total
/// tokens consumed, the turn count, whether the task succeeded, and
/// the wall-clock duration. See WO 32.6.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExternalToolReport {
    pub tool_name: String,
    pub task_name: String,
    /// Context budget pinned for the run, in tokens (e.g. 131072 = 128k).
    pub context_budget: usize,
    pub tokens_consumed: u64,
    pub turns_taken: u32,
    pub success: bool,
    pub wall_clock_secs: f64,
}

/// A batch of cross-tool reports serialized as a JSON file. This is
/// the import/export format for the runner script (`scripts/run-cross-tool-bench.sh`)
/// and the external-tool templates it emits.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExternalToolReportBatch {
    pub reports: Vec<ExternalToolReport>,
}

/// Write a batch of external-tool reports to disk as pretty JSON.
pub fn write_external_reports(batch: &ExternalToolReportBatch, path: &Path) -> Result<()> {
    let json = serde_json::to_string_pretty(batch)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, json)?;
    Ok(())
}

/// Load a batch of external-tool reports from a JSON file.
pub fn load_external_reports(path: &Path) -> Result<ExternalToolReportBatch> {
    let content = std::fs::read_to_string(path)?;
    let batch: ExternalToolReportBatch = serde_json::from_str(&content)?;
    Ok(batch)
}

/// Build the cross-tool comparison markdown table. Reports are
/// grouped by `task_name`; within each task, rows are ordered by
/// `context_budget` descending then by `tool_name` for a stable layout.
/// An empty slice yields the literal `"No cross-tool reports to compare"`.
///
/// The table columns mirror WO 32.6: tool, task, budget, tokens,
/// turns, success, wall-clock. This is the raw comparison view; the
/// thesis-validation writeup lives in `docs/benchmarks/cross-tool-2026-08.md`.
pub fn compare_cross_tool(reports: &[ExternalToolReport]) -> String {
    if reports.is_empty() {
        return "No cross-tool reports to compare".to_string();
    }

    let mut sorted: Vec<&ExternalToolReport> = reports.iter().collect();
    sorted.sort_by(|a, b| {
        a.task_name
            .cmp(&b.task_name)
            .then(b.context_budget.cmp(&a.context_budget))
            .then(a.tool_name.cmp(&b.tool_name))
    });

    let mut md = String::new();
    md.push_str("# Cross-Tool Comparison\n\n");
    md.push_str("| Tool | Task | Budget | Tokens | Turns | Success | Wall-clock (s) |\n");
    md.push_str("|------|------|--------|--------|-------|----------|----------------|\n");
    for r in &sorted {
        md.push_str(&format!(
            "| {} | {} | {} | {} | {} | {} | {:.1} |\n",
            r.tool_name,
            r.task_name,
            r.context_budget,
            r.tokens_consumed,
            r.turns_taken,
            if r.success { "Yes" } else { "No" },
            r.wall_clock_secs,
        ));
    }
    md
}

/// Write the cross-tool comparison markdown table to disk.
pub fn write_cross_tool_comparison(reports: &[ExternalToolReport], path: &Path) -> Result<()> {
    let md = compare_cross_tool(reports);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, md)?;
    Ok(())
}

/// Write the Token Budget Challenge markdown scoreboard to disk.
///
/// The table has one row per ceiling level (descending) and the six
/// metric columns from `123.md` lines 282-288: ceiling, success,
/// prompt tokens, completion tokens, compression passes, cost. An
/// empty `entries` slice yields a header-only table so the report is
/// still a valid markdown document.
pub fn write_budget_challenge_report(report: &BudgetChallengeReport, path: &Path) -> Result<()> {
    let mut md = String::new();
    md.push_str(&format!(
        "# Token Budget Challenge: {}\n\n",
        report.task_name
    ));
    md.push_str(&format!("**Model:** {}\n\n", report.model));
    md.push_str("| Ceiling | Success | Prompt Tokens | Completion Tokens | Compression Passes | Cost ($) |\n");
    md.push_str("|---------|---------|---------------|-------------------|-------------------|----------|\n");
    for e in &report.entries {
        md.push_str(&format!(
            "| {} | {} | {} | {} | {} | {:.4} |\n",
            e.ceiling,
            if e.success { "Yes" } else { "No" },
            e.prompt_tokens,
            e.completion_tokens,
            e.compression_passes,
            e.cost_usd,
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
                compression_passes: 0,
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
            compression_passes: 0,
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

    // WO 43.39: write_markdown_delta must render the per-report summary
    // rates, NOT rates recomputed from the union task set. When baseline
    // and current task sets differ, a task only in current has no
    // baseline result and would be miscounted as a baseline failure,
    // shifting both rendered percentages away from the true summaries.
    #[test]
    fn write_markdown_delta_uses_summary_rates_not_union_set() {
        // Baseline: tasks A (pass), B (fail) → 1/2 = 50%.
        let baseline = BenchReport {
            model: "base".into(),
            timestamp: "2026-01-01T00:00:00Z".into(),
            results: vec![
                TaskResult {
                    task_name: "A".into(),
                    difficulty: Difficulty::Easy,
                    success: true,
                    tokens_in: 100,
                    tokens_out: 50,
                    duration_secs: 1.0,
                    cost_usd: 0.01,
                    tool_calls: 1,
                    compression_passes: 0,
                    error: None,
                },
                TaskResult {
                    task_name: "B".into(),
                    difficulty: Difficulty::Easy,
                    success: false,
                    tokens_in: 100,
                    tokens_out: 50,
                    duration_secs: 1.0,
                    cost_usd: 0.01,
                    tool_calls: 1,
                    compression_passes: 0,
                    error: Some("fail".into()),
                },
            ],
            summary: BenchSummary {
                success_rate: 0.5,
                total_tokens_in: 200,
                total_tokens_out: 100,
                total_cost_usd: 0.02,
                total_duration_secs: 2.0,
                total_tool_calls: 2,
                tasks_run: 2,
                tasks_passed: 1,
            },
        };
        // Current: tasks B (pass), C (pass) → 2/2 = 100%.
        let current = BenchReport {
            model: "curr".into(),
            timestamp: "2026-01-02T00:00:00Z".into(),
            results: vec![
                TaskResult {
                    task_name: "B".into(),
                    difficulty: Difficulty::Easy,
                    success: true,
                    tokens_in: 100,
                    tokens_out: 50,
                    duration_secs: 1.0,
                    cost_usd: 0.01,
                    tool_calls: 1,
                    compression_passes: 0,
                    error: None,
                },
                TaskResult {
                    task_name: "C".into(),
                    difficulty: Difficulty::Easy,
                    success: true,
                    tokens_in: 100,
                    tokens_out: 50,
                    duration_secs: 1.0,
                    cost_usd: 0.01,
                    tool_calls: 1,
                    compression_passes: 0,
                    error: None,
                },
            ],
            summary: BenchSummary {
                success_rate: 1.0,
                total_tokens_in: 200,
                total_tokens_out: 100,
                total_cost_usd: 0.02,
                total_duration_secs: 2.0,
                total_tool_calls: 2,
                tasks_run: 2,
                tasks_passed: 2,
            },
        };

        let delta = compare_reports(&baseline, &current);
        // The regression signal uses summary rates.
        assert!((delta.baseline_success_rate - 0.5).abs() < 1e-9);
        assert!((delta.current_success_rate - 1.0).abs() < 1e-9);
        assert!((delta.success_rate_delta - 0.5).abs() < 1e-9);

        let dir = tempfile::tempdir().unwrap();
        let md_path = dir.path().join("delta.md");
        write_markdown_delta(&delta, &md_path).unwrap();
        let md = std::fs::read_to_string(&md_path).unwrap();

        // Rendered rates must match the true summaries (50% → 100%),
        // not the union-set recomputation. The buggy version would
        // compute baseline as 1/3 (only A passed among A,B,C) and
        // current as 2/3 (B,C passed among A,B,C), rendering 33% → 67%.
        assert!(
            md.contains("Success rate:** 50% → 100%"),
            "rendered rates must match summaries (50% → 100%), got:\n{md}"
        );
        assert!(
            !md.contains("33%") && !md.contains("67%"),
            "rendered rates must not be the union-set recomputation, got:\n{md}"
        );
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
        assert!(!infos[0].kf_only);
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
            requires_model: false,
            budget_ceiling: None,
            kf_only: false,
        };
        let result = verify_only(&task, dir.path());
        assert!(result.success);
        assert!(result.error.is_none());
    }

    #[test]
    fn test_model_comparison_empty() {
        let out = write_model_comparison(&[]);
        assert_eq!(out, "No reports to compare");
    }

    #[test]
    fn test_model_comparison_single() {
        let report = sample_report(true, 1200, 3400, 0.001);
        let out = write_model_comparison(&[report]);
        assert!(out.contains("# Model Comparison"));
        assert!(out.contains("| Model | Tasks Passed | Success Rate"));
        assert!(out.contains("| test-model | 1/1 | 100%"));
        assert!(out.contains("1.2k"));
        assert!(out.contains("3.4k"));
    }

    #[test]
    fn test_model_comparison_multiple_sorted_by_success_rate_desc() {
        let mut high = sample_report(true, 1500, 4100, 0.002);
        high.model = "glm-5.2:cloud".to_string();
        let mut low = sample_report(false, 1200, 3400, 0.001);
        low.model = "qwen2.5:0.5b".to_string();
        low.summary.success_rate = 0.0;
        low.summary.tasks_passed = 0;

        // Pass them in "wrong" order to verify sort.
        let out = write_model_comparison(&[low.clone(), high.clone()]);
        let lines: Vec<&str> = out.lines().collect();
        let data_rows: Vec<&str> = lines
            .iter()
            .filter(|l| l.starts_with("| ") && !l.starts_with("| Model"))
            .copied()
            .collect();
        assert_eq!(data_rows.len(), 2);
        assert!(
            data_rows[0].contains("glm-5.2:cloud"),
            "first row should be the higher-success-rate model, got: {}",
            data_rows[0]
        );
        assert!(
            data_rows[1].contains("qwen2.5:0.5b"),
            "second row should be the lower-success-rate model, got: {}",
            data_rows[1]
        );
        assert!(data_rows[0].contains("100%"));
        assert!(data_rows[1].contains("0%"));
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
            requires_model: false,
            budget_ceiling: None,
            kf_only: false,
        };
        let result = verify_only(&task, dir.path());
        assert!(!result.success);
        assert_eq!(result.error, Some("verification failed".to_string()));
    }

    #[test]
    fn test_verify_only_skips_requires_model() {
        let dir = tempfile::tempdir().unwrap();
        let task = BenchTask {
            name: "requires_model_task".to_string(),
            difficulty: Difficulty::Medium,
            prompt: "unused".to_string(),
            setup: HashMap::new(),
            // A verify that would fail on the unedited setup (`false`),
            // but the task is marked requires_model so verify_only must
            // skip it instead of running the verify and reporting FAIL.
            verify: VerifySpec::CommandExitsZero {
                command: "false".to_string(),
            },
            requires_model: true,
            budget_ceiling: None,
            kf_only: false,
        };
        let result = verify_only(&task, dir.path());
        // SKIP counts as success (the task is not broken, just not
        // verifiable without the model).
        assert!(result.success);
        assert_eq!(result.error.as_deref(), Some("skipped (requires model)"));
    }

    // ── WO 10.9: compare_with_threshold tests ──

    fn make_report(model: &str, tasks_run: usize, tasks_passed: usize) -> BenchReport {
        let success_rate = if tasks_run > 0 {
            tasks_passed as f64 / tasks_run as f64
        } else {
            0.0
        };
        let mut results = Vec::new();
        for i in 0..tasks_run {
            let success = i < tasks_passed;
            results.push(TaskResult {
                task_name: format!("task-{i}"),
                difficulty: Difficulty::Easy,
                success,
                tokens_in: 100,
                tokens_out: 50,
                duration_secs: 1.0,
                cost_usd: 0.001,
                tool_calls: 2,
                compression_passes: 0,
                error: if success { None } else { Some("failed".into()) },
            });
        }
        BenchReport {
            model: model.into(),
            timestamp: "2026-07-27T00:00:00Z".into(),
            results,
            summary: BenchSummary {
                success_rate,
                total_tokens_in: 100 * tasks_run as u64,
                total_tokens_out: 50 * tasks_run as u64,
                total_cost_usd: 0.001 * tasks_run as f64,
                total_duration_secs: tasks_run as f64,
                total_tool_calls: tasks_run as u32 * 2,
                tasks_run,
                tasks_passed,
            },
        }
    }

    #[test]
    fn compare_with_threshold_no_regression() {
        // Baseline 80% (8/10), current 80% (8/10) → delta 0% → no
        // regression at any threshold.
        let baseline = make_report("base", 10, 8);
        let current = make_report("curr", 10, 8);
        let result = compare_with_threshold(&baseline, &current, 0.10);
        assert!(!result.regression_detected, "no change → no regression");
        assert!((result.delta.success_rate_delta - 0.0).abs() < 1e-9);
    }

    #[test]
    fn compare_with_threshold_within_threshold() {
        // Baseline 100% (100/100), current 92% (92/100) → delta -8%.
        // Threshold 10pp → -8% is within (not beyond) → no regression.
        let baseline = make_report("base", 100, 100);
        let current = make_report("curr", 100, 92);
        let result = compare_with_threshold(&baseline, &current, 0.10);
        assert!(
            !result.regression_detected,
            "8pp drop is within the 10pp threshold (not a regression): delta={:.4}",
            result.delta.success_rate_delta
        );
    }

    #[test]
    fn compare_with_threshold_beyond_threshold() {
        // Baseline 80% (8/10), current 60% (6/10) → delta -20% →
        // regression at threshold 10pp.
        let baseline = make_report("base", 10, 8);
        let current = make_report("curr", 10, 6);
        let result = compare_with_threshold(&baseline, &current, 0.10);
        assert!(
            result.regression_detected,
            "20pp drop exceeds 10pp threshold → regression"
        );
        assert!(
            result.delta.success_rate_delta < -0.10,
            "delta should be worse than -0.10: {}",
            result.delta.success_rate_delta
        );
    }

    #[test]
    fn compare_with_threshold_improvement_is_not_regression() {
        // Baseline 60% (6/10), current 80% (8/10) → delta +20% →
        // improvement, not regression.
        let baseline = make_report("base", 10, 6);
        let current = make_report("curr", 10, 8);
        let result = compare_with_threshold(&baseline, &current, 0.10);
        assert!(
            !result.regression_detected,
            "improvement is never a regression"
        );
        assert!(result.delta.success_rate_delta > 0.0);
    }

    // ── WO 14.7: budget_ceiling + BudgetChallengeReport tests ──

    #[test]
    fn budget_ceiling_serializes_when_some() {
        // ponytail: budget_ceiling is serde-optional; Some(N) must
        // round-trip through toml so the task file format stays
        // stable. None must be omitted (default) so existing task
        // files parse without the field.
        let task = BenchTask {
            name: "tbc".to_string(),
            difficulty: Difficulty::Medium,
            prompt: "p".to_string(),
            setup: HashMap::new(),
            verify: VerifySpec::CommandExitsZero {
                command: "true".to_string(),
            },
            requires_model: true,
            budget_ceiling: Some(32_768),
            kf_only: false,
        };
        let toml = toml::to_string(&task).expect("serialize BenchTask");
        assert!(
            toml.contains("budget_ceiling = 32768"),
            "Some(32768) must serialize as a literal, got:\n{toml}"
        );
        let parsed: BenchTask = toml::from_str(&toml).expect("deserialize BenchTask");
        assert_eq!(parsed.budget_ceiling, Some(32_768));
    }

    #[test]
    fn budget_ceiling_defaults_none_when_absent() {
        let toml = r#"
            name = "no_budget"
            difficulty = "easy"
            prompt = "p"

            [verify]
            type = "command_exits_zero"
            command = "true"
        "#;
        let task: BenchTask = toml::from_str(toml).expect("parse without budget_ceiling");
        assert!(task.budget_ceiling.is_none());
    }

    #[test]
    fn budget_env_returns_var_when_set() {
        let task = BenchTask {
            name: "tbc".to_string(),
            difficulty: Difficulty::Medium,
            prompt: "p".to_string(),
            setup: HashMap::new(),
            verify: VerifySpec::CommandExitsZero {
                command: "true".to_string(),
            },
            requires_model: true,
            budget_ceiling: Some(16_384),
            kf_only: false,
        };
        let env = task.budget_env().expect("Some ceiling → Some env");
        assert_eq!(env.0, BUDGET_CEILING_ENV);
        assert_eq!(env.1, 16_384);
    }

    #[test]
    fn budget_env_returns_none_when_unset() {
        let task = BenchTask {
            name: "tbc".to_string(),
            difficulty: Difficulty::Medium,
            prompt: "p".to_string(),
            setup: HashMap::new(),
            verify: VerifySpec::CommandExitsZero {
                command: "true".to_string(),
            },
            requires_model: true,
            budget_ceiling: None,
            kf_only: false,
        };
        assert!(task.budget_env().is_none());
    }

    #[test]
    fn write_budget_challenge_report_emits_table() {
        let dir = tempfile::tempdir().unwrap();
        let report = BudgetChallengeReport {
            task_name: "token_budget_challenge".to_string(),
            model: "qwen2.5:0.5b".to_string(),
            entries: vec![
                BudgetChallengeEntry {
                    ceiling: 131_072,
                    success: true,
                    prompt_tokens: 8_412,
                    completion_tokens: 1_153,
                    compression_passes: 0,
                    cost_usd: 0.1200,
                },
                BudgetChallengeEntry {
                    ceiling: 8_192,
                    success: false,
                    prompt_tokens: 1_200,
                    completion_tokens: 300,
                    compression_passes: 3,
                    cost_usd: 0.0100,
                },
            ],
        };
        let path = dir.path().join("tbc.md");
        write_budget_challenge_report(&report, &path).unwrap();
        let md = std::fs::read_to_string(&path).unwrap();
        assert!(md.contains("# Token Budget Challenge: token_budget_challenge"));
        assert!(md.contains("**Model:** qwen2.5:0.5b"));
        assert!(md.contains("| Ceiling | Success | Prompt Tokens | Completion Tokens | Compression Passes | Cost ($) |"));
        // Descending-ceiling rows present with the six metric columns.
        assert!(md.contains("| 131072 | Yes | 8412 | 1153 | 0 | 0.1200 |"));
        assert!(md.contains("| 8192 | No | 1200 | 300 | 3 | 0.0100 |"));
    }

    #[test]
    fn write_budget_challenge_report_empty_entries_header_only() {
        let dir = tempfile::tempdir().unwrap();
        let report = BudgetChallengeReport {
            task_name: "tbc".to_string(),
            model: "m".to_string(),
            entries: vec![],
        };
        let path = dir.path().join("empty.md");
        write_budget_challenge_report(&report, &path).unwrap();
        let md = std::fs::read_to_string(&path).unwrap();
        assert!(md.contains("| Ceiling | Success |"));
        // No data rows, just the header line.
        assert!(!md.contains("| 131072 |"));
    }

    // ── WO 32.6: cross-tool comparison tests ──

    fn ext_report(tool: &str, task: &str, budget: usize, success: bool) -> ExternalToolReport {
        ExternalToolReport {
            tool_name: tool.to_string(),
            task_name: task.to_string(),
            context_budget: budget,
            tokens_consumed: 1000,
            turns_taken: 3,
            success,
            wall_clock_secs: 12.5,
        }
    }

    #[test]
    fn compare_cross_tool_empty_returns_placeholder() {
        assert_eq!(compare_cross_tool(&[]), "No cross-tool reports to compare");
    }

    #[test]
    fn compare_cross_tool_renders_table_sorted_by_task_then_budget_desc() {
        let reports = vec![
            ext_report("kf-code", "bug-fix", 32_768, true),
            ext_report("kf-code", "bug-fix", 131_072, true),
            ext_report("codex", "bug-fix", 131_072, false),
            ext_report("claude-code", "refactor", 65_536, true),
        ];
        let md = compare_cross_tool(&reports);
        assert!(md.contains("# Cross-Tool Comparison"));
        assert!(md.contains("| Tool | Task | Budget | Tokens | Turns | Success | Wall-clock (s) |"));
        // Descending-budget sort: 131072 before 32768 for the same task.
        let bug_rows: Vec<&str> = md
            .lines()
            .filter(|l| l.starts_with("| ") && l.contains("bug-fix"))
            .collect();
        assert_eq!(bug_rows.len(), 3);
        // First two rows are the 131072 entries (descending), 32768 is last.
        assert!(bug_rows[0].contains("131072"));
        assert!(bug_rows[2].contains("32768"));
        assert!(bug_rows[0].contains("codex") || bug_rows[1].contains("codex"));
        // Success column renders Yes/No.
        assert!(md.contains("| codex | bug-fix | 131072 | 1000 | 3 | No | 12.5 |"));
    }

    #[test]
    fn external_tool_report_json_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let batch = ExternalToolReportBatch {
            reports: vec![
                ext_report("kf-code", "feature-add", 131_072, true),
                ext_report("codex", "feature-add", 131_072, false),
            ],
        };
        let path = dir.path().join("cross_tool.json");
        write_external_reports(&batch, &path).unwrap();
        let loaded = load_external_reports(&path).unwrap();
        assert_eq!(loaded.reports.len(), 2);
        assert_eq!(loaded.reports[0].tool_name, "kf-code");
        assert_eq!(loaded.reports[1].tool_name, "codex");
        assert_eq!(loaded.reports[0].context_budget, 131_072);
        assert!(!loaded.reports[1].success);
    }

    #[test]
    fn write_cross_tool_comparison_writes_markdown() {
        let dir = tempfile::tempdir().unwrap();
        let reports = vec![ext_report("kf-code", "docs", 8_192, true)];
        let path = dir.path().join("cmp.md");
        write_cross_tool_comparison(&reports, &path).unwrap();
        let md = std::fs::read_to_string(&path).unwrap();
        assert!(md.contains("| kf-code | docs | 8192 |"));
    }
}
