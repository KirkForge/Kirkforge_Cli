// `kf-code bench <subcommand>` dispatch + handlers.
// Extracted from the binary root — pure move, no behaviour change.

use kf_code::cli::BenchCommand;

pub(super) async fn handle_bench_command(
    command: kf_code::cli::BenchCommand,
) -> anyhow::Result<()> {
    match command {
        BenchCommand::Run {
            tasks,
            model,
            output,
            summary,
            timeout,
        } => handle_bench_run(tasks, model, output, summary, timeout).await,
        BenchCommand::Compare {
            baseline,
            current,
            summary,
            fail_on_regression,
        } => handle_bench_compare(baseline, current, summary, fail_on_regression),
        BenchCommand::List { tasks } => handle_bench_list(tasks),
        BenchCommand::ExportTasks {
            tasks,
            dir,
            include_kf_only,
        } => handle_bench_export_tasks(tasks, dir, include_kf_only),
        BenchCommand::VerifyOnly { tasks, task } => handle_bench_verify_only(tasks, task),
        BenchCommand::RunModels {
            tasks,
            models,
            output,
            summary,
            timeout,
        } => handle_bench_run_models(tasks, models, output, summary, timeout).await,
    }
}

async fn handle_bench_run(
    tasks: std::path::PathBuf,
    model: Option<String>,
    output: Option<std::path::PathBuf>,
    summary: Option<std::path::PathBuf>,
    timeout: u64,
) -> anyhow::Result<()> {
    let config = kf_code::session::config::load_or_create_config_strict()?;
    let model_name = model.unwrap_or_else(|| config.model.default_model.clone());
    let bench_tasks = kf_bench::load_tasks(&tasks)?;
    if bench_tasks.is_empty() {
        anyhow::bail!("no task files found in {}", tasks.display());
    }
    eprintln!(
        "running {} benchmark tasks with model {}",
        bench_tasks.len(),
        model_name
    );
    let report =
        kf_code::session::bench::run_all(&bench_tasks, &model_name, &config, timeout).await;
    eprintln!(
        "{}/{} tasks passed ({:.0}%)",
        report.summary.tasks_passed,
        report.summary.tasks_run,
        report.summary.success_rate * 100.0
    );
    let json_path = output.unwrap_or_else(|| {
        std::path::PathBuf::from(format!(
            "bench-report-{}.json",
            chrono::Local::now().format("%Y%m%d-%H%M%S")
        ))
    });
    kf_bench::write_report(&report, &json_path)?;
    eprintln!("report written to {}", json_path.display());
    if let Some(md_path) = summary {
        kf_bench::write_markdown_summary(&report, &md_path)?;
        eprintln!("summary written to {}", md_path.display());
    }
    // WO 38.10: bench run must reflect success in its exit code. A 0%
    // pass rate is a failure for CI gates; previously the command exited
    // 0 unconditionally. We bail only when tasks actually ran and none
    // passed — a run with 0 tasks already bailed above. Bailing (rather
    // than `process::exit`) routes through the error classifier
    // (General → exit 1) and still prints the report first.
    if report.summary.tasks_run > 0 && report.summary.tasks_passed == 0 {
        anyhow::bail!(
            "bench run: 0/{} tasks passed (0%)",
            report.summary.tasks_run
        );
    }
    Ok(())
}

async fn handle_bench_run_models(
    tasks: std::path::PathBuf,
    models: Vec<String>,
    output: Option<std::path::PathBuf>,
    summary: Option<std::path::PathBuf>,
    timeout: u64,
) -> anyhow::Result<()> {
    if models.is_empty() {
        anyhow::bail!("--models requires at least one model name");
    }
    let config = kf_code::session::config::load_or_create_config_strict()?;
    let bench_tasks = kf_bench::load_tasks(&tasks)?;
    if bench_tasks.is_empty() {
        anyhow::bail!("no task files found in {}", tasks.display());
    }
    eprintln!(
        "running {} benchmark tasks across {} model(s)",
        bench_tasks.len(),
        models.len()
    );

    let mut reports = Vec::new();
    for model in &models {
        eprintln!("→ model: {model}");
        let report = kf_code::session::bench::run_all(&bench_tasks, model, &config, timeout).await;
        eprintln!(
            "  {}/{} tasks passed ({:.0}%)",
            report.summary.tasks_passed,
            report.summary.tasks_run,
            report.summary.success_rate * 100.0
        );
        if let Some(out_dir) = &output {
            std::fs::create_dir_all(out_dir)?;
            let safe_name = model.replace([':', '/'], "_");
            let json_path = out_dir.join(format!("{safe_name}.json"));
            kf_bench::write_report(&report, &json_path)?;
            eprintln!("  report written to {}", json_path.display());
        }
        reports.push(report);
    }

    let comparison = kf_bench::write_model_comparison(&reports);
    println!("{comparison}");
    if let Some(md_path) = summary {
        if let Some(parent) = md_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&md_path, &comparison)?;
        eprintln!("comparison written to {}", md_path.display());
    }
    // WO 46.26: mirror the WO 38.10 0/N guard from handle_bench_run. A
    // model whose tasks all failed is a CI failure; previously the
    // command exited 0 unconditionally. We bail after writing the
    // per-model reports + comparison so the user keeps the artifacts.
    // A run with 0 tasks already bailed above.
    if let Some(failed) = reports
        .iter()
        .find(|r| r.summary.tasks_run > 0 && r.summary.tasks_passed == 0)
    {
        anyhow::bail!(
            "bench run-models: model {} got 0/{} tasks passed (0%)",
            failed.model,
            failed.summary.tasks_run
        );
    }
    Ok(())
}

fn handle_bench_compare(
    baseline: std::path::PathBuf,
    current: std::path::PathBuf,
    summary: Option<std::path::PathBuf>,
    fail_on_regression: Option<f64>,
) -> anyhow::Result<()> {
    let baseline_json = std::fs::read_to_string(&baseline)?;
    let current_json = std::fs::read_to_string(&current)?;
    let baseline_report: kf_bench::BenchReport = serde_json::from_str(&baseline_json)?;
    let current_report: kf_bench::BenchReport = serde_json::from_str(&current_json)?;

    if let Some(threshold_pct) = fail_on_regression {
        // WO 10.9: regression gate. The CLI flag is a percentage (e.g.
        // 10 = 10 percentage points); compare_with_threshold takes a
        // fraction (0.10).
        let threshold = threshold_pct / 100.0;
        let result = kf_bench::compare_with_threshold(&baseline_report, &current_report, threshold);
        let delta = &result.delta;
        println!("Delta: {} → {}", delta.baseline_model, delta.current_model);
        println!(
            "Success rate: {:+.0}% | Δtokens_in: {:+} | Δcost: ${:+.4}",
            delta.success_rate_delta * 100.0,
            delta.total_tokens_in_delta,
            delta.total_cost_delta_usd,
        );
        if let Some(md_path) = summary {
            kf_bench::write_markdown_delta(delta, &md_path)?;
            eprintln!("delta summary written to {}", md_path.display());
        }
        if result.regression_detected {
            eprintln!(
                "❌ Bench regression detected: success rate dropped by {:.0} percentage points \
                 (threshold: {:.0} percentage points).",
                -delta.success_rate_delta * 100.0,
                threshold_pct,
            );
            anyhow::bail!("bench regression detected");
        }
        eprintln!(
            "✓ No bench regression (success rate delta {:+.0}%, threshold {:.0} percentage points).",
            delta.success_rate_delta * 100.0,
            threshold_pct,
        );
        return Ok(());
    }

    // Historical path (no --fail-on-regression): always exits 0.
    let delta = kf_bench::compare_reports(&baseline_report, &current_report);
    println!("Delta: {} → {}", delta.baseline_model, delta.current_model);
    println!(
        "Success rate: {:+.0}% | Δtokens_in: {:+} | Δcost: ${:+.4}",
        delta.success_rate_delta * 100.0,
        delta.total_tokens_in_delta,
        delta.total_cost_delta_usd,
    );
    if let Some(md_path) = summary {
        kf_bench::write_markdown_delta(&delta, &md_path)?;
        eprintln!("delta summary written to {}", md_path.display());
    }
    Ok(())
}

fn handle_bench_list(tasks: std::path::PathBuf) -> anyhow::Result<()> {
    let task_infos = kf_bench::list_tasks(&tasks)?;
    if task_infos.is_empty() {
        println!("No tasks found in {}", tasks.display());
        return Ok(());
    }
    println!(
        "{:<30} {:<12} {:<8} Verify",
        "Name", "Difficulty", "KF-only"
    );
    println!("{}", "-".repeat(65));
    for t in &task_infos {
        let diff_str = match t.difficulty {
            kf_bench::Difficulty::Easy => "easy",
            kf_bench::Difficulty::Medium => "medium",
            kf_bench::Difficulty::Hard => "hard",
        };
        println!(
            "{:<30} {:<12} {:<8} {}",
            t.name,
            diff_str,
            if t.kf_only { "yes" } else { "no" },
            t.verify_type
        );
    }
    println!("\n{} task(s)", task_infos.len());
    Ok(())
}

fn handle_bench_verify_only(tasks: std::path::PathBuf, task: Option<String>) -> anyhow::Result<()> {
    let bench_tasks = kf_bench::load_tasks(&tasks)?;
    if bench_tasks.is_empty() {
        anyhow::bail!("no task files found in {}", tasks.display());
    }
    let filtered: Vec<_> = match &task {
        Some(name) => bench_tasks
            .into_iter()
            .filter(|t| t.name == *name)
            .collect(),
        None => bench_tasks,
    };
    if filtered.is_empty() {
        anyhow::bail!("no matching task found");
    }
    let tmp = tempfile::tempdir()?;
    let mut passed = 0;
    let mut skipped = 0;
    for bt in &filtered {
        let result = kf_bench::verify_only(bt, tmp.path());
        let is_skip = result
            .error
            .as_deref()
            .map(|e| e.contains("skipped (requires model)"))
            .unwrap_or(false);
        let status = if is_skip {
            "SKIP"
        } else if result.success {
            "PASS"
        } else {
            "FAIL"
        };
        println!(
            "[{}] {} ({})",
            status,
            bt.name,
            result.error.unwrap_or_default()
        );
        if is_skip {
            skipped += 1;
        } else if result.success {
            passed += 1;
        }
    }
    println!(
        "{}/{} tasks verified, {} skipped (requires model)",
        passed,
        filtered.len(),
        skipped
    );
    Ok(())
}

fn handle_bench_export_tasks(
    tasks: std::path::PathBuf,
    dir: std::path::PathBuf,
    include_kf_only: bool,
) -> anyhow::Result<()> {
    let count = kf_bench::export_tasks(&tasks, &dir, include_kf_only)?;
    println!(
        "exported {} task{} to {} (kf_only: {})",
        count,
        if count == 1 { "" } else { "s" },
        dir.display(),
        if include_kf_only {
            "included"
        } else {
            "excluded"
        }
    );
    Ok(())
}
