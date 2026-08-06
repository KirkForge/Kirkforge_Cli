//! Headless benchmark session execution.
//!
//! Runs a single benchmark task against a kf-code executor, collects
//! metrics from turn events, and verifies the result.

use crate::session::access::{DenyList, PathGuard};
use crate::shared::{Config, SharedConfig};
use kf_bench::{BenchReport, BenchSummary, BenchTask, TaskResult};
use std::path::Path;
use std::time::Instant;

/// Collect metrics from a completed turn's events and produce a TaskResult.
///
/// This is the pure, testable portion of `run_task` — no adapter, no async,
/// no timeout. It aggregates token counts, tool call counts, and cost from
/// the `TurnEvent` stream, then runs verification.
pub fn collect_turn_metrics(
    events: &[super::executor::TurnEvent],
    duration_secs: f64,
    task: &BenchTask,
    sandbox_path: &std::path::Path,
    run_error: Option<String>,
) -> TaskResult {
    let mut tokens_in: u64 = 0;
    let mut tokens_out: u64 = 0;
    let mut cost_usd: f64 = 0.0;
    let mut tool_calls: u32 = 0;
    let mut compression_passes: u32 = 0;

    for event in events {
        match event {
            super::executor::TurnEvent::CostStats {
                prompt_tokens,
                completion_tokens,
                turn_cost,
                ..
            } => {
                tokens_in += *prompt_tokens as u64;
                tokens_out += *completion_tokens as u64;
                cost_usd += turn_cost;
            }
            super::executor::TurnEvent::ToolStart { .. } => {
                tool_calls += 1;
            }
            super::executor::TurnEvent::CompactionReport { .. } => {
                compression_passes += 1;
            }
            _ => {}
        }
    }

    let success = if run_error.is_none() {
        kf_bench::verify_task(task, sandbox_path).unwrap_or(false)
    } else {
        false
    };

    TaskResult {
        task_name: task.name.clone(),
        difficulty: task.difficulty,
        success,
        tokens_in,
        tokens_out,
        duration_secs,
        cost_usd,
        tool_calls,
        compression_passes,
        error: run_error,
    }
}

/// Build a sandboxed toolset for bench runs.
///
/// Provides 6 core tools (read_file, write_file, edit_file, bash, glob, grep)
/// constrained to the temp sandbox dir. No undo stack, no images, no LSP,
/// no computer_use, no docker.
fn build_bench_toolset(sandbox_path: &Path) -> super::toolset::CompositeToolset {
    let deny_list = DenyList::default();
    let path_guard = PathGuard {
        sandbox_dir: Some(sandbox_path.to_path_buf()),
        deny_extensions: PathGuard::default().deny_extensions,
        block_dotfiles: false,
        block_gitignored_dotfiles: false,
        max_read_size: 1024 * 1024,
        max_overwrite_size: 1024 * 1024,
        deny_list: deny_list.clone(),
        follow_symlinks: false,
        allowed_write_dirs: vec![],
        block_binary_reads: false,
    };

    let ctx = crate::tools::ToolContextBuilder {
        undo_stack: None,
        supports_images: false,
        deny_list,
        path_guard,
        bash_sandbox_workdir: true,
        minify_write_side: false,
        minify_above_bytes: 4096,
        lsp_pool: None,
        computer_use_enabled: false,
        computer_use_config: None,
        chrome_tab: None,
        session_launcher: None,
        docker_config: None,
        sandbox_config: crate::shared::SandboxConfig::default(),
        block_edits: false,
    };
    let tools = crate::tools::all_tools(&ctx);

    let mut toolset = super::toolset::CompositeToolset::empty();
    toolset.add(Box::new(super::toolset::VecToolset::new("builtin", tools)));
    toolset
}

/// Run a single benchmark task.
///
/// Creates a temp sandbox dir, applies setup files, starts a headless
/// kf-code session, sends the prompt, waits for completion (or timeout),
/// runs the verify command, and collects metrics.
pub async fn run_task(
    task: &BenchTask,
    model: &str,
    config: &Config,
    timeout_secs: u64,
) -> anyhow::Result<TaskResult> {
    let sandbox = tempfile::tempdir()?;
    let sandbox_path = sandbox.path().to_path_buf();

    // Apply setup files.
    for (rel_path, content) in &task.setup {
        let file_path = sandbox_path.join(rel_path);
        if let Some(parent) = file_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&file_path, content)?;
    }

    let start = Instant::now();

    // Build a session config for this task.
    let mut task_config = config.clone();
    task_config.model.default_model = model.to_string();
    task_config.security.sandbox_dir = Some(sandbox_path.to_string_lossy().to_string());
    task_config.security.auto_approve = true;
    task_config.tools.dry_run = false;
    // WO 14.7: when the task pins a budget ceiling, export it as
    // KF_CODE_BUDGET_CEILING so the budget guard (init_from_config
    // → apply_env_overrides) enforces it for this run. The Token
    // Budget Challenge sets this per run; other tasks leave it None.
    if let Some((env_name, ceiling)) = task.budget_env() {
        std::env::set_var(env_name, ceiling.to_string());
    }
    super::config::freeze_launch_sandbox(&mut task_config);

    let shared_config: SharedConfig = std::sync::Arc::new(std::sync::RwLock::new(task_config));

    let ollama_host = crate::shared::read_shared_config(&shared_config)
        .model
        .ollama_host
        .clone();
    let anthropic_provider = crate::shared::read_shared_config(&shared_config)
        .model
        .anthropic_provider
        .clone();
    let request_timeout = crate::shared::read_shared_config(&shared_config)
        .model
        .request_timeout_secs;
    let zen_endpoint = crate::shared::read_shared_config(&shared_config)
        .model
        .opencode_zen_endpoint
        .clone();
    let zen_api_key = crate::shared::read_shared_config(&shared_config)
        .model
        .opencode_zen_api_key
        .clone();

    // Create adapter.
    let adapter = crate::adapters::adapter_for_with_provider(
        model,
        &ollama_host,
        None,
        &anthropic_provider,
        request_timeout,
        &zen_endpoint,
        zen_api_key.as_deref(),
        None,
        &crate::adapters::ProviderApiKeys::default(),
        None,
        None,
        None,
        None,
    );

    // Open conversation log in sandbox.
    let data_dir = sandbox_path.join("kf-code-data");
    std::fs::create_dir_all(&data_dir)?;
    let session_id = format!("bench-{}", task.name);
    let log_path = data_dir.join(format!("{session_id}.conv.ndjson"));
    let (conversation, _open_outcome) = super::conversation::ConversationLog::open(log_path)?;

    let toolset = build_bench_toolset(&sandbox_path);

    // Construct executor.
    let mut executor = super::executor::Executor::with_log_and_undo(
        adapter,
        toolset,
        shared_config,
        conversation,
        None,
        None,
    )?;
    executor.set_session_id(session_id.clone());

    // Approval channel: auto-approve all tool calls for bench runs.
    let (approval_tx, mut approval_rx) =
        tokio::sync::mpsc::unbounded_channel::<super::executor::ApprovalRequest>();

    let cancel_token = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));

    tokio::spawn(async move {
        while let Some(req) = approval_rx.recv().await {
            let _ = req
                .response
                .send(super::executor::ApprovalResponse::Approved);
        }
    });

    // Run with timeout.
    let turn_result = tokio::time::timeout(
        std::time::Duration::from_secs(timeout_secs),
        executor.run_turn_collecting(&task.prompt, &approval_tx, &cancel_token),
    )
    .await;

    // WO 14.7: clear the budget-ceiling env var so it does not leak
    // into subsequent tasks in the same `bench run` invocation. Only
    // the task that set it clears it; tasks without a ceiling leave
    // the env untouched.
    if task.budget_ceiling.is_some() {
        std::env::remove_var(kf_bench::BUDGET_CEILING_ENV);
    }

    let duration = start.elapsed().as_secs_f64();

    // Collect metrics from turn events.
    let (events, run_error) = match turn_result {
        Ok(Ok(evts)) => (evts, None),
        Ok(Err(e)) => (vec![], Some(e.to_string())),
        Err(_) => (vec![], Some(format!("timeout after {timeout_secs}s"))),
    };

    Ok(collect_turn_metrics(
        &events,
        duration,
        task,
        &sandbox_path,
        run_error,
    ))
}

/// Descending budget ceilings for the Token Budget Challenge (WO 14.7).
/// The runner executes the task once per ceiling and records the six
/// metrics per run. The progression showcases the tree-sitter index +
/// Stratum compression + budget guard under progressively tighter
/// context budgets. See ADR-0066.
pub const BUDGET_CHALLENGE_CEILINGS: [usize; 5] = [131_072, 65_536, 32_768, 16_384, 8_192];

/// Name of the signature Token Budget Challenge task. `run_all` detects
/// this task by name and dispatches it to `run_token_budget_challenge`
/// instead of the single-run path. See ADR-0066.
pub const BUDGET_CHALLENGE_TASK_NAME: &str = "token_budget_challenge";

/// Run the Token Budget Challenge: execute the task 5x under descending
/// budget ceilings (128k → 64k → 32k → 16k → 8k), collecting the six
/// metrics per run into a `BudgetChallengeReport`. Each run clones the
/// task with `budget_ceiling` set to the current ceiling so the runner
/// exports `KF_CODE_BUDGET_CEILING` for that run. Returns the report
/// plus a flat `Vec<TaskResult>` (one per ceiling) so `run_all` can
/// fold the per-ceiling results into the standard `BenchReport`.
pub async fn run_token_budget_challenge(
    task: &BenchTask,
    model: &str,
    config: &Config,
    timeout_secs: u64,
) -> (kf_bench::BudgetChallengeReport, Vec<TaskResult>) {
    let mut entries = Vec::with_capacity(BUDGET_CHALLENGE_CEILINGS.len());
    let mut results = Vec::with_capacity(BUDGET_CHALLENGE_CEILINGS.len());
    for ceiling in BUDGET_CHALLENGE_CEILINGS {
        eprintln!("  running task: {} @ {}k...", task.name, ceiling / 1024);
        let mut ceiling_task = task.clone();
        ceiling_task.budget_ceiling = Some(ceiling);
        match run_task(&ceiling_task, model, config, timeout_secs).await {
            Ok(result) => {
                entries.push(kf_bench::BudgetChallengeEntry {
                    ceiling,
                    success: result.success,
                    prompt_tokens: result.tokens_in,
                    completion_tokens: result.tokens_out,
                    compression_passes: result.compression_passes,
                    cost_usd: result.cost_usd,
                });
                results.push(result);
            }
            Err(e) => {
                let result = TaskResult {
                    task_name: format!("{}@{}k", task.name, ceiling / 1024),
                    difficulty: task.difficulty,
                    success: false,
                    tokens_in: 0,
                    tokens_out: 0,
                    duration_secs: 0.0,
                    cost_usd: 0.0,
                    tool_calls: 0,
                    compression_passes: 0,
                    error: Some(e.to_string()),
                };
                entries.push(kf_bench::BudgetChallengeEntry {
                    ceiling,
                    success: false,
                    prompt_tokens: 0,
                    completion_tokens: 0,
                    compression_passes: 0,
                    cost_usd: 0.0,
                });
                results.push(result);
            }
        }
    }
    let report = kf_bench::BudgetChallengeReport {
        task_name: task.name.clone(),
        model: model.to_string(),
        entries,
    };
    (report, results)
}

/// Run all tasks and collect results.
pub async fn run_all(
    tasks: &[BenchTask],
    model: &str,
    config: &Config,
    timeout_secs: u64,
) -> BenchReport {
    let mut results = Vec::new();
    for task in tasks {
        // WO 14.7: the signature Token Budget Challenge runs 5x under
        // descending ceilings instead of the single-run path. The
        // per-ceiling results fold into the standard report; the
        // dedicated BudgetChallengeReport is written separately by
        // the CLI handler when the task is present.
        if task.name == BUDGET_CHALLENGE_TASK_NAME {
            let (_challenge_report, challenge_results) =
                run_token_budget_challenge(task, model, config, timeout_secs).await;
            results.extend(challenge_results);
            continue;
        }
        eprintln!("  running task: {}...", task.name);
        match run_task(task, model, config, timeout_secs).await {
            Ok(result) => results.push(result),
            Err(e) => results.push(TaskResult {
                task_name: task.name.clone(),
                difficulty: task.difficulty,
                success: false,
                tokens_in: 0,
                tokens_out: 0,
                duration_secs: 0.0,
                cost_usd: 0.0,
                tool_calls: 0,
                compression_passes: 0,
                error: Some(e.to_string()),
            }),
        }
    }
    let summary = BenchSummary::from_results(&results);
    BenchReport {
        model: model.to_string(),
        timestamp: chrono::Utc::now().to_rfc3339(),
        results,
        summary,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kf_bench::{Difficulty, VerifySpec};
    use std::collections::HashMap;

    fn sample_task(name: &str, verify: VerifySpec) -> BenchTask {
        BenchTask {
            name: name.to_string(),
            difficulty: Difficulty::Easy,
            prompt: "test prompt".to_string(),
            setup: HashMap::new(),
            verify,
            requires_model: false,
            budget_ceiling: None,
        }
    }

    #[test]
    fn bench_collect_metrics_empty_events_verify_success() {
        let dir = tempfile::tempdir().unwrap();
        let task = sample_task(
            "verify-true",
            VerifySpec::CommandExitsZero {
                command: "true".to_string(),
            },
        );
        let result = collect_turn_metrics(&[], 1.5, &task, dir.path(), None);
        assert!(result.success);
        assert_eq!(result.tokens_in, 0);
        assert_eq!(result.tokens_out, 0);
        assert_eq!(result.tool_calls, 0);
        assert_eq!(result.duration_secs, 1.5);
        assert!(result.error.is_none());
    }

    #[test]
    fn bench_collect_metrics_with_cost_stats() {
        let dir = tempfile::tempdir().unwrap();
        let task = sample_task(
            "cost",
            VerifySpec::CommandExitsZero {
                command: "true".to_string(),
            },
        );
        let events = vec![
            super::super::executor::TurnEvent::CostStats {
                prompt_tokens: 100,
                completion_tokens: 50,
                turn_cost: 0.002,
                cumulative_cost: 0.002,
            },
            super::super::executor::TurnEvent::ToolStart {
                name: "write_file".to_string(),
                args: serde_json::json!({}),
            },
            super::super::executor::TurnEvent::CostStats {
                prompt_tokens: 200,
                completion_tokens: 80,
                turn_cost: 0.003,
                cumulative_cost: 0.005,
            },
            super::super::executor::TurnEvent::ToolStart {
                name: "bash".to_string(),
                args: serde_json::json!({}),
            },
        ];
        let result = collect_turn_metrics(&events, 3.2, &task, dir.path(), None);
        assert!(result.success);
        assert_eq!(result.tokens_in, 300);
        assert_eq!(result.tokens_out, 130);
        assert!((result.cost_usd - 0.005).abs() < 0.0001);
        assert_eq!(result.tool_calls, 2);
    }

    #[test]
    fn bench_collect_metrics_error_sets_success_false() {
        let dir = tempfile::tempdir().unwrap();
        let task = sample_task(
            "err",
            VerifySpec::CommandExitsZero {
                command: "true".to_string(),
            },
        );
        let result =
            collect_turn_metrics(&[], 5.0, &task, dir.path(), Some("model error".to_string()));
        assert!(!result.success);
        assert_eq!(result.error.as_deref(), Some("model error"));
    }

    #[test]
    fn bench_collect_metrics_timeout_sets_success_false() {
        let dir = tempfile::tempdir().unwrap();
        let task = sample_task(
            "timeout",
            VerifySpec::CommandExitsZero {
                command: "true".to_string(),
            },
        );
        let result = collect_turn_metrics(
            &[],
            10.0,
            &task,
            dir.path(),
            Some("timeout after 300s".to_string()),
        );
        assert!(!result.success);
        assert!(result.error.as_ref().unwrap().contains("timeout"));
    }

    #[test]
    fn bench_collect_metrics_verify_fails() {
        let dir = tempfile::tempdir().unwrap();
        let task = sample_task(
            "verify-fail",
            VerifySpec::CommandExitsZero {
                command: "false".to_string(),
            },
        );
        let result = collect_turn_metrics(&[], 2.0, &task, dir.path(), None);
        assert!(!result.success);
    }

    #[test]
    fn bench_collect_metrics_file_contains_verify() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src/main.rs"), "fn main() {}").unwrap();
        let task = BenchTask {
            name: "file-contains".to_string(),
            difficulty: Difficulty::Medium,
            prompt: "add a test".to_string(),
            setup: HashMap::new(),
            verify: VerifySpec::FileContains {
                path: "src/main.rs".to_string(),
                contains: "fn main".to_string(),
            },
            requires_model: false,
            budget_ceiling: None,
        };
        let result = collect_turn_metrics(&[], 1.0, &task, dir.path(), None);
        assert!(result.success);
    }

    #[test]
    fn bench_collect_metrics_file_contains_missing_file() {
        let dir = tempfile::tempdir().unwrap();
        let task = BenchTask {
            name: "file-missing".to_string(),
            difficulty: Difficulty::Easy,
            prompt: "create a file".to_string(),
            setup: HashMap::new(),
            verify: VerifySpec::FileContains {
                path: "nonexistent.rs".to_string(),
                contains: "hello".to_string(),
            },
            requires_model: false,
            budget_ceiling: None,
        };
        let result = collect_turn_metrics(&[], 1.0, &task, dir.path(), None);
        assert!(!result.success);
    }

    #[test]
    fn bench_run_all_collects_error_result() {
        let results = vec![
            TaskResult {
                task_name: "ok".to_string(),
                difficulty: Difficulty::Easy,
                success: true,
                tokens_in: 100,
                tokens_out: 50,
                duration_secs: 1.0,
                cost_usd: 0.01,
                tool_calls: 2,
                compression_passes: 0,
                error: None,
            },
            TaskResult {
                task_name: "fail".to_string(),
                difficulty: Difficulty::Hard,
                success: false,
                tokens_in: 0,
                tokens_out: 0,
                duration_secs: 0.0,
                cost_usd: 0.0,
                tool_calls: 0,
                compression_passes: 0,
                error: Some("model error".to_string()),
            },
        ];
        let summary = BenchSummary::from_results(&results);
        assert_eq!(summary.tasks_run, 2);
        assert_eq!(summary.tasks_passed, 1);
        assert!((summary.success_rate - 0.5).abs() < 0.001);
        assert_eq!(summary.total_tokens_in, 100);
        assert_eq!(summary.total_tool_calls, 2);
    }

    #[test]
    fn bench_collect_metrics_ignores_non_relevant_events() {
        let dir = tempfile::tempdir().unwrap();
        let task = sample_task(
            "ignore-events",
            VerifySpec::CommandExitsZero {
                command: "true".to_string(),
            },
        );
        let events = vec![
            super::super::executor::TurnEvent::Token("hello".into()),
            super::super::executor::TurnEvent::Thinking("thought".into()),
            super::super::executor::TurnEvent::Error("err".into()),
        ];
        let result = collect_turn_metrics(&events, 1.0, &task, dir.path(), None);
        assert!(result.success);
        assert_eq!(result.tokens_in, 0);
        assert_eq!(result.tokens_out, 0);
        assert_eq!(result.tool_calls, 0);
        assert_eq!(result.cost_usd, 0.0);
    }

    #[test]
    fn bench_collect_metrics_accumulates_tool_calls_across_events() {
        let dir = tempfile::tempdir().unwrap();
        let task = sample_task(
            "multi-tools",
            VerifySpec::CommandExitsZero {
                command: "true".to_string(),
            },
        );
        let events = vec![
            super::super::executor::TurnEvent::ToolStart {
                name: "bash".to_string(),
                args: serde_json::json!({}),
            },
            super::super::executor::TurnEvent::ToolStart {
                name: "write_file".to_string(),
                args: serde_json::json!({}),
            },
            super::super::executor::TurnEvent::ToolStart {
                name: "edit_file".to_string(),
                args: serde_json::json!({}),
            },
        ];
        let result = collect_turn_metrics(&events, 2.5, &task, dir.path(), None);
        assert_eq!(result.tool_calls, 3);
    }

    #[test]
    fn bench_collect_metrics_cost_stats_accumulate_cost() {
        let dir = tempfile::tempdir().unwrap();
        let task = sample_task(
            "cost-acc",
            VerifySpec::CommandExitsZero {
                command: "true".to_string(),
            },
        );
        let events = vec![
            super::super::executor::TurnEvent::CostStats {
                prompt_tokens: 10,
                completion_tokens: 5,
                turn_cost: 0.001,
                cumulative_cost: 0.001,
            },
            super::super::executor::TurnEvent::CostStats {
                prompt_tokens: 20,
                completion_tokens: 10,
                turn_cost: 0.002,
                cumulative_cost: 0.003,
            },
        ];
        let result = collect_turn_metrics(&events, 1.0, &task, dir.path(), None);
        assert_eq!(result.tokens_in, 30);
        assert_eq!(result.tokens_out, 15);
        assert!((result.cost_usd - 0.003).abs() < 0.0001);
    }

    #[test]
    fn bench_collect_metrics_preserves_task_name_and_difficulty() {
        let dir = tempfile::tempdir().unwrap();
        let task = BenchTask {
            name: "named-task".to_string(),
            difficulty: Difficulty::Hard,
            prompt: "p".to_string(),
            setup: HashMap::new(),
            verify: VerifySpec::CommandExitsZero {
                command: "true".to_string(),
            },
            requires_model: false,
            budget_ceiling: None,
        };
        let result = collect_turn_metrics(&[], 1.0, &task, dir.path(), None);
        assert_eq!(result.task_name, "named-task");
        assert_eq!(result.difficulty, Difficulty::Hard);
    }

    #[test]
    fn bench_collect_metrics_run_error_overrides_verify_success() {
        let dir = tempfile::tempdir().unwrap();
        let task = sample_task(
            "err-overrides",
            VerifySpec::CommandExitsZero {
                command: "true".to_string(),
            },
        );
        let result =
            collect_turn_metrics(&[], 1.0, &task, dir.path(), Some("adapter crashed".into()));
        assert!(!result.success);
        assert_eq!(result.error.as_deref(), Some("adapter crashed"));
    }

    #[test]
    fn bench_collect_metrics_zero_duration_is_preserved() {
        let dir = tempfile::tempdir().unwrap();
        let task = sample_task(
            "zero-dur",
            VerifySpec::CommandExitsZero {
                command: "true".to_string(),
            },
        );
        let result = collect_turn_metrics(&[], 0.0, &task, dir.path(), None);
        assert_eq!(result.duration_secs, 0.0);
    }

    #[test]
    fn bench_summary_from_empty_results() {
        let summary = BenchSummary::from_results(&[]);
        assert_eq!(summary.tasks_run, 0);
        assert_eq!(summary.tasks_passed, 0);
        assert_eq!(summary.success_rate, 0.0);
    }

    #[test]
    fn bench_summary_all_pass() {
        let results = vec![TaskResult {
            task_name: "ok".to_string(),
            difficulty: Difficulty::Easy,
            success: true,
            tokens_in: 10,
            tokens_out: 5,
            duration_secs: 1.0,
            cost_usd: 0.001,
            tool_calls: 1,
            compression_passes: 0,
            error: None,
        }];
        let summary = BenchSummary::from_results(&results);
        assert_eq!(summary.tasks_run, 1);
        assert_eq!(summary.tasks_passed, 1);
        assert!((summary.success_rate - 1.0).abs() < 0.001);
    }

    #[test]
    fn bench_summary_accumulates_tokens() {
        let results = vec![
            TaskResult {
                task_name: "a".to_string(),
                difficulty: Difficulty::Easy,
                success: true,
                tokens_in: 100,
                tokens_out: 50,
                duration_secs: 1.0,
                cost_usd: 0.01,
                tool_calls: 2,
                compression_passes: 0,
                error: None,
            },
            TaskResult {
                task_name: "b".to_string(),
                difficulty: Difficulty::Easy,
                success: true,
                tokens_in: 200,
                tokens_out: 100,
                duration_secs: 2.0,
                cost_usd: 0.02,
                tool_calls: 3,
                compression_passes: 0,
                error: None,
            },
        ];
        let summary = BenchSummary::from_results(&results);
        assert_eq!(summary.total_tokens_in, 300);
        assert_eq!(summary.total_tokens_out, 150);
        assert_eq!(summary.total_tool_calls, 5);
        assert!((summary.total_cost_usd - 0.03).abs() < 0.001);
    }

    // ── WO 14.7: Token Budget Challenge ──

    #[test]
    fn budget_challenge_ceilings_are_descending_powers_of_two() {
        // ponytail: the signature challenge runs 128k → 64k → 32k →
        // 16k → 8k. Pinning the exact ceilings catches a silent
        // reorder or unit confusion (bytes vs k).
        assert_eq!(
            BUDGET_CHALLENGE_CEILINGS,
            [131_072, 65_536, 32_768, 16_384, 8_192],
            "ceilings must be 128k/64k/32k/16k/8k descending"
        );
        // Descending invariant: each ceiling is half the previous.
        for w in BUDGET_CHALLENGE_CEILINGS.windows(2) {
            assert_eq!(w[0], w[1] * 2, "each ceiling must be 2x the next");
        }
    }

    #[test]
    fn budget_challenge_task_name_is_token_budget_challenge() {
        // ponytail: run_all dispatches on this exact name; a typo
        // would silently fall through to the single-run path and the
        // signature challenge would never run.
        assert_eq!(BUDGET_CHALLENGE_TASK_NAME, "token_budget_challenge");
    }

    #[test]
    fn budget_challenge_clones_task_with_ceiling_per_run() {
        // The loop sets budget_ceiling on a per-run clone so the
        // runner exports KF_CODE_BUDGET_CEILING for that run. Verify
        // the base task is not mutated and the env helper resolves.
        let base = BenchTask {
            name: BUDGET_CHALLENGE_TASK_NAME.to_string(),
            difficulty: Difficulty::Medium,
            prompt: "p".to_string(),
            setup: HashMap::new(),
            verify: VerifySpec::CommandExitsZero {
                command: "true".to_string(),
            },
            requires_model: true,
            budget_ceiling: None,
        };
        for ceiling in BUDGET_CHALLENGE_CEILINGS {
            let mut run = base.clone();
            run.budget_ceiling = Some(ceiling);
            let env = run.budget_env().expect("ceiling set → env Some");
            assert_eq!(env.0, kf_bench::BUDGET_CEILING_ENV);
            assert_eq!(env.1, ceiling);
        }
        // Base task unchanged.
        assert!(base.budget_ceiling.is_none());
    }

    #[test]
    fn collect_turn_metrics_counts_compaction_reports_as_compression_passes() {
        // ponytail: the six metrics per ceiling include
        // compression_passes; this pins that a CompactionReport event
        // increments the counter (the budget/Stratum pressure signal
        // the challenge measures).
        let dir = tempfile::tempdir().unwrap();
        let task = sample_task(
            "compress",
            VerifySpec::CommandExitsZero {
                command: "true".to_string(),
            },
        );
        let events = vec![
            super::super::executor::TurnEvent::CompactionReport {
                new_messages: vec![],
                dropped_tool_results: 1,
                condensed_assistant_turns: 0,
                original_count: 10,
                compacted_count: 4,
                tokens_before: 1000,
                tokens_after: 400,
            },
            super::super::executor::TurnEvent::CompactionReport {
                new_messages: vec![],
                dropped_tool_results: 0,
                condensed_assistant_turns: 1,
                original_count: 4,
                compacted_count: 2,
                tokens_before: 400,
                tokens_after: 200,
            },
        ];
        let result = collect_turn_metrics(&events, 2.0, &task, dir.path(), None);
        assert_eq!(result.compression_passes, 2);
    }
}
