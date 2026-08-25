//! Programmable JSON workflow engine for KirkForge.
//!
//! Workflows are user-editable DAGs of steps. Each step has a **kind**:
//! `agent` (subagent with a prompt and persona), `bash` (shell command), or
//! `tool` (registered tool call). Steps declare `depends_on` for ordering.
//!
//! # Schema
//!
//! ```json
//! {
//!   "name": "add-feature",
//!   "steps": [
//!     {"name": "explore", "prompt": "Map the codebase areas relevant to <X>", "persona": "explore"},
//!     {"name": "tests", "kind": "bash", "command": "cargo test"},
//!     {"name": "plan", "prompt": "Design the implementation for <X>", "persona": "plan", "depends_on": ["explore"]},
//!     {"name": "execute", "prompt": "Implement <X> per the plan", "persona": "coder", "depends_on": ["plan"]}
//!   ]
//! }
//! ```
//!
//! `kind` defaults to `"agent"` for backward compatibility. Agent steps require
//! `prompt` and `persona` (must be `explore`, `plan`, or `coder`). Bash steps
//! require `command`. Tool steps require `tool_name` and `tool_arguments`.
//! `critique` (agent-only) is an optional bool; when true, the step is
//! additionally run with the `plan` persona and the critique output is appended
//! to the step's context.
//!
//! # Loading
//!
//! Workflow files are JSON loaded from `.kf-code/workflows/<name>.json` or
//! `~/.local/share/kf-code/workflows/<name>.json`. Built-in templates live
//! in `crates/kf-workflow/templates/` and are copied to the user share
//! directory on first use.

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Step kind: agent (subagent), bash (shell command), tool (registered tool),
/// fan_out (parallel fan-out), or fan_in (fan-in join point).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum StepKind {
    Agent,
    Bash,
    Tool,
    FanOut,
    FanIn,
}

fn default_step_kind() -> StepKind {
    StepKind::Agent
}

fn is_default_step_kind(k: &StepKind) -> bool {
    matches!(k, StepKind::Agent)
}

/// One step in a workflow.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Step {
    /// Unique identifier within the workflow.
    pub name: String,
    /// Step kind: agent, bash, or tool. Defaults to `"agent"`.
    #[serde(
        default = "default_step_kind",
        skip_serializing_if = "is_default_step_kind"
    )]
    pub kind: StepKind,
    /// Prompt sent to the subagent. Required for agent steps.
    #[serde(default)]
    pub prompt: Option<String>,
    /// Persona/tool restriction: explore, plan, or coder. Required for agent steps.
    #[serde(default)]
    pub persona: Option<String>,
    /// Shell command to run. Required for bash steps.
    #[serde(default)]
    pub command: Option<String>,
    /// Tool name to invoke. Required for tool steps.
    #[serde(default)]
    pub tool_name: Option<String>,
    /// Tool arguments (JSON object). Required for tool steps.
    #[serde(default)]
    pub tool_arguments: Option<serde_json::Value>,
    /// Prior step names that must complete before this one runs.
    #[serde(default)]
    pub depends_on: Vec<String>,
    /// If true, also run the step through the `plan` persona as a critique
    /// and append that output to the step summary. Agent steps only.
    #[serde(default)]
    pub critique: Option<bool>,
    /// Shell condition; step is skipped if the condition evaluates to non-zero.
    /// ponytail: eval("sh -c <condition>") — upgrade to expression parser if needed.
    #[serde(default)]
    pub condition: Option<String>,
    /// Step name to route to on failure instead of aborting the whole workflow.
    #[serde(default)]
    pub on_error: Option<String>,
    /// When set, the step clones conversation context from the named prior step
    /// instead of starting fresh. The forked-from step's output is prepended to
    /// the prompt.
    #[serde(default)]
    pub fork_from: Option<String>,
    /// For FanOut: the `$(step_name.field)` expression that resolves to a JSON
    /// array. Each element spawns one sub-step with `as_name` bound to the element.
    #[serde(default)]
    pub over: Option<String>,
    /// For FanOut: the variable name bound to each fan-out element in the prompt.
    #[serde(default)]
    pub as_name: Option<String>,
    /// For FanOut: maximum number of parallel sub-steps.
    #[serde(default)]
    pub max_parallel: Option<usize>,
}

/// Budget limits for a workflow.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Budget {
    /// Maximum total tokens across all steps.
    #[serde(default)]
    pub max_tokens: Option<u64>,
    /// Maximum wall-clock seconds for the entire workflow.
    #[serde(default)]
    pub max_seconds: Option<u64>,
    /// Maximum number of step iterations (steps executed).
    #[serde(default)]
    pub max_iterations: Option<u64>,
    /// Step name to route to when the budget is exceeded.
    #[serde(default)]
    pub on_exceeded: Option<String>,
}

/// A workflow: a named DAG of steps.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Workflow {
    pub name: String,
    #[serde(default)]
    pub steps: Vec<Step>,
    /// Budget limits for the workflow.
    #[serde(default)]
    pub budget: Option<Budget>,
}

impl Workflow {
    /// Load a workflow from JSON bytes.
    pub fn from_json(data: &[u8]) -> Result<Self> {
        let wf: Workflow =
            serde_json::from_slice(data).with_context(|| "failed to parse workflow JSON")?;
        wf.validate()?;
        Ok(wf)
    }

    /// Load a workflow from a file path.
    pub fn from_file<P: AsRef<Path>>(path: P) -> Result<Self> {
        let data = std::fs::read(path.as_ref())
            .with_context(|| format!("failed to read {}", path.as_ref().display()))?;
        Self::from_json(&data)
    }

    /// Validate the workflow: duplicate names, kind-specific required fields,
    /// unknown personas, missing dependencies, and dependency cycles.
    pub fn validate(&self) -> Result<()> {
        let mut names = HashSet::new();
        for step in &self.steps {
            if !names.insert(step.name.clone()) {
                bail!("duplicate step name: {}", step.name);
            }
            match step.kind {
                StepKind::Agent => {
                    if step.prompt.as_ref().is_none_or(|s| s.is_empty()) {
                        bail!("step '{}' is an agent step but has no prompt", step.name);
                    }
                    if let Some(ref p) = step.persona {
                        if !matches!(p.as_str(), "explore" | "plan" | "coder") {
                            bail!(
                                "step '{}' has unknown persona '{p}'; expected explore/plan/coder",
                                step.name
                            );
                        }
                    } else {
                        bail!("step '{}' is an agent step but has no persona", step.name);
                    }
                }
                StepKind::Bash => {
                    if step.command.as_ref().is_none_or(|s| s.is_empty()) {
                        bail!("step '{}' is a bash step but has no command", step.name);
                    }
                }
                StepKind::Tool => {
                    if step.tool_name.as_ref().is_none_or(|s| s.is_empty()) {
                        bail!("step '{}' is a tool step but has no tool_name", step.name);
                    }
                }
                StepKind::FanOut => {
                    if step.over.as_ref().is_none_or(|s| s.is_empty()) {
                        bail!("step '{}' is a fan_out step but has no 'over'", step.name);
                    }
                    if step.as_name.as_ref().is_none_or(|s| s.is_empty()) {
                        bail!(
                            "step '{}' is a fan_out step but has no 'as_name'",
                            step.name
                        );
                    }
                    if step.max_parallel == Some(0) {
                        bail!(
                            "step '{}' has max_parallel=0 which would deadlock",
                            step.name
                        );
                    }
                }
                StepKind::FanIn => {
                    // FanIn is a join point — no extra required fields.
                }
            }
            if let Some(ref on_error) = step.on_error {
                if !self.steps.iter().any(|s| &s.name == on_error) {
                    bail!(
                        "step '{}' on_error references unknown step '{on_error}'",
                        step.name
                    );
                }
            }
            if let Some(ref fork_from) = step.fork_from {
                if !self.steps.iter().any(|s| &s.name == fork_from) {
                    bail!(
                        "step '{}' fork_from references unknown step '{fork_from}'",
                        step.name
                    );
                }
            }
            for dep in &step.depends_on {
                if dep == &step.name {
                    bail!("step '{}' depends on itself", step.name);
                }
                if !self.steps.iter().any(|s| &s.name == dep) {
                    bail!("step '{}' depends on unknown step '{dep}'", step.name);
                }
            }
        }
        if let Some(ref budget) = self.budget {
            if let Some(ref on_exceeded) = budget.on_exceeded {
                if !self.steps.iter().any(|s| &s.name == on_exceeded) {
                    bail!("budget on_exceeded references unknown step '{on_exceeded}'");
                }
            }
        }
        if let Some(cycle) = self.find_cycle() {
            bail!("dependency cycle detected: {}", cycle.join(" -> "));
        }
        Ok(())
    }

    /// Return true if the workflow contains a cycle.
    pub fn has_cycle(&self) -> bool {
        self.find_cycle().is_some()
    }

    fn find_cycle(&self) -> Option<Vec<String>> {
        let index: HashMap<String, usize> = self
            .steps
            .iter()
            .enumerate()
            .map(|(i, s)| (s.name.clone(), i))
            .collect();
        let n = self.steps.len();
        let mut state = vec![0u8; n]; // 0=unvisited, 1=visiting, 2=done
        let mut path: Vec<usize> = Vec::new();

        fn dfs(
            idx: usize,
            steps: &[Step],
            index: &HashMap<String, usize>,
            state: &mut [u8],
            path: &mut Vec<usize>,
        ) -> Option<Vec<String>> {
            state[idx] = 1;
            path.push(idx);
            for dep in &steps[idx].depends_on {
                let dep_idx = *index.get(dep)?;
                if state[dep_idx] == 1 {
                    // Found cycle: extract from first occurrence of dep_idx.
                    let start = path.iter().position(|&p| p == dep_idx).unwrap_or(0);
                    let cycle = path[start..]
                        .iter()
                        .map(|&p| steps[p].name.clone())
                        .collect::<Vec<_>>();
                    let mut full = cycle.clone();
                    full.push(steps[dep_idx].name.clone());
                    return Some(full);
                }
                if state[dep_idx] == 0 {
                    if let Some(c) = dfs(dep_idx, steps, index, state, path) {
                        return Some(c);
                    }
                }
            }
            path.pop();
            state[idx] = 2;
            None
        }

        for i in 0..n {
            if state[i] == 0 {
                if let Some(c) = dfs(i, &self.steps, &index, &mut state, &mut path) {
                    return Some(c);
                }
            }
        }
        None
    }

    /// Return the names of steps that have all dependencies satisfied by
    /// `completed`. This is the executor's scheduling frontier.
    pub fn ready_steps(&self, completed: &HashSet<String>) -> Vec<String> {
        self.steps
            .iter()
            .filter(|s| {
                !completed.contains(&s.name) && s.depends_on.iter().all(|d| completed.contains(d))
            })
            .map(|s| s.name.clone())
            .collect()
    }

    /// Return all dependency names referenced by any step.
    pub fn all_dependencies(&self) -> HashSet<String> {
        self.steps
            .iter()
            .flat_map(|s| s.depends_on.iter().cloned())
            .collect()
    }
}

/// Output of a completed workflow step.
///
/// `run_id` (WO 45.1) is the canonical run id of the session that
/// executed this workflow — `None` for workflows run outside a session
/// (bench, tests). Lets a step's output be attributed back to its run
/// for replay / metrics / audit.
#[derive(Debug, Clone)]
pub struct StepOutput {
    pub name: String,
    pub kind: StepKind,
    pub persona: String,
    pub summary: String,
    pub critique: Option<String>,
    /// Structured output parsed from the summary JSON. Set when the step's
    /// summary is valid JSON; enables `$(step.field)` lookups.
    pub structured_output: Option<serde_json::Value>,
    /// Canonical run id of the executing session (WO 45.1). `None` when
    /// the workflow was run outside a session (bench, tests).
    pub run_id: Option<String>,
}

impl Default for StepOutput {
    fn default() -> Self {
        Self {
            name: String::new(),
            kind: StepKind::Agent,
            persona: String::new(),
            summary: String::new(),
            critique: None,
            structured_output: None,
            run_id: None,
        }
    }
}

/// Trait abstracting how the workflow executor runs steps.
/// The binary crate implements this with `tools::task::InProcessTaskSpawner`.
#[async_trait::async_trait]
pub trait StepRunner: Send + Sync {
    /// Run one agent step prompt under the given persona and return the summary.
    async fn run_step(&self, name: &str, prompt: &str, persona: &str) -> Result<String>;

    /// Run a bash command and return its combined stdout+stderr output.
    async fn run_bash(&self, name: &str, _command: &str) -> Result<String> {
        bail!("bash steps not supported by this runner (step '{name}')")
    }

    /// Run a tool by name with JSON arguments and return the result.
    async fn run_tool(
        &self,
        name: &str,
        _tool_name: &str,
        _arguments: &serde_json::Value,
    ) -> Result<String> {
        bail!("tool steps not supported by this runner (step '{name}')")
    }

    /// Evaluate a step's `condition` shell string. Returns `true` if the
    /// condition is absent or exits 0, `false` otherwise (including timeout
    /// and spawn failure — a hung condition skips the step, not the workflow).
    /// Override to route the condition through the same deny gate the runner
    /// applies to bash steps. The default is `eval_condition_bounded`.
    async fn eval_condition(&self, condition: &str) -> bool {
        eval_condition_bounded(condition).await
    }

    /// Run a batch of independent steps and return their summaries in input order.
    ///
    /// The default implementation runs sequentially, dispatching by step kind.
    async fn run_batch(&self, steps: Vec<StepRequest>) -> Result<Vec<(String, String)>> {
        let mut out = Vec::with_capacity(steps.len());
        for req in steps {
            let summary = match req.kind {
                StepKind::Agent => self.run_step(&req.name, &req.prompt, &req.persona).await?,
                StepKind::Bash => self.run_bash(&req.name, &req.command).await?,
                StepKind::Tool => {
                    self.run_tool(&req.name, &req.tool_name, &req.tool_arguments)
                        .await?
                }
                StepKind::FanOut => {
                    bail!("fan_out steps are expanded before reaching run_batch")
                }
                StepKind::FanIn => {
                    bail!("fan_in steps collect fan-out results, not dispatched individually")
                }
            };
            out.push((req.name, summary));
        }
        Ok(out)
    }
}

/// Input for one step in a batch.
#[derive(Debug, Clone)]
pub struct StepRequest {
    pub name: String,
    pub kind: StepKind,
    pub prompt: String,
    pub persona: String,
    pub command: String,
    pub tool_name: String,
    pub tool_arguments: serde_json::Value,
    pub with_critique: bool,
}

/// Outcome of a `run_batch` that joined ALL handles (never early-returned on
/// the first error). Carries both the succeeded steps' `(name, summary)`
/// pairs and the failed steps' `(name, error)` pairs so the executor can
/// preserve sibling results instead of dropping them. A runner that cannot
/// partition (e.g. the sequential default) returns a plain `anyhow::Error`
/// and the executor falls back to the all-failed path.
#[derive(Debug)]
pub struct BatchErrors {
    /// `(step_name, summary)` for steps that succeeded.
    pub successes: Vec<(String, String)>,
    /// `(step_name, error)` for steps that failed.
    pub failures: Vec<(String, anyhow::Error)>,
}

impl std::fmt::Display for BatchErrors {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "batch failed: {}/{} steps errored",
            self.failures.len(),
            self.successes.len() + self.failures.len()
        )
    }
}

impl std::error::Error for BatchErrors {}

impl BatchErrors {
    /// True if at least one step failed.
    pub fn has_failures(&self) -> bool {
        !self.failures.is_empty()
    }
}

/// Resolve `$(step_name)` and `$(step_name.field.path)` references in a string
/// using completed step outputs. `$(step_name)` is replaced with the step's
/// summary. `$(step_name.field)` extracts `field` (and nested paths via `.`)
/// from the step's structured output; falls back to the summary if no
/// structured output is available. Unknown step names are left as-is.
///
/// The `$(...)` reference syntax uses ASCII `$`, `(`, `)` so byte-level
/// matching is correct for references. Surrounding text is emitted
/// character-by-character so multi-byte UTF-8 is preserved.
pub fn resolve_step_refs(text: &str, outputs: &HashMap<String, StepOutput>) -> String {
    let mut result = String::with_capacity(text.len());
    let bytes = text.as_bytes();
    let mut char_indices = text.char_indices().peekable();
    while let Some((byte_pos, ch)) = char_indices.next() {
        if ch == '$' && char_indices.peek().is_some_and(|(_, c)| *c == '(') {
            // Find the closing ')' using byte offsets (all reference
            // chars are ASCII, so byte scanning is correct here).
            let start = byte_pos + 2; // skip past "$("
            let mut depth = 1;
            let mut end = start;
            while end < bytes.len() && depth > 0 {
                if bytes[end] == b'(' {
                    depth += 1;
                } else if bytes[end] == b')' {
                    depth -= 1;
                }
                if depth > 0 {
                    end += 1;
                }
            }
            if depth != 0 {
                // Unmatched paren — leave as-is.
                result.push('$');
                continue;
            }
            let content = std::str::from_utf8(&bytes[start..end]).unwrap_or("");
            // Split into step_name.field.path
            let mut parts = content.splitn(2, '.');
            let step_name = parts.next().unwrap_or("");
            let field_path = parts.next();
            if let Some(output) = outputs.get(step_name) {
                if let Some(path) = field_path {
                    if let Some(ref val) = output.structured_output {
                        result.push_str(&extract_field(val, path));
                    } else {
                        result.push_str(&output.summary);
                    }
                } else {
                    result.push_str(&output.summary);
                }
            } else {
                // Unknown step — leave as-is.
                result.push_str(&format!("$({content})"));
            }
            // Advance the character iterator past the reference.
            while let Some((pos, _)) = char_indices.peek() {
                if *pos <= end {
                    char_indices.next();
                } else {
                    break;
                }
            }
        } else {
            result.push(ch);
        }
    }
    result
}

/// Extract a dotted field path from a JSON value, returning the string
/// representation. Strings and numbers are unwrapped; everything else is
/// JSON-serialized. Numeric path components index into arrays.
fn extract_field(val: &serde_json::Value, path: &str) -> String {
    let mut current = val;
    for field in path.split('.') {
        // Try string key first (for objects), then numeric index (for arrays).
        let next = current
            .get(field)
            .or_else(|| field.parse::<usize>().ok().and_then(|i| current.get(i)));
        match next {
            Some(v) => current = v,
            None => return format!("$({path} not found)"),
        }
    }
    match current {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Number(n) => n.to_string(),
        serde_json::Value::Bool(b) => b.to_string(),
        serde_json::Value::Null => "null".to_string(),
        other => other.to_string(),
    }
}

/// Wall-clock bound for condition evaluation. Mirrors the workflow bash
/// step timeout (`WORKFLOW_BASH_TIMEOUT_SECS` in `src/tools/workflow.rs`):
/// a condition should be a quick test/grep, not a long-running command.
/// `ceiling:` a template that needs a longer condition should surface a
/// per-step timeout knob; upgrade path is a `step.timeout_secs` field.
#[cfg(not(test))]
const CONDITION_TIMEOUT_SECS: u64 = 30;
/// Under cfg(test) the bound is 2s so the `sleep infinity` pinning test
/// resolves fast instead of hanging the suite for 30s.
#[cfg(test)]
const CONDITION_TIMEOUT_SECS: u64 = 2;

/// Evaluate a shell condition string with a wall-clock bound and
/// `kill_on_drop`. Returns `true` if the condition exits 0, `false`
/// otherwise — including timeout and spawn failure, so a hung condition
/// skips the step instead of wedging the workflow.
/// ponytail: sh -c eval — upgrade to expression parser if needed.
pub async fn eval_condition_bounded(condition: &str) -> bool {
    let mut cmd = tokio::process::Command::new("sh");
    cmd.arg("-c")
        .arg(condition)
        .kill_on_drop(true)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    let status_fut = cmd.status();
    let timeout_fut = tokio::time::sleep(std::time::Duration::from_secs(CONDITION_TIMEOUT_SECS));
    tokio::pin!(status_fut);
    tokio::pin!(timeout_fut);
    tokio::select! {
        biased;
        result = &mut status_fut => match result {
            Ok(status) => status.success(),
            Err(e) => {
                tracing::warn!("condition eval failed for '{condition}': {e}");
                false
            }
        },
        _ = &mut timeout_fut => {
            tracing::warn!(
                "condition '{condition}' timed out after {CONDITION_TIMEOUT_SECS}s — skipping step"
            );
            false
        }
    }
    // On timeout the `cmd` future (still owned here) is dropped; `kill_on_drop`
    // reaps the spawned `sh` process so a `sleep infinity` condition cannot
    // outlive the bound.
}

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
                    run_id: None,
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
                    run_id: None,
                };
                completed.insert(on_error.clone());
                let error_output = StepOutput {
                    name: on_error.clone(),
                    kind: StepKind::Agent,
                    persona: String::new(),
                    summary: format!("error handler triggered by: {e}"),
                    critique: None,
                    structured_output: None,
                    run_id: None,
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
            run_id: None,
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
            run_id: None,
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
                    run_id: None,
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
                    run_id: None,
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
                    run_id: None,
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
                    run_id: None,
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
                    run_id: None,
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
                        run_id: None,
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
                        run_id: None,
                    },
                );
            }
        }

        Ok(WorkflowSummary {
            workflow_name: self.workflow.name.clone(),
            outputs,
            budget_exceeded: false,
            run_id: None,
        })
    }
}

/// Completed workflow summary.
#[derive(Debug, Clone, Default)]
pub struct WorkflowSummary {
    pub workflow_name: String,
    pub outputs: HashMap<String, StepOutput>,
    /// `true` if a budget limit (max_iterations / max_seconds) was hit. When
    /// `budget.on_exceeded` is configured, the handler step's output is in
    /// `outputs` and the workflow returns `Ok` with this flag set instead of
    /// bailing — so the configured handler output reaches the model.
    pub budget_exceeded: bool,
    /// Canonical run id of the session that executed this workflow (WO
    /// 45.1). `None` when the workflow ran outside a session (bench,
    /// tests, scheduled jobs with no owning session). Each step's
    /// `StepOutput.run_id` carries the same value.
    pub run_id: Option<String>,
}

impl WorkflowSummary {
    pub fn step(&self, name: &str) -> Option<&StepOutput> {
        self.outputs.get(name)
    }

    pub fn ordered_outputs(&self, order: &[String]) -> Vec<&StepOutput> {
        order.iter().filter_map(|n| self.outputs.get(n)).collect()
    }
}

/// Find a workflow file by name in the standard search paths.
///
/// Searches:
/// 1. `.kf-code/workflows/<name>.json` in the current directory.
/// 2. `~/.local/share/kf-code/workflows/<name>.json`.
pub fn find_workflow_file(name: &str) -> Option<PathBuf> {
    let local = PathBuf::from(".kf-code/workflows").join(format!("{name}.json"));
    if local.exists() {
        return Some(local);
    }
    if let Some(data_dir) = directories::BaseDirs::new() {
        let shared = data_dir
            .data_local_dir()
            .join("kf-code/workflows")
            .join(format!("{name}.json"));
        if shared.exists() {
            return Some(shared);
        }
    }
    None
}

/// Return the path to the user share directory for workflows.
pub fn user_workflow_dir() -> PathBuf {
    directories::BaseDirs::new()
        .map(|b| b.data_local_dir().join("kf-code/workflows"))
        .unwrap_or_else(|| PathBuf::from(".kf-code/workflows"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;
    use std::sync::{Arc, Mutex};

    struct MockRunner {
        log: Arc<Mutex<Vec<(String, String, String)>>>,
    }

    #[async_trait::async_trait]
    impl StepRunner for MockRunner {
        async fn run_step(&self, name: &str, prompt: &str, persona: &str) -> Result<String> {
            self.log.lock().unwrap().push((
                name.to_string(),
                persona.to_string(),
                prompt.to_string(),
            ));
            Ok(format!("{persona}:{name}:done"))
        }
    }

    #[allow(clippy::type_complexity)]
    fn make_runner() -> (MockRunner, Arc<Mutex<Vec<(String, String, String)>>>) {
        let log = Arc::new(Mutex::new(Vec::new()));
        (MockRunner { log: log.clone() }, log)
    }

    #[test]
    fn parses_simple_workflow() {
        let json = br#"{"name":"test","steps":[{"name":"a","prompt":"do a","persona":"explore"}]}"#;
        let wf = Workflow::from_json(json).unwrap();
        assert_eq!(wf.name, "test");
        assert_eq!(wf.steps.len(), 1);
    }

    #[test]
    fn rejects_unknown_persona() {
        let json = br#"{"name":"bad","steps":[{"name":"a","prompt":"x","persona":"write"}]}"#;
        let err = Workflow::from_json(json).unwrap_err().to_string();
        assert!(err.contains("unknown persona"));
    }

    #[test]
    fn detects_self_dependency() {
        let json = br#"{"name":"bad","steps":[{"name":"a","prompt":"x","persona":"explore","depends_on":["a"]}]}"#;
        let err = Workflow::from_json(json).unwrap_err().to_string();
        assert!(err.contains("depends on itself"));
    }

    #[test]
    fn detects_unknown_dependency() {
        let json = br#"{"name":"bad","steps":[{"name":"a","prompt":"x","persona":"explore","depends_on":["b"]}]}"#;
        let err = Workflow::from_json(json).unwrap_err().to_string();
        assert!(err.contains("depends on unknown step"));
    }

    #[test]
    fn detects_cycle() {
        let wf = Workflow {
            name: "cycle".into(),
            steps: vec![
                Step {
                    name: "a".into(),
                    kind: StepKind::Agent,
                    prompt: Some("x".into()),
                    persona: Some("explore".into()),
                    command: None,
                    tool_name: None,
                    tool_arguments: None,
                    depends_on: vec!["b".into()],
                    critique: None,
                    condition: None,
                    on_error: None,
                    fork_from: None,
                    over: None,
                    as_name: None,
                    max_parallel: None,
                },
                Step {
                    name: "b".into(),
                    kind: StepKind::Agent,
                    prompt: Some("x".into()),
                    persona: Some("plan".into()),
                    command: None,
                    tool_name: None,
                    tool_arguments: None,
                    depends_on: vec!["a".into()],
                    critique: None,
                    condition: None,
                    on_error: None,
                    fork_from: None,
                    over: None,
                    as_name: None,
                    max_parallel: None,
                },
            ],
            budget: None,
        };
        let err = wf.validate().unwrap_err().to_string();
        assert!(err.contains("cycle"));
    }

    #[tokio::test]
    async fn propagates_dependency_outputs() {
        let wf = Workflow {
            name: "prop".into(),
            steps: vec![
                Step {
                    name: "explore".into(),
                    kind: StepKind::Agent,
                    prompt: Some("Map X".into()),
                    persona: Some("explore".into()),
                    command: None,
                    tool_name: None,
                    tool_arguments: None,
                    depends_on: vec![],
                    critique: None,
                    condition: None,
                    on_error: None,
                    fork_from: None,
                    over: None,
                    as_name: None,
                    max_parallel: None,
                },
                Step {
                    name: "plan".into(),
                    kind: StepKind::Agent,
                    prompt: Some("Design X".into()),
                    persona: Some("plan".into()),
                    command: None,
                    tool_name: None,
                    tool_arguments: None,
                    depends_on: vec!["explore".into()],
                    critique: None,
                    condition: None,
                    on_error: None,
                    fork_from: None,
                    over: None,
                    as_name: None,
                    max_parallel: None,
                },
            ],
            budget: None,
        };
        let (runner, log) = make_runner();
        let exe = WorkflowExecutor::new(wf);
        let summary = exe.run(Arc::new(runner), None).await.unwrap();
        assert_eq!(summary.step("plan").unwrap().summary, "plan:plan:done");
        let plan_prompt = &log.lock().unwrap()[1].2;
        assert!(plan_prompt.contains("Context from previous steps"));
        assert!(plan_prompt.contains("explore:explore:done"));
    }

    #[tokio::test]
    async fn independent_steps_run_in_batch() {
        let wf = Workflow {
            name: "parallel".into(),
            steps: vec![
                Step {
                    name: "a".into(),
                    kind: StepKind::Agent,
                    prompt: Some("a".into()),
                    persona: Some("explore".into()),
                    command: None,
                    tool_name: None,
                    tool_arguments: None,
                    depends_on: vec![],
                    critique: None,
                    condition: None,
                    on_error: None,
                    fork_from: None,
                    over: None,
                    as_name: None,
                    max_parallel: None,
                },
                Step {
                    name: "b".into(),
                    kind: StepKind::Agent,
                    prompt: Some("b".into()),
                    persona: Some("explore".into()),
                    command: None,
                    tool_name: None,
                    tool_arguments: None,
                    depends_on: vec![],
                    critique: None,
                    condition: None,
                    on_error: None,
                    fork_from: None,
                    over: None,
                    as_name: None,
                    max_parallel: None,
                },
                Step {
                    name: "c".into(),
                    kind: StepKind::Agent,
                    prompt: Some("c".into()),
                    persona: Some("coder".into()),
                    command: None,
                    tool_name: None,
                    tool_arguments: None,
                    depends_on: vec!["a".into(), "b".into()],
                    critique: None,
                    condition: None,
                    on_error: None,
                    fork_from: None,
                    over: None,
                    as_name: None,
                    max_parallel: None,
                },
            ],
            budget: None,
        };
        let (runner, log) = make_runner();
        let exe = WorkflowExecutor::new(wf);
        let summary = exe.run(Arc::new(runner), None).await.unwrap();
        assert_eq!(summary.outputs.len(), 3);
        let calls = log.lock().unwrap();
        // a and b are in the first batch; order within batch is insertion order.
        assert_eq!(calls[0].0, "a");
        assert_eq!(calls[1].0, "b");
        assert_eq!(calls[2].0, "c");
        assert!(calls[2].2.contains("a"));
        assert!(calls[2].2.contains("b"));
    }

    #[tokio::test]
    async fn cancellation_stops_executor() {
        let wf = Workflow {
            name: "cancel".into(),
            steps: vec![
                Step {
                    name: "a".into(),
                    kind: StepKind::Agent,
                    prompt: Some("x".into()),
                    persona: Some("explore".into()),
                    command: None,
                    tool_name: None,
                    tool_arguments: None,
                    depends_on: vec![],
                    critique: None,
                    condition: None,
                    on_error: None,
                    fork_from: None,
                    over: None,
                    as_name: None,
                    max_parallel: None,
                },
                Step {
                    name: "b".into(),
                    kind: StepKind::Agent,
                    prompt: Some("y".into()),
                    persona: Some("plan".into()),
                    command: None,
                    tool_name: None,
                    tool_arguments: None,
                    depends_on: vec!["a".into()],
                    critique: None,
                    condition: None,
                    on_error: None,
                    fork_from: None,
                    over: None,
                    as_name: None,
                    max_parallel: None,
                },
            ],
            budget: None,
        };
        let (runner, _log) = make_runner();
        let exe = WorkflowExecutor::new(wf);
        let cancel = AtomicBool::new(true);
        let err = exe
            .run(Arc::new(runner), Some(&cancel))
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("cancelled"));
    }

    #[tokio::test]
    async fn critique_spawns_extra_plan_step() {
        let wf = Workflow {
            name: "crit".into(),
            steps: vec![Step {
                name: "plan".into(),
                kind: StepKind::Agent,
                prompt: Some("Design X".into()),
                persona: Some("plan".into()),
                command: None,
                tool_name: None,
                tool_arguments: None,
                depends_on: vec![],
                critique: Some(true),
                condition: None,
                on_error: None,
                fork_from: None,
                over: None,
                as_name: None,
                max_parallel: None,
            }],
            budget: None,
        };
        let (runner, log) = make_runner();
        let exe = WorkflowExecutor::new(wf);
        let summary = exe.run(Arc::new(runner), None).await.unwrap();
        assert!(summary.step("plan").unwrap().critique.is_some());
        let calls = log.lock().unwrap();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[1].0, "plan-critique");
        assert_eq!(calls[1].1, "plan");
    }

    /// A runner that sleeps for a fixed duration per step and records
    /// per-step start/end times. Its `run_batch` spawns each step concurrently.
    struct SleepingBatchRunner {
        sleep_ms: u64,
        starts: Arc<Mutex<Vec<(String, std::time::Instant)>>>,
        ends: Arc<Mutex<Vec<(String, std::time::Instant)>>>,
    }

    #[async_trait::async_trait]
    impl StepRunner for SleepingBatchRunner {
        async fn run_step(&self, name: &str, _prompt: &str, _persona: &str) -> Result<String> {
            let start = std::time::Instant::now();
            self.starts.lock().unwrap().push((name.to_string(), start));
            tokio::time::sleep(tokio::time::Duration::from_millis(self.sleep_ms)).await;
            let end = std::time::Instant::now();
            self.ends.lock().unwrap().push((name.to_string(), end));
            Ok(format!("{name}:done"))
        }

        async fn run_batch(&self, steps: Vec<StepRequest>) -> Result<Vec<(String, String)>> {
            let mut handles = Vec::with_capacity(steps.len());
            for req in steps {
                let this = self.clone();
                handles.push(tokio::spawn(async move {
                    let summary = this.run_step(&req.name, &req.prompt, &req.persona).await?;
                    Ok::<(String, String), anyhow::Error>((req.name, summary))
                }));
            }
            let mut out = Vec::with_capacity(handles.len());
            for h in handles {
                out.push(h.await.map_err(|e| anyhow!("batch task panicked: {e}"))??);
            }
            Ok(out)
        }
    }

    impl Clone for SleepingBatchRunner {
        fn clone(&self) -> Self {
            Self {
                sleep_ms: self.sleep_ms,
                starts: self.starts.clone(),
                ends: self.ends.clone(),
            }
        }
    }

    /// A runner whose `run_batch` drops the second result (partial batch).
    struct PartialBatchRunner;

    #[async_trait::async_trait]
    impl StepRunner for PartialBatchRunner {
        async fn run_step(&self, name: &str, _prompt: &str, persona: &str) -> Result<String> {
            Ok(format!("{persona}:{name}:done"))
        }

        async fn run_batch(&self, steps: Vec<StepRequest>) -> Result<Vec<(String, String)>> {
            // Return only the first step's result, dropping the rest.
            Ok(steps
                .into_iter()
                .take(1)
                .map(|req| (req.name, "partial".to_string()))
                .collect())
        }
    }

    #[tokio::test]
    async fn partial_batch_result_is_detected() {
        let wf = Workflow {
            name: "partial".into(),
            steps: vec![
                Step {
                    name: "a".into(),
                    kind: StepKind::Agent,
                    prompt: Some("a".into()),
                    persona: Some("explore".into()),
                    command: None,
                    tool_name: None,
                    tool_arguments: None,
                    depends_on: vec![],
                    critique: None,
                    condition: None,
                    on_error: None,
                    fork_from: None,
                    over: None,
                    as_name: None,
                    max_parallel: None,
                },
                Step {
                    name: "b".into(),
                    kind: StepKind::Agent,
                    prompt: Some("b".into()),
                    persona: Some("explore".into()),
                    command: None,
                    tool_name: None,
                    tool_arguments: None,
                    depends_on: vec![],
                    critique: None,
                    condition: None,
                    on_error: None,
                    fork_from: None,
                    over: None,
                    as_name: None,
                    max_parallel: None,
                },
            ],
            budget: None,
        };
        let exe = WorkflowExecutor::new(wf);
        let err = exe
            .run(Arc::new(PartialBatchRunner), None)
            .await
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("no result for step"),
            "expected partial-batch error, got: {err}"
        );
    }

    #[tokio::test]
    async fn independent_steps_run_concurrently() {
        let wf = Workflow {
            name: "parallel".into(),
            steps: vec![
                Step {
                    name: "a".into(),
                    kind: StepKind::Agent,
                    prompt: Some("a".into()),
                    persona: Some("explore".into()),
                    command: None,
                    tool_name: None,
                    tool_arguments: None,
                    depends_on: vec![],
                    critique: None,
                    condition: None,
                    on_error: None,
                    fork_from: None,
                    over: None,
                    as_name: None,
                    max_parallel: None,
                },
                Step {
                    name: "b".into(),
                    kind: StepKind::Agent,
                    prompt: Some("b".into()),
                    persona: Some("explore".into()),
                    command: None,
                    tool_name: None,
                    tool_arguments: None,
                    depends_on: vec![],
                    critique: None,
                    condition: None,
                    on_error: None,
                    fork_from: None,
                    over: None,
                    as_name: None,
                    max_parallel: None,
                },
                Step {
                    name: "c".into(),
                    kind: StepKind::Agent,
                    prompt: Some("c".into()),
                    persona: Some("coder".into()),
                    command: None,
                    tool_name: None,
                    tool_arguments: None,
                    depends_on: vec!["a".into(), "b".into()],
                    critique: None,
                    condition: None,
                    on_error: None,
                    fork_from: None,
                    over: None,
                    as_name: None,
                    max_parallel: None,
                },
            ],
            budget: None,
        };
        let runner = SleepingBatchRunner {
            sleep_ms: 1000,
            starts: Arc::new(Mutex::new(Vec::new())),
            ends: Arc::new(Mutex::new(Vec::new())),
        };
        let starts = runner.starts.clone();
        let ends = runner.ends.clone();
        let exe = WorkflowExecutor::new(wf);
        let start = std::time::Instant::now();
        let summary = exe.run(Arc::new(runner), None).await.unwrap();
        let elapsed = start.elapsed().as_secs_f64();

        assert_eq!(summary.outputs.len(), 3);
        // a and b should start in the first batch and overlap; c waits.
        let first_starts: Vec<_> = starts
            .lock()
            .unwrap()
            .iter()
            .filter(|(n, _)| n == "a" || n == "b")
            .map(|(_, t)| *t)
            .collect();
        let first_ends: Vec<_> = ends
            .lock()
            .unwrap()
            .iter()
            .filter(|(n, _)| n == "a" || n == "b")
            .map(|(_, t)| *t)
            .collect();
        let latest_start = *first_starts.iter().max().unwrap();
        let earliest_end = *first_ends.iter().min().unwrap();
        let overlap = earliest_end.duration_since(latest_start).as_secs_f64();
        assert!(
            overlap >= 0.5,
            "a and b should overlap by at least 0.5s; got {overlap:.2}s"
        );
        assert!(
            elapsed < 3.5,
            "three 1s steps (two parallel + one dependent) should finish in ~2s; got {elapsed:.2}s"
        );
    }

    #[test]
    fn parses_agent_step_backward_compat() {
        // Old JSON format with prompt+persona still works.
        let json = br#"{"name":"test","steps":[{"name":"a","prompt":"do a","persona":"explore"}]}"#;
        let wf = Workflow::from_json(json).unwrap();
        assert_eq!(wf.steps[0].kind, StepKind::Agent);
        assert_eq!(wf.steps[0].prompt.as_deref(), Some("do a"));
        assert_eq!(wf.steps[0].persona.as_deref(), Some("explore"));
    }

    #[test]
    fn parses_bash_step() {
        let json =
            br#"{"name":"test","steps":[{"name":"run","kind":"bash","command":"cargo test"}]}"#;
        let wf = Workflow::from_json(json).unwrap();
        assert_eq!(wf.steps[0].kind, StepKind::Bash);
        assert_eq!(wf.steps[0].command.as_deref(), Some("cargo test"));
    }

    #[test]
    fn parses_tool_step() {
        let json = br#"{"name":"test","steps":[{"name":"grep","kind":"tool","tool_name":"grep","tool_arguments":{"pattern":"TODO"}}]}"#;
        let wf = Workflow::from_json(json).unwrap();
        assert_eq!(wf.steps[0].kind, StepKind::Tool);
        assert_eq!(wf.steps[0].tool_name.as_deref(), Some("grep"));
    }

    #[test]
    fn rejects_agent_step_without_prompt() {
        let json = br#"{"name":"bad","steps":[{"name":"a","persona":"explore"}]}"#;
        let err = Workflow::from_json(json).unwrap_err().to_string();
        assert!(err.contains("no prompt"), "got: {err}");
    }

    #[test]
    fn rejects_bash_step_without_command() {
        let json = br#"{"name":"bad","steps":[{"name":"a","kind":"bash"}]}"#;
        let err = Workflow::from_json(json).unwrap_err().to_string();
        assert!(err.contains("no command"), "got: {err}");
    }

    #[test]
    fn rejects_tool_step_without_tool_name() {
        let json = br#"{"name":"bad","steps":[{"name":"a","kind":"tool"}]}"#;
        let err = Workflow::from_json(json).unwrap_err().to_string();
        assert!(err.contains("no tool_name"), "got: {err}");
    }

    #[test]
    fn rejects_on_error_referencing_unknown_step() {
        let json = br#"{"name":"bad","steps":[{"name":"a","prompt":"x","persona":"explore","on_error":"nonexistent"}]}"#;
        let err = Workflow::from_json(json).unwrap_err().to_string();
        assert!(
            err.contains("on_error references unknown step"),
            "got: {err}"
        );
    }

    #[test]
    fn accepts_on_error_referencing_known_step() {
        let json = br#"{"name":"ok","steps":[{"name":"a","prompt":"x","persona":"explore","on_error":"handler"},{"name":"handler","prompt":"handle","persona":"plan"}]}"#;
        let wf = Workflow::from_json(json).unwrap();
        assert_eq!(wf.steps[0].on_error.as_deref(), Some("handler"));
    }

    #[test]
    fn rejects_fork_from_referencing_unknown_step() {
        let json = br#"{"name":"bad","steps":[{"name":"a","prompt":"x","persona":"explore","fork_from":"nonexistent"}]}"#;
        let err = Workflow::from_json(json).unwrap_err().to_string();
        assert!(
            err.contains("fork_from references unknown step"),
            "got: {err}"
        );
    }

    #[test]
    fn accepts_fork_from_referencing_known_step() {
        let json = br#"{"name":"ok","steps":[{"name":"a","prompt":"x","persona":"explore"},{"name":"b","prompt":"y","persona":"coder","fork_from":"a","depends_on":["a"]}]}"#;
        let wf = Workflow::from_json(json).unwrap();
        assert_eq!(wf.steps[1].fork_from.as_deref(), Some("a"));
    }

    #[test]
    fn parses_fan_out_step() {
        let json = br#"{"name":"test","steps":[{"name":"fan","kind":"fan_out","over":"$(explore.items)","as_name":"item","prompt":"Process ${item}","persona":"coder","depends_on":["explore"]},{"name":"explore","prompt":"List items","persona":"explore"}]}"#;
        let wf = Workflow::from_json(json).unwrap();
        assert_eq!(wf.steps[0].kind, StepKind::FanOut);
        assert_eq!(wf.steps[0].over.as_deref(), Some("$(explore.items)"));
        assert_eq!(wf.steps[0].as_name.as_deref(), Some("item"));
    }

    #[test]
    fn parses_fan_in_step() {
        let json = br#"{"name":"test","steps":[{"name":"fan","kind":"fan_out","over":"$(x)","as_name":"item","prompt":"p","persona":"coder"},{"name":"collect","kind":"fan_in","depends_on":["fan"]}]}"#;
        let wf = Workflow::from_json(json).unwrap();
        assert_eq!(wf.steps[1].kind, StepKind::FanIn);
    }

    #[test]
    fn rejects_fan_out_without_over() {
        let json = br#"{"name":"bad","steps":[{"name":"fan","kind":"fan_out","as_name":"item"}]}"#;
        let err = Workflow::from_json(json).unwrap_err().to_string();
        assert!(err.contains("no 'over'"), "got: {err}");
    }

    #[test]
    fn rejects_fan_out_without_as_name() {
        let json = br#"{"name":"bad","steps":[{"name":"fan","kind":"fan_out","over":"$(x)"}]}"#;
        let err = Workflow::from_json(json).unwrap_err().to_string();
        assert!(err.contains("no 'as_name'"), "got: {err}");
    }

    #[test]
    fn fan_out_zero_max_parallel_is_rejected() {
        let json = br#"{"name":"bad","steps":[{"name":"fan","kind":"fan_out","over":"$(x)","as_name":"item","max_parallel":0}]}"#;
        let err = Workflow::from_json(json).unwrap_err().to_string();
        assert!(err.contains("max_parallel=0"), "got: {err}");
    }

    #[tokio::test]
    async fn fork_from_prepends_context() {
        let wf = Workflow {
            name: "fork".into(),
            steps: vec![
                Step {
                    name: "source".into(),
                    kind: StepKind::Agent,
                    prompt: Some("Do something".into()),
                    persona: Some("explore".into()),
                    command: None,
                    tool_name: None,
                    tool_arguments: None,
                    depends_on: vec![],
                    critique: None,
                    condition: None,
                    on_error: None,
                    fork_from: None,
                    over: None,
                    as_name: None,
                    max_parallel: None,
                },
                Step {
                    name: "forked".into(),
                    kind: StepKind::Agent,
                    prompt: Some("Continue from source".into()),
                    persona: Some("coder".into()),
                    command: None,
                    tool_name: None,
                    tool_arguments: None,
                    depends_on: vec!["source".into()],
                    critique: None,
                    condition: None,
                    on_error: None,
                    fork_from: Some("source".into()),
                    over: None,
                    as_name: None,
                    max_parallel: None,
                },
            ],
            budget: None,
        };
        let (runner, log) = make_runner();
        let exe = WorkflowExecutor::new(wf);
        let summary = exe.run(Arc::new(runner), None).await.unwrap();
        assert_eq!(summary.outputs.len(), 2);
        let forked_prompt = &log.lock().unwrap()[1].2;
        assert!(
            forked_prompt.contains("Context from forked step 'source'"),
            "expected fork_from context in prompt, got: {forked_prompt}"
        );
    }

    #[tokio::test]
    async fn fan_out_expands_and_runs() {
        let wf = Workflow {
            name: "fanout".into(),
            steps: vec![
                Step {
                    name: "explore".into(),
                    kind: StepKind::Agent,
                    prompt: Some("List items".into()),
                    persona: Some("explore".into()),
                    command: None,
                    tool_name: None,
                    tool_arguments: None,
                    depends_on: vec![],
                    critique: None,
                    condition: None,
                    on_error: None,
                    fork_from: None,
                    over: None,
                    as_name: None,
                    max_parallel: None,
                },
                Step {
                    name: "fan".into(),
                    kind: StepKind::FanOut,
                    prompt: Some("Process ${item}".into()),
                    persona: Some("coder".into()),
                    command: None,
                    tool_name: None,
                    tool_arguments: None,
                    depends_on: vec!["explore".into()],
                    critique: None,
                    condition: None,
                    on_error: None,
                    fork_from: None,
                    over: Some(r#"$(explore)"#.into()),
                    as_name: Some("item".into()),
                    max_parallel: None,
                },
                Step {
                    name: "collect".into(),
                    kind: StepKind::FanIn,
                    prompt: None,
                    persona: None,
                    command: None,
                    tool_name: None,
                    tool_arguments: None,
                    depends_on: vec!["fan".into()],
                    critique: None,
                    condition: None,
                    on_error: None,
                    fork_from: None,
                    over: None,
                    as_name: None,
                    max_parallel: None,
                },
            ],
            budget: None,
        };

        // We need the explore step's output to contain a JSON array so FanOut
        // can parse it. The JsonRunner returns a JSON object, but FanOut
        // needs the *over* expression to resolve to a JSON array. Since
        // explore's summary is a JSON object, FanOut with over="$(explore)"
        // will try to parse it as an array and fail. Instead, use a simpler
        // test: explore returns a raw JSON array.
        struct ArrayRunner;
        #[async_trait::async_trait]
        impl StepRunner for ArrayRunner {
            async fn run_step(&self, name: &str, _prompt: &str, persona: &str) -> Result<String> {
                if name == "explore" {
                    Ok(r#"["alpha","beta","gamma"]"#.to_string())
                } else {
                    Ok(format!("{persona}:{name}:done"))
                }
            }
        }

        let exe = WorkflowExecutor::new(wf);
        let summary = exe.run(Arc::new(ArrayRunner), None).await.unwrap();
        // FanOut step should be completed.
        assert!(summary.outputs.contains_key("explore"));
        assert!(summary.outputs.contains_key("fan"));
        // FanOut creates child steps but stores combined output under "fan".
        let fan_out = summary.outputs.get("fan").unwrap();
        assert_eq!(fan_out.kind, StepKind::FanOut);
        // Should have spawned 3 child steps (alpha, beta, gamma).
        assert!(fan_out.summary.contains("fan_0"));
        assert!(fan_out.summary.contains("fan_1"));
        assert!(fan_out.summary.contains("fan_2"));
        // FanIn should aggregate.
        let fan_in = summary.outputs.get("collect").unwrap();
        assert!(fan_in.summary.contains("fan"));
    }

    /// A runner that records per-step start/end times with atomic timestamps
    /// and sleeps for a fixed duration. Used to verify max_parallel enforcement.
    #[derive(Clone)]
    struct ConcurrencyTrackingRunner {
        sleep_ms: u64,
        starts: Arc<std::sync::atomic::AtomicUsize>,
        active: Arc<std::sync::atomic::AtomicUsize>,
        max_active: Arc<std::sync::atomic::AtomicUsize>,
    }

    #[async_trait::async_trait]
    impl StepRunner for ConcurrencyTrackingRunner {
        async fn run_step(&self, name: &str, _prompt: &str, persona: &str) -> Result<String> {
            let prev = self
                .active
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            // Track the maximum number of concurrently active steps.
            loop {
                let cur = self.max_active.load(std::sync::atomic::Ordering::SeqCst);
                let observed = prev + 1;
                if observed <= cur
                    || self
                        .max_active
                        .compare_exchange(
                            cur,
                            observed,
                            std::sync::atomic::Ordering::SeqCst,
                            std::sync::atomic::Ordering::SeqCst,
                        )
                        .is_ok()
                {
                    break;
                }
            }
            self.starts
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            tokio::time::sleep(tokio::time::Duration::from_millis(self.sleep_ms)).await;
            self.active
                .fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
            Ok(format!("{persona}:{name}:done"))
        }
    }

    #[tokio::test]
    async fn fan_out_respects_max_parallel() {
        // 4 items with max_parallel=2 should never have more than 2 concurrent tasks.
        let wf = Workflow {
            name: "maxpar".into(),
            steps: vec![
                Step {
                    name: "explore".into(),
                    kind: StepKind::Agent,
                    prompt: Some("List items".into()),
                    persona: Some("explore".into()),
                    command: None,
                    tool_name: None,
                    tool_arguments: None,
                    depends_on: vec![],
                    critique: None,
                    condition: None,
                    on_error: None,
                    fork_from: None,
                    over: None,
                    as_name: None,
                    max_parallel: None,
                },
                Step {
                    name: "fan".into(),
                    kind: StepKind::FanOut,
                    prompt: Some("Process ${item}".into()),
                    persona: Some("coder".into()),
                    command: None,
                    tool_name: None,
                    tool_arguments: None,
                    depends_on: vec!["explore".into()],
                    critique: None,
                    condition: None,
                    on_error: None,
                    fork_from: None,
                    over: Some(r#"$(explore)"#.into()),
                    as_name: Some("item".into()),
                    max_parallel: Some(2),
                },
            ],
            budget: None,
        };

        let runner = ConcurrencyTrackingRunner {
            sleep_ms: 200,
            starts: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            active: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            max_active: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        };
        let max_active = runner.max_active.clone();
        let exe = WorkflowExecutor::new(wf);

        // Need a runner that returns a JSON array for the explore step.
        struct ArrayRunner2(ConcurrencyTrackingRunner);
        #[async_trait::async_trait]
        impl StepRunner for ArrayRunner2 {
            async fn run_step(&self, name: &str, prompt: &str, persona: &str) -> Result<String> {
                if name == "explore" {
                    Ok(r#"["a","b","c","d"]"#.to_string())
                } else {
                    self.0.run_step(name, prompt, persona).await
                }
            }
        }
        let tracking = runner;
        let arr_runner = ArrayRunner2(tracking);
        let summary = exe.run(Arc::new(arr_runner), None).await.unwrap();

        // Fan-out should have completed.
        assert!(summary.outputs.contains_key("fan"));
        // max_active should never exceed max_parallel (2).
        let observed_max = max_active.load(std::sync::atomic::Ordering::SeqCst);
        assert!(
            observed_max <= 2,
            "max_parallel=2 but observed {observed_max} concurrent tasks"
        );
        // With 4 items and max_parallel=2, we should have at least 2 concurrent at some point.
        assert!(
            observed_max >= 2,
            "expected at least 2 concurrent tasks, got {observed_max}"
        );
    }

    #[test]
    fn resolve_step_refs_replaces_whole_summary() {
        let mut outputs = HashMap::new();
        outputs.insert(
            "explore".into(),
            StepOutput {
                name: "explore".into(),
                kind: StepKind::Agent,
                persona: "explore".into(),
                summary: "found 3 items".into(),
                critique: None,
                structured_output: None,
                run_id: None,
            },
        );
        let result = resolve_step_refs("Result: $(explore)", &outputs);
        assert_eq!(result, "Result: found 3 items");
    }

    #[test]
    fn resolve_step_refs_extracts_field() {
        let mut outputs = HashMap::new();
        outputs.insert(
            "explore".into(),
            StepOutput {
                name: "explore".into(),
                kind: StepKind::Agent,
                persona: "explore".into(),
                summary: r#"{"count": 5, "items": ["a","b"]}"#.into(),
                critique: None,
                structured_output: Some(serde_json::json!({"count": 5, "items": ["a", "b"]})),
                run_id: None,
            },
        );
        let result = resolve_step_refs("Count: $(explore.count)", &outputs);
        assert_eq!(result, "Count: 5");
        // Nested field.
        let result2 = resolve_step_refs("First: $(explore.items.0)", &outputs);
        assert_eq!(result2, "First: a");
    }

    #[test]
    fn resolve_step_refs_falls_back_to_summary_without_structured() {
        let mut outputs = HashMap::new();
        outputs.insert(
            "explore".into(),
            StepOutput {
                name: "explore".into(),
                kind: StepKind::Agent,
                persona: "explore".into(),
                summary: "plain text summary".into(),
                critique: None,
                structured_output: None,
                run_id: None,
            },
        );
        let result = resolve_step_refs("$(explore.count)", &outputs);
        // Without structured_output, field access falls back to the summary.
        assert_eq!(result, "plain text summary");
    }

    #[test]
    fn resolve_step_refs_leaves_unknown_step_as_is() {
        let outputs = HashMap::new();
        let result = resolve_step_refs("$(unknown)", &outputs);
        assert_eq!(result, "$(unknown)");
    }

    #[test]
    fn resolve_step_refs_nested_path() {
        let mut outputs = HashMap::new();
        outputs.insert(
            "step1".into(),
            StepOutput {
                name: "step1".into(),
                kind: StepKind::Agent,
                persona: "explore".into(),
                summary: "{}".into(),
                critique: None,
                structured_output: Some(serde_json::json!({
                    "a": {"b": {"c": "deep"}}
                })),
                run_id: None,
            },
        );
        let result = resolve_step_refs("$(step1.a.b.c)", &outputs);
        assert_eq!(result, "deep");
    }

    // WO 44.45 defect 1: a hung condition (sleep infinity) must resolve to
    // `false` within the bound (2s under cfg(test)), not wedge the workflow.
    // Bounded by an outer 5s wall: if the timeout path regresses, the test
    // fails instead of hanging indefinitely. unix-only mirrors the bash
    // step timeout test (same kill_on_drop + sh tree concern on Windows).
    #[cfg(unix)]
    #[tokio::test]
    async fn eval_condition_bounded_hung_condition_resolves_false() {
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            eval_condition_bounded("sleep infinity"),
        )
        .await;
        match result {
            Ok(false) => {}
            Ok(true) => panic!("hung condition must evaluate false, not true"),
            Err(_) => panic!("eval_condition_bounded hung past 5s — bound not honoured"),
        }
    }

    // WO 44.45 defect 1: a passing condition exits 0 and returns true.
    #[tokio::test]
    async fn eval_condition_bounded_passing_condition_is_true() {
        assert!(eval_condition_bounded("true").await);
    }

    // WO 44.45 defect 1: a failing condition (exit non-zero) returns false.
    #[tokio::test]
    async fn eval_condition_bounded_failing_condition_is_false() {
        assert!(!eval_condition_bounded("false").await);
    }

    // WO 44.45 defect 2: a budget.on_exceeded handler must surface in the
    // summary, not be dropped by a bail. Previously check_budget inserted the
    // synthetic on_exceeded StepOutput then bailed, so WorkflowSummary (the
    // only carrier of outputs) was dropped and the caller saw only
    // "workflow budget exceeded". Now run() returns Ok with budget_exceeded
    // =true and the handler output in `outputs`.
    //
    // `handler` depends on `work` so it is NOT ready in batch 1 — the budget
    // hits on the batch-2 check (iterations=1 >= max_iterations=1) before
    // handler runs, and check_budget inserts the synthetic handler output.
    #[tokio::test]
    async fn budget_on_exceeded_surfaces_handler_output() {
        let wf = Workflow {
            name: "budget".into(),
            steps: vec![
                Step {
                    name: "work".into(),
                    kind: StepKind::Agent,
                    prompt: Some("do work".into()),
                    persona: Some("explore".into()),
                    command: None,
                    tool_name: None,
                    tool_arguments: None,
                    depends_on: vec![],
                    critique: None,
                    condition: None,
                    on_error: None,
                    fork_from: None,
                    over: None,
                    as_name: None,
                    max_parallel: None,
                },
                Step {
                    name: "handler".into(),
                    kind: StepKind::Agent,
                    prompt: Some("handle budget exceed".into()),
                    persona: Some("plan".into()),
                    command: None,
                    tool_name: None,
                    tool_arguments: None,
                    depends_on: vec!["work".into()],
                    critique: None,
                    condition: None,
                    on_error: None,
                    fork_from: None,
                    over: None,
                    as_name: None,
                    max_parallel: None,
                },
            ],
            budget: Some(Budget {
                max_tokens: None,
                max_seconds: None,
                max_iterations: Some(1),
                on_exceeded: Some("handler".into()),
            }),
        };
        let (runner, _log) = make_runner();
        let exe = WorkflowExecutor::new(wf);
        // Must be Ok, not Err — the handler output must reach the model.
        let summary = exe.run(Arc::new(runner), None).await.unwrap();
        assert!(
            summary.budget_exceeded,
            "budget_exceeded must be true when max_iterations is hit"
        );
        // The on_exceeded handler's synthetic output must be present.
        let handler = summary
            .step("handler")
            .expect("on_exceeded handler output must be in summary");
        assert!(
            handler.summary.contains("budget exceeded: max_iterations"),
            "handler summary must name the budget reason, got: {}",
            handler.summary
        );
        // The first step's output is still present — budget exceed does not
        // erase already-completed work.
        assert!(summary.step("work").is_some());
    }

    // WO 44.45 defect 2: a budget with NO on_exceeded still returns Ok with
    // budget_exceeded=true (the workflow just stops, no handler output).
    // Two chained steps so the workflow has remaining work when budget hits.
    #[tokio::test]
    async fn budget_exceeded_without_handler_returns_ok_flag_set() {
        let wf = Workflow {
            name: "budget_no_handler".into(),
            steps: vec![
                Step {
                    name: "a".into(),
                    kind: StepKind::Agent,
                    prompt: Some("do a".into()),
                    persona: Some("explore".into()),
                    command: None,
                    tool_name: None,
                    tool_arguments: None,
                    depends_on: vec![],
                    critique: None,
                    condition: None,
                    on_error: None,
                    fork_from: None,
                    over: None,
                    as_name: None,
                    max_parallel: None,
                },
                Step {
                    name: "b".into(),
                    kind: StepKind::Agent,
                    prompt: Some("do b".into()),
                    persona: Some("plan".into()),
                    command: None,
                    tool_name: None,
                    tool_arguments: None,
                    depends_on: vec!["a".into()],
                    critique: None,
                    condition: None,
                    on_error: None,
                    fork_from: None,
                    over: None,
                    as_name: None,
                    max_parallel: None,
                },
            ],
            budget: Some(Budget {
                max_tokens: None,
                max_seconds: None,
                max_iterations: Some(1),
                on_exceeded: None,
            }),
        };
        let (runner, _log) = make_runner();
        let exe = WorkflowExecutor::new(wf);
        let summary = exe.run(Arc::new(runner), None).await.unwrap();
        assert!(summary.budget_exceeded);
    }

    // WO 44.45 defect 3: a batch of [ok, fail, ok] must preserve BOTH ok
    // siblings' real outputs and mark only the failed step — not mark every
    // task in the batch failed (the old handle_batch_error behavior) or drop
    // the siblings' JoinHandles (the old run_batch early-return). Uses a
    // runner that returns Err(BatchErrors{successes, failures}) so the
    // executor's handle_partitioned_batch_error path is exercised.
    #[tokio::test]
    async fn batch_error_preserves_successful_siblings() {
        // Runner: run_step always succeeds (for the on_error handler step);
        // run_batch returns BatchErrors with [ok, fail, ok].
        struct BatchErrorsRunner;
        #[async_trait::async_trait]
        impl StepRunner for BatchErrorsRunner {
            async fn run_step(&self, name: &str, _prompt: &str, persona: &str) -> Result<String> {
                Ok(format!("{persona}:{name}:done"))
            }

            async fn run_batch(&self, steps: Vec<StepRequest>) -> Result<Vec<(String, String)>> {
                let mut successes = Vec::new();
                let mut failures: Vec<(String, anyhow::Error)> = Vec::new();
                for req in steps {
                    if req.name == "fail" {
                        failures.push((req.name, anyhow::anyhow!("boom")));
                    } else {
                        successes.push((req.name.clone(), format!("{}:done", req.name)));
                    }
                }
                Err(anyhow::Error::from(BatchErrors {
                    successes,
                    failures,
                }))
            }
        }

        let wf = Workflow {
            name: "batch_siblings".into(),
            steps: vec![
                Step {
                    name: "ok1".into(),
                    kind: StepKind::Agent,
                    prompt: Some("ok1".into()),
                    persona: Some("explore".into()),
                    command: None,
                    tool_name: None,
                    tool_arguments: None,
                    depends_on: vec![],
                    critique: None,
                    condition: None,
                    on_error: None,
                    fork_from: None,
                    over: None,
                    as_name: None,
                    max_parallel: None,
                },
                Step {
                    name: "fail".into(),
                    kind: StepKind::Agent,
                    prompt: Some("fail".into()),
                    persona: Some("explore".into()),
                    command: None,
                    tool_name: None,
                    tool_arguments: None,
                    depends_on: vec![],
                    critique: None,
                    condition: None,
                    on_error: Some("handler".into()),
                    fork_from: None,
                    over: None,
                    as_name: None,
                    max_parallel: None,
                },
                Step {
                    name: "ok2".into(),
                    kind: StepKind::Agent,
                    prompt: Some("ok2".into()),
                    persona: Some("coder".into()),
                    command: None,
                    tool_name: None,
                    tool_arguments: None,
                    depends_on: vec![],
                    critique: None,
                    condition: None,
                    on_error: None,
                    fork_from: None,
                    over: None,
                    as_name: None,
                    max_parallel: None,
                },
                Step {
                    name: "handler".into(),
                    kind: StepKind::Agent,
                    prompt: Some("handle".into()),
                    persona: Some("plan".into()),
                    command: None,
                    tool_name: None,
                    tool_arguments: None,
                    depends_on: vec![],
                    critique: None,
                    condition: None,
                    on_error: None,
                    fork_from: None,
                    over: None,
                    as_name: None,
                    max_parallel: None,
                },
            ],
            budget: None,
        };
        let exe = WorkflowExecutor::new(wf);
        let summary = exe.run(Arc::new(BatchErrorsRunner), None).await.unwrap();
        // Both ok siblings' real outputs must be preserved (not "step failed").
        let ok1 = summary.step("ok1").expect("ok1 output must be preserved");
        assert_eq!(ok1.summary, "ok1:done");
        assert_eq!(ok1.persona, "explore");
        let ok2 = summary.step("ok2").expect("ok2 output must be preserved");
        assert_eq!(ok2.summary, "ok2:done");
        assert_eq!(ok2.persona, "coder");
        // Only the failed step is marked failed.
        let fail = summary.step("fail").expect("fail output present");
        assert!(
            fail.summary.contains("step failed: boom"),
            "fail summary must name the error, got: {}",
            fail.summary
        );
        // The on_error handler was routed to.
        let handler = summary
            .step("handler")
            .expect("on_error handler output present");
        assert!(
            handler.summary.contains("error handler triggered by:"),
            "handler summary must name the trigger, got: {}",
            handler.summary
        );
    }

    // WO 44.45 defect 3: a BatchErrors with NO on_error route propagates the
    // first failure (does not silently swallow it).
    #[tokio::test]
    async fn batch_error_without_on_error_propagates() {
        struct FailNoRouteRunner;
        #[async_trait::async_trait]
        impl StepRunner for FailNoRouteRunner {
            async fn run_step(&self, _name: &str, _prompt: &str, persona: &str) -> Result<String> {
                Ok(format!("{persona}:done"))
            }

            async fn run_batch(&self, _steps: Vec<StepRequest>) -> Result<Vec<(String, String)>> {
                Err(anyhow::Error::from(BatchErrors {
                    successes: vec![("ok".into(), "ok:done".into())],
                    failures: vec![("fail".into(), anyhow::anyhow!("unrecoverable"))],
                }))
            }
        }

        let wf = Workflow {
            name: "batch_no_route".into(),
            steps: vec![
                Step {
                    name: "ok".into(),
                    kind: StepKind::Agent,
                    prompt: Some("ok".into()),
                    persona: Some("explore".into()),
                    command: None,
                    tool_name: None,
                    tool_arguments: None,
                    depends_on: vec![],
                    critique: None,
                    condition: None,
                    on_error: None,
                    fork_from: None,
                    over: None,
                    as_name: None,
                    max_parallel: None,
                },
                Step {
                    name: "fail".into(),
                    kind: StepKind::Agent,
                    prompt: Some("fail".into()),
                    persona: Some("explore".into()),
                    command: None,
                    tool_name: None,
                    tool_arguments: None,
                    depends_on: vec![],
                    critique: None,
                    condition: None,
                    on_error: None,
                    fork_from: None,
                    over: None,
                    as_name: None,
                    max_parallel: None,
                },
            ],
            budget: None,
        };
        let exe = WorkflowExecutor::new(wf);
        let err = exe
            .run(Arc::new(FailNoRouteRunner), None)
            .await
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("unrecoverable"),
            "propagated error must name the failure, got: {err}"
        );
    }
}
