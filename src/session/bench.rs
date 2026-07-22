//! Headless benchmark session execution.
//!
//! Runs a single benchmark task against a kirkforge executor, collects
//! metrics from turn events, and verifies the result.

use crate::shared::{Config, SharedConfig};
use kirkforge_bench::{BenchReport, BenchSummary, BenchTask, TaskResult};
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
            _ => {}
        }
    }

    let success = if run_error.is_none() {
        kirkforge_bench::verify_task(task, sandbox_path).unwrap_or(false)
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
        error: run_error,
    }
}

/// Run a single benchmark task.
///
/// Creates a temp sandbox dir, applies setup files, starts a headless
/// kirkforge session, sends the prompt, waits for completion (or timeout),
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
    task_config.default_model = model.to_string();
    task_config.sandbox_dir = Some(sandbox_path.to_string_lossy().to_string());
    task_config.auto_approve = true;
    task_config.dry_run = false;
    super::config::freeze_launch_sandbox(&mut task_config);

    let shared_config: SharedConfig = std::sync::Arc::new(std::sync::RwLock::new(task_config));

    let ollama_host = shared_config.read().unwrap().ollama_host.clone();
    let anthropic_provider = shared_config.read().unwrap().anthropic_provider.clone();
    let request_timeout = shared_config.read().unwrap().request_timeout_secs;

    // Create adapter.
    let adapter = crate::adapters::adapter_for_with_provider(
        model,
        &ollama_host,
        None,
        &anthropic_provider,
        request_timeout,
    );

    // Open conversation log in sandbox.
    let data_dir = sandbox_path.join("kirkforge-data");
    std::fs::create_dir_all(&data_dir)?;
    let session_id = format!("bench-{}", task.name);
    let log_path = data_dir.join(format!("{session_id}.conv.ndjson"));
    let (conversation, _open_outcome) = super::conversation::ConversationLog::open(log_path)?;

    // Build an empty toolset for bench runs. The model can still make tool
    // calls but they will be auto-approved (safe for benchmarking).
    let toolset = super::toolset::CompositeToolset::empty();

    // Construct executor.
    let mut executor = super::executor::Executor::with_log_and_undo(
        adapter,
        toolset,
        shared_config,
        conversation,
        None,
        None,
    );
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

/// Run all tasks and collect results.
pub async fn run_all(
    tasks: &[BenchTask],
    model: &str,
    config: &Config,
    timeout_secs: u64,
) -> BenchReport {
    let mut results = Vec::new();
    for task in tasks {
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
    use kirkforge_bench::{Difficulty, VerifySpec};
    use std::collections::HashMap;

    fn sample_task(name: &str, verify: VerifySpec) -> BenchTask {
        BenchTask {
            name: name.to_string(),
            difficulty: Difficulty::Easy,
            prompt: "test prompt".to_string(),
            setup: HashMap::new(),
            verify,
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
}
