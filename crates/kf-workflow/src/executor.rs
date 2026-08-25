use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use anyhow::{anyhow, bail, Context, Result};

use crate::{
    resolve_step_refs, BatchErrors, Budget, Step, StepKind, StepOutput, StepRequest, StepRunner,
    Workflow, WorkflowSummary,
};

/// Executes a workflow in dependency order.
///
/// The executor runs all ready steps together. If the host does not yet
/// support parallel `task` dispatch (WO-2), the runner is called sequentially
/// from a single task; otherwise the runner can fan out.
pub struct WorkflowExecutor {
    workflow: Workflow,
}

impl WorkflowExecutor {
    pub fn new(workflow: Workflow) -> Self {
        Self { workflow }
    }

    /// Insert the synthetic `on_exceeded` step output (if configured and not
    /// already present) so the handler's output reaches the model. Deduped
    /// across the max_iterations / max_seconds branches.
    fn insert_on_exceeded(
        on_exceeded: &str,
        reason: String,
        completed: &mut HashSet<String>,
        skipped: &HashSet<String>,
        outputs: &mut HashMap<String, StepOutput>,
    ) {
        if !completed.contains(on_exceeded) && !skipped.contains(on_exceeded) {
            completed.insert(on_exceeded.to_string());
            outputs.insert(
                on_exceeded.to_string(),
                StepOutput {
                    name: on_exceeded.to_string(),
                    kind: StepKind::Agent,
                    persona: String::new(),
                    summary: reason,
                    critique: None,
                    structured_output: None,
                },
            );
        }
    }

    /// Check budget limits (max_iterations, max_seconds). Returns `true` if a
    /// budget is exceeded (and inserts the `on_exceeded` step output if one is
    /// configured), `false` if within budget. Does NOT bail — the caller
    /// returns `Ok(WorkflowSummary { budget_exceeded: true, .. })` so the
    /// configured handler output reaches the model instead of being dropped.
    fn check_budget(
        budget: &Budget,
        iterations: u64,
        start: std::time::Instant,
        completed: &mut HashSet<String>,
        skipped: &HashSet<String>,
        outputs: &mut HashMap<String, StepOutput>,
    ) -> bool {
        if let Some(max_iter) = budget.max_iterations {
            if iterations >= max_iter {
                if let Some(ref on_exceeded) = budget.on_exceeded {
                    Self::insert_on_exceeded(
                        on_exceeded,
                        format!("budget exceeded: max_iterations ({max_iter})"),
                        completed,
                        skipped,
                        outputs,
                    );
                }
                return true;
            }
        }
        if let Some(max_secs) = budget.max_seconds {
            if start.elapsed().as_secs() >= max_secs {
                if let Some(ref on_exceeded) = budget.on_exceeded {
                    Self::insert_on_exceeded(
                        on_exceeded,
                        format!("budget exceeded: max_seconds ({max_secs})"),
                        completed,
                        skipped,
                        outputs,
                    );
                }
                return true;
            }
        }
        false
    }

    /// Build the prompt for an Agent step by prepending fork_from context
    /// and appending dependency context.
    fn build_agent_prompt(step: &Step, outputs: &HashMap<String, StepOutput>) -> Result<String> {
        let mut prompt = step.prompt.clone().unwrap_or_default();

        if let Some(ref fork) = step.fork_from {
            if let Some(fork_out) = outputs.get(fork) {
                prompt = format!(
                    "Context from forked step '{}':\n{}\n\n---\n\n{}",
                    fork, fork_out.summary, prompt
                );
            }
        }

        if !step.depends_on.is_empty() {
            prompt.push_str("\n\nContext from previous steps:\n");
            for dep in &step.depends_on {
                let dep_out = outputs
                    .get(dep)
                    .ok_or_else(|| anyhow!("missing output for dependency {dep}"))?;
                prompt.push_str(&format!(
                    "\n## {} ({}):\n{}",
                    dep, dep_out.persona, dep_out.summary
                ));
                if let Some(critique) = &dep_out.critique {
                    prompt.push_str(&format!("\n\nCritique of {dep}:\n{critique}"));
                }
            }
        }

        Ok(resolve_step_refs(&prompt, outputs))
    }

    /// Execute a FanOut step: spawn concurrent sub-agents, collect results.
    async fn run_fan_out(
        step: &Step,
        outputs: &mut HashMap<String, StepOutput>,
        runner: &Arc<dyn StepRunner>,
        completed: &mut HashSet<String>,
    ) -> Result<Option<StepOutput>> {
        let over_expr = step
            .over
            .as_ref()
            .ok_or_else(|| anyhow!("fan_out step '{}' requires 'over'", step.name))?;
        let as_name = step
            .as_name
            .as_ref()
            .ok_or_else(|| anyhow!("fan_out step '{}' requires 'as_name'", step.name))?;
        let resolved_over = resolve_step_refs(over_expr, outputs);
        let items: Vec<serde_json::Value> =
            serde_json::from_str(&resolved_over).with_context(|| {
                format!(
                    "fan_out step '{}': 'over' must resolve to a JSON array, got: {}",
                    step.name, resolved_over
                )
            })?;
        let prompt_template = step.prompt.clone().unwrap_or_default();
        let persona = step.persona.clone().unwrap_or_else(|| "coder".to_string());

        let max_permits = step.max_parallel.unwrap_or(items.len()).max(1);
        let semaphore = Arc::new(tokio::sync::Semaphore::new(max_permits));
        let mut join_set = tokio::task::JoinSet::new();
        for (i, item) in items.iter().enumerate() {
            let item_str = match item {
                serde_json::Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            let child_prompt = resolve_step_refs(&prompt_template, outputs)
                .replace(&format!("${{{as_name}}}"), &item_str);
            let child_name = format!("{}_{}", step.name, i);
            let sem = semaphore.clone();
            let item_clone = item.clone();
            let runner_clone = runner.clone();
            let persona_clone = persona.clone();
            join_set.spawn(async move {
                let _permit = sem.acquire().await.unwrap();
                let summary = runner_clone
                    .run_step(&child_name, &child_prompt, &persona_clone)
                    .await?;
                Ok::<(usize, serde_json::Value, String), anyhow::Error>((i, item_clone, summary))
            });
        }
        let mut child_summaries = Vec::with_capacity(items.len());
        let mut child_structured = Vec::with_capacity(items.len());
        let mut results: Vec<(usize, serde_json::Value, String)> = Vec::with_capacity(items.len());
        let mut fan_out_err: Option<anyhow::Error> = None;
        while let Some(res) = join_set.join_next().await {
            match res {
                Ok(Ok(tuple)) => results.push(tuple),
                Ok(Err(e)) => {
                    if fan_out_err.is_none() {
                        fan_out_err = Some(e);
                    }
                }
                Err(e) => {
                    if fan_out_err.is_none() {
                        fan_out_err = Some(anyhow!("fan-out task panicked: {e}"));
                    }
                }
            }
        }
        if let Some(e) = fan_out_err {
            if let Some(ref on_error) = step.on_error {
                completed.insert(step.name.clone());
                // ponytail: on_error fan-out routing — single error handler step,
                // not per-item. Upgrade to per-item handlers if needed.
                let step_output = StepOutput {
                    name: step.name.clone(),
                    kind: StepKind::FanOut,
                    persona: persona.clone(),
                    summary: format!("fan-out failed: {e}"),
                    critique: None,
                    structured_output: None,
                };
                completed.insert(on_error.clone());
                let error_output = StepOutput {
                    name: on_error.clone(),
                    kind: StepKind::Agent,
                    persona: String::new(),
                    summary: format!("error handler triggered by: {e}"),
                    critique: None,
                    structured_output: None,
                };
                outputs.insert(step.name.clone(), step_output);
                outputs.insert(on_error.clone(), error_output);
                return Ok(None);
            }
            return Err(e);
        }
        results.sort_by_key(|(i, _, _)| *i);
        for (i, item_clone, summary) in results {
            let child_structured_val = serde_json::from_str::<serde_json::Value>(&summary).ok();
            let child_name = format!("{}_{}", step.name, i);
            child_summaries.push(format!("## {child_name} (item {i}):\n{summary}"));
            child_structured.push(serde_json::json!({
                "index": i,
                "item": item_clone,
                "summary": summary,
                "structured_output": child_structured_val,
            }));
        }

        let combined_summary = child_summaries.join("\n\n");
        let combined_structured = serde_json::Value::Array(child_structured);
        completed.insert(step.name.clone());
        Ok(Some(StepOutput {
            name: step.name.clone(),
            kind: StepKind::FanOut,
            persona,
            summary: combined_summary,
            critique: None,
            structured_output: Some(combined_structured),
        }))
    }

    /// Execute a FanIn step: aggregate outputs from fan-out dependencies.
    fn run_fan_in(
        step: &Step,
        outputs: &HashMap<String, StepOutput>,
        completed: &mut HashSet<String>,
    ) -> StepOutput {
        let mut combined = String::new();
        for dep in &step.depends_on {
            if let Some(dep_out) = outputs.get(dep) {
                combined.push_str(&format!("## {}:\n{}\n\n", dep, dep_out.summary));
            }
        }
        completed.insert(step.name.clone());
        StepOutput {
            name: step.name.clone(),
            kind: StepKind::FanIn,
            persona: String::new(),
            summary: if combined.is_empty() {
                "no fan-out results".to_string()
            } else {
                combined.trim_end().to_string()
            },
            critique: None,
            structured_output: None,
        }
    }

    /// Handle a batch-level error. When the error is a `BatchErrors` (the
    /// parallel runner joined all handles and partitioned ok/err), preserve
    /// the succeeded siblings' real outputs and mark only the actually-failed
    /// steps — instead of the old behavior of marking every task in the
    /// batch failed (including successful ones). For a plain `anyhow::Error`
    /// (sequential default runner, or a panic that lost per-step info), fall
    /// back to the old all-failed path. Returns `Ok(())` to continue (when an
    /// `on_error` route exists) or `Err(e)` to abort.
    fn handle_batch_error(
        tasks: &[StepRequest],
        workflow: &Workflow,
        error: anyhow::Error,
        completed: &mut HashSet<String>,
        outputs: &mut HashMap<String, StepOutput>,
    ) -> Result<()> {
        // If the runner partitioned ok/err, preserve sibling successes.
        if let Some(batch) = error.downcast_ref::<BatchErrors>() {
            return Self::handle_partitioned_batch_error(workflow, batch, completed, outputs);
        }

        // Fallback: plain error (sequential default runner, or a panic that
        // lost per-step info). Old all-failed behavior — every task marked
        // failed with the batch error.
        let has_error_route = tasks.iter().any(|t| {
            workflow
                .steps
                .iter()
                .find(|s| s.name == t.name)
                .and_then(|s| s.on_error.clone())
                .is_some()
        });
        if !has_error_route {
            return Err(error);
        }
        for task in tasks {
            let step = workflow
                .steps
                .iter()
                .find(|s| s.name == task.name)
                .ok_or_else(|| {
                    anyhow::anyhow!("batch error step '{}' not found in workflow", task.name)
                })?;
            outputs.insert(
                task.name.clone(),
                StepOutput {
                    name: task.name.clone(),
                    kind: step.kind.clone(),
                    persona: step.persona.clone().unwrap_or_default(),
                    summary: format!("step failed: {error}"),
                    critique: None,
                    structured_output: None,
                },
            );
            completed.insert(task.name.clone());
        }
        // Route to the first on_error step found.
        if let Some(on_error) = tasks
            .iter()
            .filter_map(|t| {
                workflow
                    .steps
                    .iter()
                    .find(|s| s.name == t.name)
                    .and_then(|s| s.on_error.clone())
            })
            .next()
        {
            completed.insert(on_error.clone());
            outputs.insert(
                on_error.clone(),
                StepOutput {
                    name: on_error.clone(),
                    kind: StepKind::Agent,
                    persona: String::new(),
                    summary: format!("error handler triggered by: {error}"),
                    critique: None,
                    structured_output: None,
                },
            );
        }
        Ok(())
    }

    /// Handle a `BatchErrors` (partitioned ok/err) batch result: insert the
    /// succeeded siblings' real outputs (with persona + structured_output)
    /// and mark only the actually-failed steps. Route to the first failed
    /// step's `on_error` if any; otherwise propagate the error.
    fn handle_partitioned_batch_error(
        workflow: &Workflow,
        batch: &BatchErrors,
        completed: &mut HashSet<String>,
        outputs: &mut HashMap<String, StepOutput>,
    ) -> Result<()> {
        // Insert succeeded siblings as real outputs (not the canned
        // "step failed" the old path applied to every task).
        for (name, summary) in &batch.successes {
            let step = workflow
                .steps
                .iter()
                .find(|s| s.name == *name)
                .ok_or_else(|| anyhow::anyhow!("batch step '{name}' not found in workflow"))?;
            let persona = step
                .persona
                .clone()
                .unwrap_or_else(|| format!("{:?}", step.kind).to_lowercase());
            let structured_output: Option<serde_json::Value> = serde_json::from_str(summary).ok();
            outputs.insert(
                name.clone(),
                StepOutput {
                    name: name.clone(),
                    kind: step.kind.clone(),
                    persona,
                    summary: summary.clone(),
                    critique: None,
                    structured_output,
                },
            );
            completed.insert(name.clone());
        }

        // Mark only the actually-failed steps.
        let mut first_error_msg: Option<String> = None;
        let mut on_error_route: Option<String> = None;
        for (name, err) in &batch.failures {
            if first_error_msg.is_none() {
                first_error_msg = Some(err.to_string());
            }
            let step = workflow
                .steps
                .iter()
                .find(|s| s.name == *name)
                .ok_or_else(|| anyhow::anyhow!("batch step '{name}' not found in workflow"))?;
            if on_error_route.is_none() {
                on_error_route = step.on_error.clone();
            }
            outputs.insert(
                name.clone(),
                StepOutput {
                    name: name.clone(),
                    kind: step.kind.clone(),
                    persona: step.persona.clone().unwrap_or_default(),
                    summary: format!("step failed: {err}"),
                    critique: None,
                    structured_output: None,
                },
            );
            completed.insert(name.clone());
        }

        // Route to the first failed step's on_error if configured.
        if let Some(on_error) = on_error_route {
            let trigger = first_error_msg.unwrap_or_else(|| "batch failure".to_string());
            completed.insert(on_error.clone());
            outputs.insert(
                on_error.clone(),
                StepOutput {
                    name: on_error.clone(),
                    kind: StepKind::Agent,
                    persona: String::new(),
                    summary: format!("error handler triggered by: {trigger}"),
                    critique: None,
                    structured_output: None,
                },
            );
            return Ok(());
        }

        // No on_error route — propagate the first failure.
        Err(anyhow!(
            "{}",
            first_error_msg.unwrap_or_else(|| "batch failed with no error detail".to_string())
        ))
    }

    /// Run the workflow to completion, invoking `runner` for each step.
    ///
    /// The runner receives the step prompt concatenated with the human-readable
    /// summaries of all `depends_on` steps. If `cancellation` is set, the
    /// executor stops before scheduling the next step batch and returns an
    /// error. Steps with a `condition` that evaluates to non-zero are skipped.
    /// Steps with `on_error` route failures to the named step instead of
    /// aborting. Budget limits (if set) are checked between batches.
    pub async fn run(
        &self,
        runner: Arc<dyn StepRunner>,
        cancellation: Option<&std::sync::atomic::AtomicBool>,
    ) -> Result<WorkflowSummary> {
        let mut completed: HashSet<String> = HashSet::new();
        let mut skipped: HashSet<String> = HashSet::new();
        let mut outputs: HashMap<String, StepOutput> = HashMap::new();
        let start = std::time::Instant::now();
        let mut iterations: u64 = 0;

        while completed.len() + skipped.len() < self.workflow.steps.len() {
            if let Some(cancel) = cancellation {
                if cancel.load(std::sync::atomic::Ordering::SeqCst) {
                    bail!("workflow cancelled");
                }
            }

            // Budget checks. On exceed, return Ok with budget_exceeded=true
            // so the on_exceeded handler output (if any) reaches the model —
            // bailing would drop WorkflowSummary, the only carrier of outputs.
            if let Some(ref budget) = self.workflow.budget {
                if Self::check_budget(
                    budget,
                    iterations,
                    start,
                    &mut completed,
                    &skipped,
                    &mut outputs,
                ) {
                    return Ok(WorkflowSummary {
                        workflow_name: self.workflow.name.clone(),
                        outputs,
                        budget_exceeded: true,
                    });
                }
            }

            // Also consider skipped steps as "done" for dependency resolution.
            let effective_completed: HashSet<String> = completed.union(&skipped).cloned().collect();
            let ready = self.workflow.ready_steps(&effective_completed);
            if ready.is_empty() {
                bail!("workflow has no ready steps but is not complete (possible cycle)");
            }

            iterations += 1;

            // Evaluate conditions and filter out skipped steps.
            let mut ready_filtered: Vec<String> = Vec::new();
            for name in &ready {
                let step = self
                    .workflow
                    .steps
                    .iter()
                    .find(|s| &s.name == name)
                    .ok_or_else(|| anyhow!("missing step {name}"))?;
                if let Some(ref cond) = step.condition {
                    if !runner.eval_condition(cond).await {
                        skipped.insert(name.clone());
                        continue;
                    }
                }
                ready_filtered.push(name.clone());
            }

            if ready_filtered.is_empty() {
                // All steps in this batch were skipped; continue to next batch.
                continue;
            }

            // Process FanOut and FanIn steps inline, collect regular steps for batch.
            let mut tasks: Vec<StepRequest> = Vec::new();
            for name in &ready_filtered {
                let step = self
                    .workflow
                    .steps
                    .iter()
                    .find(|s| &s.name == name)
                    .ok_or_else(|| anyhow!("missing step {name}"))?;

                match step.kind {
                    StepKind::Agent => {
                        let prompt = Self::build_agent_prompt(step, &outputs)?;
                        tasks.push(StepRequest {
                            name: step.name.clone(),
                            kind: StepKind::Agent,
                            prompt,
                            persona: step.persona.clone().unwrap_or_default(),
                            command: String::new(),
                            tool_name: String::new(),
                            tool_arguments: serde_json::Value::Null,
                            with_critique: step.critique.unwrap_or(false),
                        });
                    }
                    StepKind::Bash => {
                        let command = resolve_step_refs(
                            step.command.as_deref().unwrap_or_default(),
                            &outputs,
                        );
                        tasks.push(StepRequest {
                            name: step.name.clone(),
                            kind: StepKind::Bash,
                            prompt: String::new(),
                            persona: String::new(),
                            command,
                            tool_name: String::new(),
                            tool_arguments: serde_json::Value::Null,
                            with_critique: false,
                        });
                    }
                    StepKind::Tool => {
                        let prompt =
                            resolve_step_refs(step.prompt.as_deref().unwrap_or_default(), &outputs);
                        tasks.push(StepRequest {
                            name: step.name.clone(),
                            kind: StepKind::Tool,
                            prompt,
                            persona: String::new(),
                            command: String::new(),
                            tool_name: step.tool_name.clone().unwrap_or_default(),
                            tool_arguments: step
                                .tool_arguments
                                .clone()
                                .unwrap_or(serde_json::Value::Object(Default::default())),
                            with_critique: false,
                        });
                    }
                    StepKind::FanOut => {
                        let result =
                            Self::run_fan_out(step, &mut outputs, &runner, &mut completed).await?;
                        if let Some(output) = result {
                            outputs.insert(step.name.clone(), output);
                        }
                        // run_fan_out already handled on_error routing if it
                        // returned Ok(None); continue to next step.
                        continue;
                    }
                    StepKind::FanIn => {
                        let output = Self::run_fan_in(step, &outputs, &mut completed);
                        outputs.insert(step.name.clone(), output);
                        continue;
                    }
                }
            }

            // If only fan-out/fan-in steps were in this batch, continue to next iteration.
            if tasks.is_empty() {
                continue;
            }

            // Run the batch. The runner decides whether to parallelise.
            let batch_result = runner.run_batch(tasks.clone()).await;
            let results = match batch_result {
                Ok(r) => r,
                Err(e) => {
                    Self::handle_batch_error(
                        &tasks,
                        &self.workflow,
                        e,
                        &mut completed,
                        &mut outputs,
                    )?;
                    continue;
                }
            };

            // Pair results by step name.
            let by_name: HashMap<&String, &String> = results.iter().map(|(n, s)| (n, s)).collect();
            for task in &tasks {
                let step = self
                    .workflow
                    .steps
                    .iter()
                    .find(|s| s.name == task.name)
                    .ok_or_else(|| anyhow!("missing step {}", task.name))?;
                let summary = by_name
                    .get(&task.name)
                    .ok_or_else(|| anyhow!("runner returned no result for step '{}'", task.name))?;
                let critique = if task.with_critique {
                    let critique_prompt = format!(
                        "You are a critical reviewer. Evaluate the following output for risks, gaps, and correctness. Keep it concise.\n\nOutput to critique:\n{summary}"
                    );
                    Some(
                        runner
                            .run_step(&format!("{}-critique", task.name), &critique_prompt, "plan")
                            .await?,
                    )
                } else {
                    None
                };
                let persona = step
                    .persona
                    .clone()
                    .unwrap_or_else(|| format!("{:?}", step.kind).to_lowercase());
                let structured_output: Option<serde_json::Value> =
                    serde_json::from_str(summary).ok();
                completed.insert(task.name.clone());
                outputs.insert(
                    task.name.clone(),
                    StepOutput {
                        name: task.name.clone(),
                        kind: step.kind.clone(),
                        persona,
                        summary: summary.to_string(),
                        critique,
                        structured_output,
                    },
                );
            }
        }

        Ok(WorkflowSummary {
            workflow_name: self.workflow.name.clone(),
            outputs,
            budget_exceeded: false,
        })
    }
}
