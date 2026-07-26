//! `workflow_run` tool: invoke a `kirkforge-workflow` template as a tool
//! call so the agent loop and the bench harness can run workflows the
//! same way the TUI `/workflow run` slash command does.
//!
//! Mirrors `src/tools/task.rs`: the tool holds no state of its own; it
//! borrows a `StepRunner` from the `ToolContext`'s `task_spawner` (wrapped
//! by `TaskSpawnerStepRunner`) and delegates execution to
//! `WorkflowExecutor::run` from the `kirkforge-workflow` crate.
//!
//! The workflow crate is always compiled into the binary (not feature
//! gated — see root `Cargo.toml`), so this tool is registered
//! unconditionally in `all_tools()`. When `ctx.task_spawner` is `None`
//! (e.g. inside a sandboxed bench run that does not wire up a spawner),
//! the tool returns `ToolOutcome::Error` rather than silently no-op'ing.

use crate::shared::{ToolDef, ToolError, ToolOutcome};
use crate::tools::task::TaskSpawner;
use crate::tools::{Tool, ToolContext};
use anyhow::{Context, Result};
use kirkforge_workflow::{StepOutput, StepRequest, StepRunner, Workflow, WorkflowExecutor};
use std::collections::HashMap;
use std::sync::Arc;

/// `workflow_run` tool. Stateless: every invocation resolves a template
/// by name, interpolates `${var}` tokens into step prompts, and runs the
/// workflow via the `StepRunner` adapted from `ctx.task_spawner`.
pub struct WorkflowTool;

impl WorkflowTool {
    pub fn new() -> Self {
        Self
    }
}

impl Default for WorkflowTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl Tool for WorkflowTool {
    fn def(&self) -> ToolDef {
        ToolDef {
            name: "workflow_run",
            description: "Run a named workflow template (a JSON DAG of persona-driven steps) \
            and return the per-step summaries as JSON. Templates are resolved from \
            `.kirkforge/workflows/<template>.json` or \
            `~/.local/share/kirkforge/workflows/<template>.json`. `${var}` tokens in step \
            prompts are interpolated from the `vars` map. Each step runs as an isolated \
            subagent under the persona declared in the template (`explore`, `plan`, or \
            `coder`); the tool reuses the same in-process spawner as the `task` tool.",
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "template": {
                        "type": "string",
                        "description": "Workflow template name (file stem, no `.json` extension). \
                        Resolved from `.kirkforge/workflows/<template>.json` or the user share dir."
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

        match run_workflow(&template, &vars, spawner).await {
            Ok(json) => ToolOutcome::Success { content: json },
            Err(e) => ToolOutcome::Error {
                message: format!("workflow '{template}' failed: {e}"),
            },
        }
    }
}

async fn run_workflow(
    template: &str,
    vars: &HashMap<String, String>,
    spawner: Arc<dyn TaskSpawner>,
) -> Result<String> {
    let path = kirkforge_workflow::find_workflow_file(template)
        .with_context(|| format!("workflow template '{template}' not found"))?;
    let raw = std::fs::read(&path).with_context(|| format!("failed to read {}", path.display()))?;
    let mut workflow = Workflow::from_json(&raw)?;
    interpolate_vars(&mut workflow, vars);

    let executor = WorkflowExecutor::new(workflow);
    let runner = TaskSpawnerStepRunner { spawner };
    let summary = executor.run(&runner, None).await?;
    Ok(summary_to_json(&summary))
}

/// Replace `${name}` tokens in every step's `prompt` with the value from
/// `vars`. Unknown tokens are left intact (so partial interpolation is a
/// warning, not an error — the workflow can still run and the model will
/// see the literal `${name}` in the prompt).
fn interpolate_vars(workflow: &mut Workflow, vars: &HashMap<String, String>) {
    if vars.is_empty() {
        return;
    }
    for step in &mut workflow.steps {
        for (k, v) in vars {
            step.prompt = step.prompt.replace(&format!("${{{k}}}"), v);
        }
    }
}

/// Render a `WorkflowSummary` as compact JSON for the model. The workflow
/// crate's `WorkflowSummary` carries outputs in a `HashMap` (non-
/// deterministic iteration), so we sort step names alphabetically for a
/// stable, comparable result. The crate does not expose the original
/// declaration order on the summary, so alphabetical is the honest
/// stable choice.
fn summary_to_json(summary: &kirkforge_workflow::WorkflowSummary) -> String {
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
            "persona": s.persona,
            "summary": s.summary,
            "critique": s.critique,
        })).collect::<Vec<_>>()
    })
    .to_string()
}

/// Adapter that presents a `TaskSpawner` as a `StepRunner`. Each step
/// becomes a `TaskRequest` with the step's persona; the spawner returns
/// the subagent's final assistant summary, which becomes the step's
/// `summary`. Critique passes (`with_critique = true`) spawn an extra
/// `plan`-persona subagent over the just-produced summary.
struct TaskSpawnerStepRunner {
    spawner: Arc<dyn TaskSpawner>,
}

#[async_trait::async_trait]
impl StepRunner for TaskSpawnerStepRunner {
    async fn run_step(&self, name: &str, prompt: &str, persona: &str) -> Result<String> {
        self.spawner
            .run_task(crate::tools::task::TaskRequest {
                prompt: prompt.to_string(),
                persona: persona.to_string(),
                model: None,
            })
            .await
            .map_err(|e| anyhow::anyhow!("step '{name}' failed: {e}"))
    }

    async fn run_batch(&self, steps: Vec<StepRequest>) -> Result<Vec<(String, String)>> {
        // The TaskSpawner interface is serial (one run_task at a time);
        // the spawner itself may spawn an isolated Executor per call, but
        // we do not fan out here. The default StepRunner::run_batch does
        // the same, but inlining keeps the critique pass on this runner
        // so the extra plan-persona subagent reuses the same spawner.
        let mut out = Vec::with_capacity(steps.len());
        for req in steps {
            let summary = self.run_step(&req.name, &req.prompt, &req.persona).await?;
            out.push((req.name, summary));
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
    /// `find_workflow_file` resolves `.kirkforge/workflows/<name>.json`
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
        let wf_dir = dir.join(".kirkforge/workflows");
        std::fs::create_dir_all(&wf_dir).unwrap();
        std::fs::write(wf_dir.join(format!("{name}.json")), body).unwrap();
    }

    #[test]
    fn def_name_and_required_template() {
        let t = WorkflowTool::new();
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
        let t = WorkflowTool::new();
        let ctx = ToolContext::new();
        let out = t.run(&ctx, serde_json::json!({})).await;
        assert!(
            matches!(out, ToolOutcome::Failure(ToolError::InvalidArgs { .. })),
            "got {out:?}"
        );
    }

    #[tokio::test]
    async fn no_spawner_is_error() {
        let t = WorkflowTool::new();
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

        let t = WorkflowTool::new();
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
        let out = WorkflowTool::new()
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
            steps: vec![kirkforge_workflow::Step {
                name: "a".into(),
                prompt: "do ${thing} and ${unknown}".into(),
                persona: "explore".into(),
                depends_on: vec![],
                critique: None,
            }],
        };
        let mut vars = HashMap::new();
        vars.insert("thing".to_string(), "X".to_string());
        interpolate_vars(&mut wf, &vars);
        assert_eq!(wf.steps[0].prompt, "do X and ${unknown}");
    }

    #[test]
    fn interpolate_vars_empty_map_is_noop() {
        let mut wf = Workflow {
            name: "x".into(),
            steps: vec![kirkforge_workflow::Step {
                name: "a".into(),
                prompt: "do ${thing}".into(),
                persona: "explore".into(),
                depends_on: vec![],
                critique: None,
            }],
        };
        interpolate_vars(&mut wf, &HashMap::new());
        assert_eq!(wf.steps[0].prompt, "do ${thing}");
    }
}
