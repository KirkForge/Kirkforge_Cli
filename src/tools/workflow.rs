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

use crate::shared::access::{DenyList, PathGuard};
use crate::shared::bash_safety::check_bash_command_str;
use crate::shared::{ToolDef, ToolError, ToolOutcome};
use crate::tools::task::TaskSpawner;
use crate::tools::toolset::{CompositeToolset, Toolset};
use crate::tools::{Tool, ToolContext};
use anyhow::{bail, Context, Result};
use kf_workflow::{StepOutput, StepRequest, StepRunner, Workflow, WorkflowExecutor};
use std::collections::HashMap;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

/// `workflow_run` tool. Stateless: every invocation resolves a template
/// by name, interpolates `${var}` tokens into step prompts, and runs the
/// workflow via the `StepRunner` adapted from `ctx.task_spawner`.
pub struct WorkflowTool {
    deny_list: DenyList,
    path_guard: PathGuard,
    bash_sandbox_workdir: bool,
}

#[allow(clippy::new_without_default)]
impl WorkflowTool {
    pub fn new(deny_list: DenyList, path_guard: PathGuard, bash_sandbox_workdir: bool) -> Self {
        Self {
            deny_list,
            path_guard,
            bash_sandbox_workdir,
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

        match run_workflow(
            &template,
            &vars,
            spawner,
            ctx.tools.clone(),
            ctx.token.clone(),
            ctx.dry_run,
            &self.deny_list,
            &self.path_guard,
            self.bash_sandbox_workdir,
        )
        .await
        {
            Ok(json) => ToolOutcome::Success { content: json },
            Err(e) => ToolOutcome::Error {
                message: format!("workflow '{template}' failed: {e}"),
            },
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_workflow(
    template: &str,
    vars: &HashMap<String, String>,
    spawner: Arc<dyn TaskSpawner>,
    toolset: Option<Arc<CompositeToolset>>,
    cancel_token: CancellationToken,
    dry_run: bool,
    deny_list: &DenyList,
    path_guard: &PathGuard,
    bash_sandbox_workdir: bool,
) -> Result<String> {
    let path = kf_workflow::find_workflow_file(template)
        .with_context(|| format!("workflow template '{template}' not found"))?;
    let raw = std::fs::read(&path).with_context(|| format!("failed to read {}", path.display()))?;
    let mut workflow = Workflow::from_json(&raw)?;
    interpolate_vars(&mut workflow, vars);

    let executor = WorkflowExecutor::new(workflow);
    let runner = TaskSpawnerStepRunner {
        spawner,
        toolset,
        deny_list: deny_list.clone(),
        path_guard: path_guard.clone(),
        bash_sandbox_workdir,
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
    pub cancel_token: CancellationToken,
    pub dry_run: bool,
}

#[async_trait::async_trait]
impl StepRunner for TaskSpawnerStepRunner {
    async fn run_step(&self, name: &str, prompt: &str, persona: &str) -> Result<String> {
        self.spawner
            .run_task(crate::tools::task::TaskRequest {
                // WO 35.1: callers own the persona preamble — run_task is
                // verbatim now.
                prompt: crate::tools::task::build_task_prompt(persona, prompt),
                persona: persona.to_string(),
                model: None,
                max_turns: 1,
                cancel: None,
            })
            .await
            .map_err(|e| anyhow::anyhow!("step '{name}' failed: {e}"))
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
        let output = tokio::process::Command::new("sh")
            .arg("-c")
            .arg(command)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .output()
            .await
            .with_context(|| format!("step '{name}': failed to spawn bash for: {command}"))?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        if !output.status.success() {
            bail!(
                "step '{name}': bash exited {} — stderr: {}",
                output
                    .status
                    .code()
                    .map(|c| c.to_string())
                    .unwrap_or_else(|| "signal".into()),
                stderr.trim()
            );
        }
        Ok(format!("{stdout}{stderr}").trim_end().to_string())
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

    async fn run_batch(&self, steps: Vec<StepRequest>) -> Result<Vec<(String, String)>> {
        // Fan out independent steps in parallel. Each step is dispatched to
        // its own tokio task so they run concurrently.
        let mut handles = Vec::with_capacity(steps.len());
        for req in steps {
            let spawner = self.spawner.clone();
            let toolset = self.toolset.clone();
            let cancel_token = self.cancel_token.child_token();
            let dry_run = self.dry_run;
            let deny_list = self.deny_list.clone();
            let path_guard = self.path_guard.clone();
            let bash_sandbox_workdir = self.bash_sandbox_workdir;
            let handle = tokio::spawn(async move {
                match req.kind {
                    kf_workflow::StepKind::Agent => {
                        let result = spawner
                            .run_task(crate::tools::task::TaskRequest {
                                prompt: crate::tools::task::build_task_prompt(
                                    &req.persona,
                                    &req.prompt,
                                ),
                                persona: req.persona,
                                model: None,
                                max_turns: 1,
                                cancel: None,
                            })
                            .await
                            .map_err(|e| anyhow::anyhow!("step '{}' failed: {e}", req.name))?;
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
                        let output = tokio::process::Command::new("sh")
                            .arg("-c")
                            .arg(&req.command)
                            .stdout(std::process::Stdio::piped())
                            .stderr(std::process::Stdio::piped())
                            .output()
                            .await
                            .with_context(|| {
                                format!(
                                    "step '{}': failed to spawn bash for: {}",
                                    req.name, req.command
                                )
                            })?;
                        let stdout = String::from_utf8_lossy(&output.stdout);
                        let stderr = String::from_utf8_lossy(&output.stderr);
                        if !output.status.success() {
                            bail!(
                                "step '{}': bash exited {} — stderr: {}",
                                req.name,
                                output
                                    .status
                                    .code()
                                    .map(|c| c.to_string())
                                    .unwrap_or_else(|| "signal".into()),
                                stderr.trim()
                            );
                        }
                        Ok((req.name, format!("{stdout}{stderr}").trim_end().to_string()))
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
            handles.push(handle);
        }

        let mut out = Vec::with_capacity(handles.len());
        for h in handles {
            out.push(
                h.await
                    .map_err(|e| anyhow::anyhow!("batch task panicked: {e}"))??,
            );
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::task::TaskRequest;
    use crate::tools::ToolContext;
    use std::sync::{Mutex as StdMutex, OnceLock};
    use tokio::sync::Mutex;

    /// Global guard serializing tests that mutate the process CWD.
    /// `find_workflow_file` resolves `.kf-code/workflows/<name>.json`
    /// relative to CWD, so two CDB-mutating tests running in parallel
    /// would race and flake. The guard is process-local and test-only.
    /// `tokio::sync::Mutex` (not `std::sync`) because the guard is held
    /// across the `tool.run(...).await` call — `find_workflow_file` runs
    /// inside that async call and must observe the test's CDB.
    static CWD_GUARD: OnceLock<Mutex<()>> = OnceLock::new();
    async fn cwd_lock() -> tokio::sync::MutexGuard<'static, ()> {
        CWD_GUARD.get_or_init(|| Mutex::new(())).lock().await
    }

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
        let _guard = cwd_lock().await;
        let tmp = tempfile::tempdir().unwrap();
        write_template(
            tmp.path(),
            "demo",
            r#"{"name":"demo","steps":[
                {"name":"explore","prompt":"Map ${feature}","persona":"explore"},
                {"name":"plan","prompt":"Design ${feature}","persona":"plan","depends_on":["explore"]}
            ]}"#,
        );
        let cwd = std::env::current_dir().unwrap();
        std::env::set_current_dir(tmp.path()).unwrap();

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
        std::env::set_current_dir(cwd).unwrap();

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
        let _guard = cwd_lock().await;
        let tmp = tempfile::tempdir().unwrap();
        let cwd = std::env::current_dir().unwrap();
        std::env::set_current_dir(tmp.path()).unwrap();
        let spawner: Arc<dyn TaskSpawner> = Arc::new(EchoSpawner {
            calls: Arc::new(StdMutex::new(Vec::new())),
        });
        let ctx = ToolContext::with_spawner(spawner);
        let out = WorkflowTool::new(DenyList::default(), PathGuard::default(), false)
            .run(&ctx, serde_json::json!({"template": "nope"}))
            .await;
        std::env::set_current_dir(cwd).unwrap();
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
}
