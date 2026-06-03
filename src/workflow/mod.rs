/// Workflow engine — DAG steps, variable interpolation, execution control.
///
/// A workflow is a sequence of named steps that execute in order.
/// Steps can call tools, issue model prompts, or check conditions.
/// Results pass between steps via `{{var}}` interpolation.
///
/// # Example workflow (YAML description):
///
/// ```yaml
/// name: lint-and-fix
/// steps:
///   - name: lint
///     tool: bash
///     args:
///       command: "cargo clippy -- -D warnings 2>&1"
///
///   - name: check_output
///     condition: "{{lint.exit_code}} != 0"
///     steps:
///       - name: parse_warnings
///         tool: bash
///         args:
///           command: "echo '{{lint.stdout}}' | grep 'warning\\|error'"
///
///       - name: fix_first
///         tool: model
///         prompt: "Fix the first clippy warning:\n{{parse_warnings.stdout}}"
/// ```
use std::collections::HashMap;

// ── Variable extraction / interpolation ──────────────────────────

/// Extract variable references from a string (`{{name}}` or `{{step.field}}`).
pub fn extract_vars(template: &str) -> Vec<String> {
    let mut vars = Vec::new();
    let mut remaining = template;
    while let Some(start) = remaining.find("{{") {
        if let Some(end) = remaining[start + 2..].find("}}") {
            let var = &remaining[start + 2..start + 2 + end];
            vars.push(var.to_string());
            remaining = &remaining[start + 2 + end + 2..];
        } else {
            break;
        }
    }
    vars
}

/// Interpolate variables in a template string.
///
/// Variables use `{{var}}` or `{{step.field}}` syntax.
/// Fields: `stdout`, `stderr`, `exit_code`, `output`, `success`.
pub fn interpolate(template: &str, vars: &HashMap<String, String>) -> String {
    let mut result = String::with_capacity(template.len());
    let mut remaining = template;
    while let Some(start) = remaining.find("{{") {
        // Push everything before the variable
        result.push_str(&remaining[..start]);
        if let Some(end) = remaining[start + 2..].find("}}") {
            let var = &remaining[start + 2..start + 2 + end];
            match vars.get(var) {
                Some(val) => result.push_str(val),
                None => {
                    // Keep unresolved variable as-is
                    result.push_str(&remaining[start..start + 2 + end + 2]);
                }
            }
            remaining = &remaining[start + 2 + end + 2..];
        } else {
            result.push_str(&remaining[start..]);
            break;
        }
    }
    result.push_str(remaining);
    result
}

// ── Error type ────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub enum WorkflowError {
    StepFailed { step: String, message: String },
    VariableNotFound { name: String },
    ConditionFailed { step: String, condition: String },
    MissingTool { name: String },
    MaxIterations { step: String, count: usize },
}

impl std::fmt::Display for WorkflowError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WorkflowError::StepFailed { step, message } => {
                write!(f, "Step '{step}' failed: {message}")
            }
            WorkflowError::VariableNotFound { name } => {
                write!(f, "Variable '{{{{{name}}}}}' not found")
            }
            WorkflowError::ConditionFailed { step, condition } => {
                write!(f, "Condition on step '{step}' failed: {condition}")
            }
            WorkflowError::MissingTool { name } => {
                write!(f, "Tool '{name}' not registered")
            }
            WorkflowError::MaxIterations { step, count } => {
                write!(f, "Step '{step}' reached max iterations ({count})")
            }
        }
    }
}

impl std::error::Error for WorkflowError {}

// ── Data types ────────────────────────────────────────────────────

/// A complete workflow definition.
#[derive(Debug, Clone)]
pub struct Workflow {
    pub name: String,
    pub steps: Vec<Step>,
}

/// A single step in a workflow.
#[derive(Debug, Clone)]
pub enum Step {
    /// Call a tool with named arguments.
    ToolCall {
        name: String,
        tool: String,
        args: HashMap<String, String>,
        /// Variable name to store result under.
        store_as: String,
    },
    /// Issue a prompt to the model.
    ModelPrompt {
        name: String,
        prompt: String,
        store_as: String,
    },
    /// Conditional branching.
    Condition {
        name: String,
        /// If the expression evaluates to true, run these steps.
        condition: String,
        then_steps: Vec<Step>,
        /// Optional else branch.
        else_steps: Vec<Step>,
    },
    /// Loop a sub-sequence.
    Loop {
        name: String,
        steps: Vec<Step>,
        max_iterations: usize,
    },
}

impl Step {
    pub fn name(&self) -> &str {
        match self {
            Step::ToolCall { name, .. } => name,
            Step::ModelPrompt { name, .. } => name,
            Step::Condition { name, .. } => name,
            Step::Loop { name, .. } => name,
        }
    }
}

/// Result of executing a single step.
#[derive(Debug, Clone, Default)]
pub struct StepResult {
    pub success: bool,
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
    pub output: String,
}

impl StepResult {
    /// Build variable map for interpolation.
    pub fn to_vars(&self, prefix: &str) -> HashMap<String, String> {
        let mut map = HashMap::new();
        map.insert(format!("{}.stdout", prefix), self.stdout.clone());
        map.insert(format!("{}.stderr", prefix), self.stderr.clone());
        map.insert(format!("{}.exit_code", prefix), self.exit_code.to_string());
        map.insert(format!("{}.output", prefix), self.output.clone());
        map.insert(format!("{}.success", prefix), self.success.to_string());
        map
    }
}

/// Execution context — carries variables between steps.
#[derive(Debug, Clone, Default)]
pub struct WorkflowContext {
    pub vars: HashMap<String, String>,
    pub step_results: HashMap<String, StepResult>,
}

impl WorkflowContext {
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a variable.
    pub fn set_var(&mut self, key: &str, value: &str) {
        self.vars.insert(key.to_string(), value.to_string());
    }

    /// Store a step result and its field variables.
    pub fn store_step(&mut self, name: &str, result: &StepResult) {
        self.step_results.insert(name.to_string(), result.clone());
        // Also add field-level vars for interpolation
        for (k, v) in result.to_vars(name) {
            self.vars.insert(k, v);
        }
    }

    /// Get a variable value.
    pub fn get_var(&self, key: &str) -> Option<&str> {
        self.vars.get(key).map(|s| s.as_str())
    }

    /// Interpolate string with current variable context.
    pub fn interpolate(&self, template: &str) -> String {
        interpolate(template, &self.vars)
    }
}

// ── Tool trait for workflow ───────────────────────────────────────

/// A tool that a workflow step can call.
#[async_trait::async_trait]
pub trait WorkflowTool: Send + Sync {
    fn name(&self) -> &str;
    async fn run(&self, args: &HashMap<String, String>) -> StepResult;
}

// ── Workflow Engine ───────────────────────────────────────────────

/// Evaluates conditions expressed as simple comparisons.
///
/// Supports: `{{var}} == value`, `{{var}} != value`, `true`, `false`.
fn evaluate_condition(condition: &str, ctx: &WorkflowContext) -> bool {
    let expanded = ctx.interpolate(condition);

    let expanded = expanded.trim();
    if expanded == "true" {
        return true;
    }
    if expanded == "false" {
        return false;
    }

    // Simple comparison: lhs == rhs or lhs != rhs
    if let Some(eq_pos) = expanded.find("==") {
        let lhs = expanded[..eq_pos].trim();
        let rhs = expanded[eq_pos + 2..].trim();
        return lhs == rhs;
    }
    if let Some(ne_pos) = expanded.find("!=") {
        let lhs = expanded[..ne_pos].trim();
        let rhs = expanded[ne_pos + 2..].trim();
        return lhs != rhs;
    }

    // Non-empty string = truthy
    !expanded.is_empty()
}

/// Runs a workflow with a given set of tools.
pub struct WorkflowEngine {
    tools: HashMap<String, Box<dyn WorkflowTool>>,
}

impl WorkflowEngine {
    pub fn new() -> Self {
        Self {
            tools: HashMap::new(),
        }
    }

    pub fn with_tools(tools: Vec<Box<dyn WorkflowTool>>) -> Self {
        let mut map = HashMap::new();
        for t in tools {
            map.insert(t.name().to_string(), t);
        }
        Self { tools: map }
    }

    pub fn register_tool(&mut self, tool: Box<dyn WorkflowTool>) {
        self.tools.insert(tool.name().to_string(), tool);
    }

    /// Execute a workflow from start to finish.
    pub async fn execute(&self, workflow: &Workflow) -> Result<WorkflowContext, WorkflowError> {
        let mut ctx = WorkflowContext::new();
        self.execute_steps(&workflow.steps, &mut ctx).await?;
        Ok(ctx)
    }

    fn get_tool(&self, name: &str) -> Result<&dyn WorkflowTool, WorkflowError> {
        self.tools
            .get(name)
            .map(|b| b.as_ref())
            .ok_or_else(|| WorkflowError::MissingTool { name: name.into() })
    }

    async fn execute_steps(
        &self,
        steps: &[Step],
        ctx: &mut WorkflowContext,
    ) -> Result<(), WorkflowError> {
        for step in steps {
            Box::pin(self.execute_step(step, ctx)).await?;
        }
        Ok(())
    }

    async fn execute_step(
        &self,
        step: &Step,
        ctx: &mut WorkflowContext,
    ) -> Result<(), WorkflowError> {
        match step {
            Step::ToolCall {
                name,
                tool,
                args,
                store_as,
            } => {
                let tool_impl = self.get_tool(tool)?;
                // Interpolate args
                let resolved_args: HashMap<String, String> = args
                    .iter()
                    .map(|(k, v)| (k.clone(), ctx.interpolate(v)))
                    .collect();

                let result = tool_impl.run(&resolved_args).await;
                let store_key = if store_as.is_empty() { name } else { store_as };
                ctx.store_step(store_key, &result);

                if !result.success {
                    return Err(WorkflowError::StepFailed {
                        step: name.clone(),
                        message: format!(
                            "Exit code {}: {}",
                            result.exit_code,
                            result.stderr
                        ),
                    });
                }
                Ok(())
            }

            Step::ModelPrompt {
                name,
                prompt,
                store_as,
            } => {
                let resolved = ctx.interpolate(prompt);
                let result = StepResult {
                    success: true,
                    stdout: resolved.clone(),
                    output: resolved,
                    ..Default::default()
                };
                let store_key = if store_as.is_empty() { name } else { store_as };
                ctx.store_step(store_key, &result);
                Ok(())
            }

            Step::Condition {
                name,
                condition,
                then_steps,
                else_steps,
            } => {
                let cond_true = evaluate_condition(condition, ctx);
                if cond_true {
                    ctx.store_step(
                        name,
                        &StepResult {
                            success: true,
                            stdout: "condition: true".into(),
                            output: "true".into(),
                            ..Default::default()
                        },
                    );
                    Box::pin(self.execute_steps(then_steps, ctx)).await
                } else {
                    ctx.store_step(
                        name,
                        &StepResult {
                            success: true,
                            stdout: "condition: false".into(),
                            output: "false".into(),
                            ..Default::default()
                        },
                    );
                    if !else_steps.is_empty() {
                        Box::pin(self.execute_steps(else_steps, ctx)).await
                    } else {
                        Ok(())
                    }
                }
            }

            Step::Loop {
                name,
                steps,
                max_iterations,
            } => {
                let mut done = false;
                for iteration in 0..*max_iterations {
                    ctx.set_var(&format!("{}.iteration", name), &iteration.to_string());
                    ctx.set_var(
                        &format!("{}.iteration_remaining", name),
                        &(max_iterations - iteration - 1).to_string(),
                    );
                    ctx.set_var(&format!("{}.break", name), "false");

                    // Execute inner steps
                    Box::pin(self.execute_steps(steps, ctx)).await?;

                    // Check loop break variable
                    if ctx.get_var(&format!("{}.break", name)) == Some("true") {
                        done = true;
                    }
                    if done {
                        break;
                    }
                }
                Ok(())
            }
        }
    }
}

// ── Bash tool adapter ─────────────────────────────────────────────

pub struct BashWorkflowTool;

#[async_trait::async_trait]
impl WorkflowTool for BashWorkflowTool {
    fn name(&self) -> &str {
        "bash"
    }

    async fn run(&self, args: &HashMap<String, String>) -> StepResult {
        let command = args.get("command").cloned().unwrap_or_default();
        match tokio::process::Command::new("sh")
            .args(["-c", &command])
            .output()
            .await
        {
            Ok(output) => StepResult {
                success: output.status.success(),
                stdout: String::from_utf8_lossy(&output.stdout).to_string(),
                stderr: String::from_utf8_lossy(&output.stderr).to_string(),
                exit_code: output.status.code().unwrap_or(-1),
                output: String::from_utf8_lossy(&output.stdout).to_string(),
            },
            Err(e) => StepResult {
                success: false,
                stderr: e.to_string(),
                exit_code: -1,
                ..Default::default()
            },
        }
    }
}

/// Parse a YAML-like workflow definition into a Workflow struct.
/// This is a minimal parser supporting the step formats above.
pub fn parse_workflow(content: &str) -> anyhow::Result<Workflow> {
    let mut name = String::from("unnamed");
    let mut steps = Vec::new();
    let mut current_section: Option<String> = None;
    let mut pending_tool: Option<String> = None;
    let mut pending_args: HashMap<String, String> = HashMap::new();
    let mut in_args = false;

    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }

        // Top-level keys
        if !line.starts_with(' ') && !line.starts_with('\t') {
            // Flush any pending tool
            if let Some(tool) = pending_tool.take() {
                steps.push(Step::ToolCall {
                    name: current_section.clone().unwrap_or_default(),
                    tool,
                    args: std::mem::take(&mut pending_args),
                    store_as: current_section.clone().unwrap_or_default(),
                });
            }
            in_args = false;
            if let Some(stripped) = trimmed.strip_prefix("name:") {
                name = stripped.trim().trim_matches('"').to_string();
            }
            continue;
        }

        // Indented steps
        if let Some(stripped) = trimmed.strip_prefix("- name:") {
            // Flush any pending tool from previous step
            if let Some(tool) = pending_tool.take() {
                steps.push(Step::ToolCall {
                    name: current_section.clone().unwrap_or_default(),
                    tool,
                    args: std::mem::take(&mut pending_args),
                    store_as: current_section.clone().unwrap_or_default(),
                });
            }
            in_args = false;
            let step_name = stripped.trim().trim_matches('"').to_string();
            current_section = Some(step_name);
        } else if let Some(stripped) = trimmed.strip_prefix("tool:") {
            let tool = stripped.trim().trim_matches('"').to_string();
            pending_tool = Some(tool);
        } else if let Some(stripped) = trimmed.strip_prefix("prompt:") {
            let prompt = stripped.trim().trim_matches('"').to_string();
            pending_tool = None; // not a tool call
            steps.push(Step::ModelPrompt {
                name: current_section.clone().unwrap_or_default(),
                prompt,
                store_as: current_section.clone().unwrap_or_default(),
            });
        } else if trimmed == "args:" {
            in_args = true;
        } else if in_args {
            if let Some(colon) = trimmed.find(':') {
                let key = trimmed[..colon].trim().to_string();
                let value = trimmed[colon + 1..].trim().trim_matches('"').to_string();
                pending_args.insert(key, value);
            }
        }
    }

    // Flush pending tool at end
    if let Some(tool) = pending_tool.take() {
        steps.push(Step::ToolCall {
            name: current_section.clone().unwrap_or_default(),
            tool,
            args: pending_args,
            store_as: current_section.clone().unwrap_or_default(),
        });
    }

    Ok(Workflow { name, steps })
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Variable interpolation ─────────────────────────────────────

    #[test]
    fn test_extract_vars_simple() {
        let vars = extract_vars("Hello {{name}}, your score is {{score}}");
        assert_eq!(vars, vec!["name", "score"]);
    }

    #[test]
    fn test_extract_vars_empty() {
        let vars = extract_vars("Hello world");
        assert!(vars.is_empty());
    }

    #[test]
    fn test_interpolate_simple() {
        let mut vars = HashMap::new();
        vars.insert("name".into(), "Alice".into());
        let result = interpolate("Hello {{name}}!", &vars);
        assert_eq!(result, "Hello Alice!");
    }

    #[test]
    fn test_interpolate_multiple() {
        let mut vars = HashMap::new();
        vars.insert("a".into(), "1".into());
        vars.insert("b".into(), "2".into());
        let result = interpolate("{{a}} + {{b}} = {{c}}", &vars);
        // {{c}} not found — keep as-is
        assert_eq!(result, "1 + 2 = {{c}}");
    }

    #[test]
    fn test_interpolate_no_vars() {
        let vars = HashMap::new();
        let result = interpolate("plain text", &vars);
        assert_eq!(result, "plain text");
    }

    #[test]
    fn test_interpolate_step_field() {
        let mut vars = HashMap::new();
        vars.insert("step.stdout".into(), "output text".into());
        let result = interpolate("Result: {{step.stdout}}", &vars);
        assert_eq!(result, "Result: output text");
    }

    // ── Condition evaluation ───────────────────────────────────────

    #[test]
    fn test_condition_true() {
        let ctx = WorkflowContext::new();
        assert!(evaluate_condition("true", &ctx));
    }

    #[test]
    fn test_condition_false() {
        let ctx = WorkflowContext::new();
        assert!(!evaluate_condition("false", &ctx));
    }

    #[test]
    fn test_condition_eq() {
        let mut ctx = WorkflowContext::new();
        ctx.set_var("exit", "0");
        assert!(evaluate_condition("{{exit}} == 0", &ctx));
        assert!(!evaluate_condition("{{exit}} == 1", &ctx));
    }

    #[test]
    fn test_condition_ne() {
        let mut ctx = WorkflowContext::new();
        ctx.set_var("exit", "1");
        assert!(evaluate_condition("{{exit}} != 0", &ctx));
        assert!(!evaluate_condition("{{exit}} != 1", &ctx));
    }

    // ── Workflow context ───────────────────────────────────────────

    #[test]
    fn test_context_interpolation() {
        let mut ctx = WorkflowContext::new();
        ctx.set_var("model", "deepseek");
        let result = ctx.interpolate("Using {{model}}");
        assert_eq!(result, "Using deepseek");
    }

    #[test]
    fn test_context_store_step() {
        let mut ctx = WorkflowContext::new();
        let result = StepResult {
            success: true,
            stdout: "compiled".into(),
            output: "compiled".into(),
            exit_code: 0,
            ..Default::default()
        };
        ctx.store_step("build", &result);
        assert_eq!(ctx.get_var("build.stdout"), Some("compiled"));
        assert_eq!(ctx.get_var("build.exit_code"), Some("0"));
        assert_eq!(ctx.get_var("build.success"), Some("true"));
    }

    // ── Workflow execution ─────────────────────────────────────────

    #[tokio::test]
    async fn test_tool_call_step() {
        let engine = WorkflowEngine::with_tools(vec![Box::new(BashWorkflowTool)]);
        let workflow = Workflow {
            name: "echo".into(),
            steps: vec![Step::ToolCall {
                name: "greet".into(),
                tool: "bash".into(),
                args: [("command".into(), "echo hello".into())].into(),
                store_as: "greet".into(),
            }],
        };
        let ctx = engine.execute(&workflow).await.unwrap();
        assert_eq!(ctx.get_var("greet.stdout").unwrap().trim(), "hello");
    }

    #[tokio::test]
    async fn test_failed_tool_call() {
        let engine = WorkflowEngine::with_tools(vec![Box::new(BashWorkflowTool)]);
        let workflow = Workflow {
            name: "fail".into(),
            steps: vec![Step::ToolCall {
                name: "bad".into(),
                tool: "bash".into(),
                args: [("command".into(), "exit 1".into())].into(),
                store_as: "bad".into(),
            }],
        };
        let err = engine.execute(&workflow).await.unwrap_err();
        assert!(err.to_string().contains("Exit code 1"));
    }

    #[tokio::test]
    async fn test_missing_tool() {
        let engine = WorkflowEngine::new();
        let workflow = Workflow {
            name: "missing".into(),
            steps: vec![Step::ToolCall {
                name: "step1".into(),
                tool: "nonexistent".into(),
                args: HashMap::new(),
                store_as: "step1".into(),
            }],
        };
        let err = engine.execute(&workflow).await.unwrap_err();
        assert!(matches!(err, WorkflowError::MissingTool { .. }));
    }

    #[tokio::test]
    async fn test_variable_interpolation_in_args() {
        let engine = WorkflowEngine::with_tools(vec![Box::new(BashWorkflowTool)]);

        // First step stores a result, second step references it
        let workflow = Workflow {
            name: "interpolate".into(),
            steps: vec![
                Step::ToolCall {
                    name: "first".into(),
                    tool: "bash".into(),
                    args: [("command".into(), "echo 'world'".into())].into(),
                    store_as: "first".into(),
                },
                Step::ToolCall {
                    name: "second".into(),
                    tool: "bash".into(),
                    args: [("command".into(), "echo 'hello {{first.stdout}}'".into())].into(),
                    store_as: "second".into(),
                },
            ],
        };

        let ctx = engine.execute(&workflow).await.unwrap();
        let output = ctx.get_var("second.stdout").unwrap();
        assert!(output.contains("hello"));
        assert!(output.contains("world"));
    }

    #[tokio::test]
    async fn test_model_prompt_step() {
        let engine = WorkflowEngine::new();
        let workflow = Workflow {
            name: "prompt-test".into(),
            steps: vec![Step::ModelPrompt {
                name: "ask".into(),
                prompt: "What is the capital of France?".into(),
                store_as: "ask".into(),
            }],
        };
        let ctx = engine.execute(&workflow).await.unwrap();
        assert_eq!(
            ctx.get_var("ask.output").unwrap(),
            "What is the capital of France?"
        );
    }

    #[tokio::test]
    async fn test_condition_true_branch() {
        let engine = WorkflowEngine::new();
        let workflow = Workflow {
            name: "conditional".into(),
            steps: vec![Step::Condition {
                name: "check".into(),
                condition: "true".into(),
                then_steps: vec![Step::ModelPrompt {
                    name: "then_branch".into(),
                    prompt: "condition was true".into(),
                    store_as: "then_branch".into(),
                }],
                else_steps: vec![],
            }],
        };
        let ctx = engine.execute(&workflow).await.unwrap();
        assert_eq!(ctx.get_var("check.output").unwrap(), "true");
        assert!(ctx.get_var("then_branch.output").is_some());
    }

    #[tokio::test]
    async fn test_condition_false_branch() {
        let engine = WorkflowEngine::new();
        let workflow = Workflow {
            name: "conditional".into(),
            steps: vec![Step::Condition {
                name: "check".into(),
                condition: "false".into(),
                then_steps: vec![Step::ModelPrompt {
                    name: "then_branch".into(),
                    prompt: "this shouldn't run".into(),
                    store_as: "then_branch".into(),
                }],
                else_steps: vec![Step::ModelPrompt {
                    name: "else_branch".into(),
                    prompt: "condition was false".into(),
                    store_as: "else_branch".into(),
                }],
            }],
        };
        let ctx = engine.execute(&workflow).await.unwrap();
        assert_eq!(ctx.get_var("check.output").unwrap(), "false");
        assert!(ctx.get_var("then_branch.output").is_none());
        assert_eq!(ctx.get_var("else_branch.output").unwrap(), "condition was false");
    }

    #[tokio::test]
    async fn test_loop_execution() {
        let engine = WorkflowEngine::with_tools(vec![Box::new(BashWorkflowTool)]);
        let workflow = Workflow {
            name: "loop-test".into(),
            steps: vec![Step::Loop {
                name: "build-loop".into(),
                max_iterations: 3,
                steps: vec![Step::ToolCall {
                    name: "run".into(),
                    tool: "bash".into(),
                    args: [("command".into(), "echo iteration {{build-loop.iteration}}".into())].into(),
                    store_as: "run".into(),
                }],
            }],
        };
        let ctx = engine.execute(&workflow).await.unwrap();
        let output = ctx.get_var("run.stdout").unwrap();
        // After 3 iterations, the last one should be iteration 2
        assert!(output.contains("iteration"));
    }

    #[tokio::test]
    async fn test_loop_break_variable() {
        let engine = WorkflowEngine::new();
        let workflow = Workflow {
            name: "break-test".into(),
            steps: vec![Step::Loop {
                name: "loop".into(),
                max_iterations: 10,
                steps: vec![
                    Step::ModelPrompt {
                        name: "check".into(),
                        prompt: "iteration {{loop.iteration}}".into(),
                        store_as: "check".into(),
                    },
                ],
            }],
        };
        let ctx = engine.execute(&workflow).await.unwrap();
        // With no break variable set, loop runs all 10 iterations
        // Last iteration should be 9
        assert_eq!(ctx.get_var("loop.iteration").unwrap(), "9");
        assert_eq!(ctx.get_var("check.output").unwrap(), "iteration 9");
    }

    #[tokio::test]
    async fn test_parse_and_execute_workflow() {
        let content = r#"
name: "hello-workflow"
steps:
  - name: "greet"
    tool: "bash"
    args:
      command: echo hello
"#;
        let workflow = parse_workflow(content).unwrap();
        assert_eq!(workflow.name, "hello-workflow");

        let engine = WorkflowEngine::with_tools(vec![Box::new(BashWorkflowTool)]);
        let ctx = engine.execute(&workflow).await.unwrap();
        assert_eq!(ctx.get_var("greet.stdout").unwrap().trim(), "hello");
    }

    #[test]
    fn test_parse_workflow_with_prompt() {
        let content = r#"
name: "ask-workflow"
steps:
  - name: "ask1"
    prompt: "What is 2+2?"
  - name: "ask2"
    prompt: "What is 3+3?"
"#;
        let workflow = parse_workflow(content).unwrap();
        assert_eq!(workflow.steps.len(), 2);
    }

    #[test]
    fn test_parse_workflow_no_name() {
        let content = "steps:\n  - name: test\n    tool: bash";
        let workflow = parse_workflow(content).unwrap();
        assert_eq!(workflow.name, "unnamed");
        assert_eq!(workflow.steps.len(), 1);
    }
}