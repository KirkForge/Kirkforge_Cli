//! Programmable JSON workflow engine for KirkForge.
//!
//! Workflows are user-editable DAGs of persona-driven steps. Each step is
//! executed by reusing the existing `task` tool's `InProcessTaskSpawner`
//! through the `TaskSpawner` trait — this crate defines the schema and
//! dependency resolver; the binary crate provides the spawner and persona
//! tool restrictions.
//!
//! # Schema
//!
//! ```json
//! {
//!   "name": "add-feature",
//!   "steps": [
//!     {"name": "explore", "prompt": "Map the codebase areas relevant to <X>", "persona": "explore"},
//!     {"name": "plan", "prompt": "Design the implementation for <X>", "persona": "plan", "depends_on": ["explore"]},
//!     {"name": "execute", "prompt": "Implement <X> per the plan", "persona": "coder", "depends_on": ["plan"]}
//!   ]
//! }
//! ```
//!
//! `persona` must be `explore`, `plan`, or `coder`. `critique` is an optional
//! bool; when true, the step is additionally run with the `plan` persona and
//! the critique output is appended to the step's context.
//!
//! # Loading
//!
//! Workflow files are JSON loaded from `.kirkforge/workflows/<name>.json` or
//! `~/.local/share/kirkforge/workflows/<name>.json`. Built-in templates live
//! in `crates/kirkforge-workflow/templates/` and are copied to the user share
//! directory on first use.

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

/// One step in a workflow.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Step {
    /// Unique identifier within the workflow.
    pub name: String,
    /// Prompt sent to the subagent.
    pub prompt: String,
    /// Persona/tool restriction: explore, plan, or coder.
    pub persona: String,
    /// Prior step names that must complete before this one runs.
    #[serde(default)]
    pub depends_on: Vec<String>,
    /// If true, also run the step through the `plan` persona as a critique
    /// and append that output to the step summary.
    #[serde(default)]
    pub critique: Option<bool>,
}

/// A workflow: a named DAG of steps.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Workflow {
    pub name: String,
    #[serde(default)]
    pub steps: Vec<Step>,
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

    /// Validate the workflow: duplicate names, unknown personas, missing
    /// dependencies, and dependency cycles.
    pub fn validate(&self) -> Result<()> {
        let mut names = HashSet::new();
        for step in &self.steps {
            if !names.insert(step.name.clone()) {
                bail!("duplicate step name: {}", step.name);
            }
            if !matches!(step.persona.as_str(), "explore" | "plan" | "coder") {
                bail!(
                    "step '{}' has unknown persona '{}'; expected explore/plan/coder",
                    step.name,
                    step.persona
                );
            }
            for dep in &step.depends_on {
                if dep == &step.name {
                    bail!("step '{}' depends on itself", step.name);
                }
                if !self.steps.iter().any(|s| &s.name == dep) {
                    bail!("step '{}' depends on unknown step '{}'", step.name, dep);
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
#[derive(Debug, Clone, Default)]
pub struct StepOutput {
    pub name: String,
    pub persona: String,
    pub summary: String,
    pub critique: Option<String>,
}

/// Trait abstracting how the workflow executor runs steps.
/// The binary crate implements this with `tools::task::InProcessTaskSpawner`.
#[async_trait::async_trait]
pub trait StepRunner: Send + Sync {
    /// Run one step prompt under the given persona and return the summary.
    async fn run_step(&self, name: &str, prompt: &str, persona: &str) -> Result<String>;

    /// Run a batch of independent steps and return their summaries in input order.
    ///
    /// The default implementation runs sequentially; hosts that can dispatch
    /// subagents in parallel should override this.
    async fn run_batch(&self, steps: Vec<StepRequest>) -> Result<Vec<(String, String)>> {
        let mut out = Vec::with_capacity(steps.len());
        for req in steps {
            let summary = self.run_step(&req.name, &req.prompt, &req.persona).await?;
            out.push((req.name, summary));
        }
        Ok(out)
    }
}

/// Input for one step in a batch.
#[derive(Debug, Clone)]
pub struct StepRequest {
    pub name: String,
    pub prompt: String,
    pub persona: String,
    pub with_critique: bool,
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

    /// Run the workflow to completion, invoking `runner` for each step.
    ///
    /// The runner receives the step prompt concatenated with the human-readable
    /// summaries of all `depends_on` steps. If `cancellation` is set, the
    /// executor stops before scheduling the next step batch and returns an
    /// error.
    pub async fn run(
        &self,
        runner: &dyn StepRunner,
        cancellation: Option<&std::sync::atomic::AtomicBool>,
    ) -> Result<WorkflowSummary> {
        let mut completed: HashSet<String> = HashSet::new();
        let mut outputs: HashMap<String, StepOutput> = HashMap::new();

        while completed.len() < self.workflow.steps.len() {
            if let Some(cancel) = cancellation {
                if cancel.load(std::sync::atomic::Ordering::SeqCst) {
                    bail!("workflow cancelled");
                }
            }

            let ready = self.workflow.ready_steps(&completed);
            if ready.is_empty() {
                bail!("workflow has no ready steps but is not complete (possible cycle)");
            }

            // Build per-step prompts with dependency context.
            let mut tasks: Vec<(String, String, String, bool)> = Vec::new();
            for name in &ready {
                let step = self
                    .workflow
                    .steps
                    .iter()
                    .find(|s| &s.name == name)
                    .ok_or_else(|| anyhow!("missing step {name}"))?;
                let mut prompt = step.prompt.clone();
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
                tasks.push((
                    step.name.clone(),
                    prompt,
                    step.persona.clone(),
                    step.critique.unwrap_or(false),
                ));
            }

            // Run the batch. The runner decides whether to parallelise.
            let mut batch_outputs: HashMap<String, StepOutput> = HashMap::new();
            let batch: Vec<StepRequest> = tasks
                .iter()
                .map(|(name, prompt, persona, with_critique)| StepRequest {
                    name: name.clone(),
                    prompt: prompt.clone(),
                    persona: persona.clone(),
                    with_critique: *with_critique,
                })
                .collect();
            let results = runner.run_batch(batch).await?;
            for ((name, _prompt, persona, with_critique), (_, summary)) in
                tasks.iter().zip(results.iter())
            {
                let critique = if *with_critique {
                    let critique_prompt = format!(
                        "You are a critical reviewer. Evaluate the following output for risks, gaps, and correctness. Keep it concise.\n\nOutput to critique:\n{summary}"
                    );
                    Some(
                        runner
                            .run_step(&format!("{name}-critique"), &critique_prompt, "plan")
                            .await?,
                    )
                } else {
                    None
                };
                batch_outputs.insert(
                    name.clone(),
                    StepOutput {
                        name: name.clone(),
                        persona: persona.clone(),
                        summary: summary.clone(),
                        critique,
                    },
                );
            }

            for (name, output) in batch_outputs {
                completed.insert(name.clone());
                outputs.insert(name, output);
            }
        }

        Ok(WorkflowSummary {
            workflow_name: self.workflow.name.clone(),
            outputs,
        })
    }
}

/// Completed workflow summary.
#[derive(Debug, Clone, Default)]
pub struct WorkflowSummary {
    pub workflow_name: String,
    pub outputs: HashMap<String, StepOutput>,
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
/// 1. `.kirkforge/workflows/<name>.json` in the current directory.
/// 2. `~/.local/share/kirkforge/workflows/<name>.json`.
pub fn find_workflow_file(name: &str) -> Option<PathBuf> {
    let local = PathBuf::from(".kirkforge/workflows").join(format!("{name}.json"));
    if local.exists() {
        return Some(local);
    }
    if let Some(data_dir) = directories::BaseDirs::new() {
        let shared = data_dir
            .data_local_dir()
            .join("kirkforge/workflows")
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
        .map(|b| b.data_local_dir().join("kirkforge/workflows"))
        .unwrap_or_else(|| PathBuf::from(".kirkforge/workflows"))
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
                    prompt: "x".into(),
                    persona: "explore".into(),
                    depends_on: vec!["b".into()],
                    critique: None,
                },
                Step {
                    name: "b".into(),
                    prompt: "x".into(),
                    persona: "plan".into(),
                    depends_on: vec!["a".into()],
                    critique: None,
                },
            ],
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
                    prompt: "Map X".into(),
                    persona: "explore".into(),
                    depends_on: vec![],
                    critique: None,
                },
                Step {
                    name: "plan".into(),
                    prompt: "Design X".into(),
                    persona: "plan".into(),
                    depends_on: vec!["explore".into()],
                    critique: None,
                },
            ],
        };
        let (runner, log) = make_runner();
        let exe = WorkflowExecutor::new(wf);
        let summary = exe.run(&runner, None).await.unwrap();
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
                    prompt: "a".into(),
                    persona: "explore".into(),
                    depends_on: vec![],
                    critique: None,
                },
                Step {
                    name: "b".into(),
                    prompt: "b".into(),
                    persona: "explore".into(),
                    depends_on: vec![],
                    critique: None,
                },
                Step {
                    name: "c".into(),
                    prompt: "c".into(),
                    persona: "coder".into(),
                    depends_on: vec!["a".into(), "b".into()],
                    critique: None,
                },
            ],
        };
        let (runner, log) = make_runner();
        let exe = WorkflowExecutor::new(wf);
        let summary = exe.run(&runner, None).await.unwrap();
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
                    prompt: "x".into(),
                    persona: "explore".into(),
                    depends_on: vec![],
                    critique: None,
                },
                Step {
                    name: "b".into(),
                    prompt: "y".into(),
                    persona: "plan".into(),
                    depends_on: vec!["a".into()],
                    critique: None,
                },
            ],
        };
        let (runner, _log) = make_runner();
        let exe = WorkflowExecutor::new(wf);
        let cancel = AtomicBool::new(true);
        let err = exe
            .run(&runner, Some(&cancel))
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
                prompt: "Design X".into(),
                persona: "plan".into(),
                depends_on: vec![],
                critique: Some(true),
            }],
        };
        let (runner, log) = make_runner();
        let exe = WorkflowExecutor::new(wf);
        let summary = exe.run(&runner, None).await.unwrap();
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

    #[tokio::test]
    async fn independent_steps_run_concurrently() {
        let wf = Workflow {
            name: "parallel".into(),
            steps: vec![
                Step {
                    name: "a".into(),
                    prompt: "a".into(),
                    persona: "explore".into(),
                    depends_on: vec![],
                    critique: None,
                },
                Step {
                    name: "b".into(),
                    prompt: "b".into(),
                    persona: "explore".into(),
                    depends_on: vec![],
                    critique: None,
                },
                Step {
                    name: "c".into(),
                    prompt: "c".into(),
                    persona: "coder".into(),
                    depends_on: vec!["a".into(), "b".into()],
                    critique: None,
                },
            ],
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
        let summary = exe.run(&runner, None).await.unwrap();
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
}
