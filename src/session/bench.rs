//! Headless benchmark session execution.
//!
//! Runs a single benchmark task against a kirkforge executor, collects
//! metrics from turn events, and verifies the result.

use crate::shared::{Config, SharedConfig};
use kirkforge_bench::{BenchReport, BenchSummary, BenchTask, TaskResult};
use std::time::Instant;

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
    let mut tokens_in: u64 = 0;
    let mut tokens_out: u64 = 0;
    let mut cost_usd: f64 = 0.0;
    let mut tool_calls: u32 = 0;
    let mut run_error: Option<String> = None;

    match turn_result {
        Ok(Ok(events)) => {
            for event in &events {
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
        }
        Ok(Err(e)) => {
            run_error = Some(e.to_string());
        }
        Err(_) => {
            run_error = Some(format!("timeout after {timeout_secs}s"));
        }
    }

    // Run verification.
    let success = if run_error.is_none() {
        kirkforge_bench::verify_task(task, &sandbox_path).unwrap_or(false)
    } else {
        false
    };

    Ok(TaskResult {
        task_name: task.name.clone(),
        difficulty: task.difficulty,
        success,
        tokens_in,
        tokens_out,
        duration_secs: duration,
        cost_usd,
        tool_calls,
        error: run_error,
    })
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
