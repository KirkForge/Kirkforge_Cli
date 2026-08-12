pub mod atomic_write;
pub mod bash;
pub mod bash_minify;
pub mod computer_use;
pub mod edit_file;
pub mod glob;
pub mod grep;
pub mod lsp_query;
pub mod notebook_edit;
pub mod read_file;
pub mod read_image;
pub mod registry;
pub mod remember;
pub mod task;
pub mod todo;
pub mod web_fetch;
pub mod web_search;
pub mod workflow;
pub mod write_file;

pub use registry::{ToolContextBuilder, ToolRegistry};

pub fn validate_tool_args(tool: &dyn Tool, args: &serde_json::Value) -> Result<(), String> {
    let schema = &tool.def().parameters;
    let obj = match schema.get("type").and_then(|t| t.as_str()) {
        Some("object") => schema,
        _ => return Ok(()),
    };
    let properties = obj.get("properties").and_then(|p| p.as_object());
    let required: Vec<&str> = obj
        .get("required")
        .and_then(|r| r.as_array())
        .map(|arr| arr.iter().filter_map(|v| v.as_str()).collect())
        .unwrap_or_default();

    let args_obj = match args.as_object() {
        Some(o) => o,
        None => return Err("args must be a JSON object".into()),
    };

    for &key in &required {
        if !args_obj.contains_key(key) {
            return Err(format!("Missing required argument: {key}"));
        }
    }

    if let Some(props) = properties {
        for (key, val) in args_obj {
            if let Some(prop_schema) = props.get(key) {
                let expected_type = prop_schema.get("type").and_then(|t| t.as_str());
                if let Some(expected) = expected_type {
                    let actual_matches = match expected {
                        "string" => val.is_string(),
                        "number" => val.is_number(),
                        "integer" => val.is_i64() || val.is_u64(),
                        "boolean" => val.is_boolean(),
                        "array" => val.is_array(),
                        "object" => val.is_object(),
                        _ => true,
                    };
                    if !actual_matches && !val.is_null() {
                        return Err(format!(
                            "Argument '{}': expected type '{}', got '{}'",
                            key,
                            expected,
                            value_type_name(val)
                        ));
                    }
                }
            }
        }
    }

    Ok(())
}

fn value_type_name(val: &serde_json::Value) -> &'static str {
    match val {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "boolean",
        serde_json::Value::Number(_) => "number",
        serde_json::Value::String(_) => "string",
        serde_json::Value::Array(_) => "array",
        serde_json::Value::Object(_) => "object",
    }
}

use crate::session::toolset::CompositeToolset;
use crate::shared::{ToolDef, ToolOutcome};
use std::sync::{Arc, Mutex};
use tokio_util::sync::CancellationToken;

/// Per-invocation context passed to every tool.
///
/// This is the seam for cross-cutting concerns: cancellation,
/// per-call deadlines, dry-run mode, and request-scoped metadata. Tools
/// should respect `token` by selecting on it (or on a derived child
/// token) so a user cancel or turn timeout stops work promptly.
#[derive(Clone)]
pub struct ToolContext {
    pub token: CancellationToken,
    pub dry_run: bool,
    pub diff_review: bool,
    pub task_spawner: Option<Arc<dyn task::TaskSpawner>>,
    pub tools: Option<Arc<CompositeToolset>>,
    /// Optional channel for streaming partial tool output (e.g. PTY
    /// output) to the TUI while a command runs. `None` in non-interactive
    /// or test contexts — tools must treat it as best-effort.
    pub event_tx: Option<tokio::sync::mpsc::Sender<crate::session::executor::TurnEvent>>,
}

impl std::fmt::Debug for ToolContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ToolContext")
            .field("token", &self.token)
            .field("dry_run", &self.dry_run)
            .field("diff_review", &self.diff_review)
            .field("task_spawner", &self.task_spawner.is_some())
            .field("tools", &self.tools.is_some())
            .field("event_tx", &self.event_tx.is_some())
            .finish()
    }
}

impl ToolContext {
    pub fn new() -> Self {
        Self {
            token: CancellationToken::new(),
            dry_run: false,
            diff_review: true,
            task_spawner: None,
            tools: None,
            event_tx: None,
        }
    }

    /// Context with an explicit dry-run flag. Used by the executor when
    /// `Config::dry_run` is enabled.
    #[cfg(test)]
    pub fn with_dry_run(dry_run: bool) -> Self {
        Self {
            token: CancellationToken::new(),
            dry_run,
            diff_review: true,
            task_spawner: None,
            tools: None,
            event_tx: None,
        }
    }

    #[cfg(test)]
    pub fn with_spawner(spawner: Arc<dyn task::TaskSpawner>) -> Self {
        Self {
            token: CancellationToken::new(),
            dry_run: false,
            diff_review: true,
            task_spawner: Some(spawner),
            tools: None,
            event_tx: None,
        }
    }
}

impl Default for ToolContext {
    fn default() -> Self {
        Self::new()
    }
}

/// A tool that can be invoked by the model.
/// Each tool provides its definition (name, description, JSON schema)
/// and an async run function.
#[async_trait::async_trait]
pub trait Tool: Send + Sync {
    fn def(&self) -> ToolDef;
    async fn run(&self, ctx: &ToolContext, args: serde_json::Value) -> ToolOutcome;
}

/// Type alias for the per-session undo stack. Tools that mutate
/// files (`edit_file`, `write_file`) hold an `Option<UndoStackRef>`
/// and snapshot pre-edit bytes before the destructive write.
///
/// `Mutex` because the executor and the TUI's `/undo` handler both
/// touch it. The critical sections are tiny (push a snapshot, pop a
/// file) so contention is not a concern.
pub type UndoStackRef = Arc<Mutex<crate::session::undo::UndoStack>>;

/// All built-in tools.
///
/// Constructs the full set of built-in tools from the shared resources in
/// `ctx`. Tools that require optional capabilities (images, LSP,
/// computer_use) are conditionally registered based on the corresponding
/// flags in `ctx`.
pub fn all_tools(ctx: &ToolContextBuilder) -> Vec<Arc<dyn Tool>> {
    use crate::tools::{
        bash::Bash, bash::BashCancel, bash::BashStatus, computer_use::ComputerUse,
        edit_file::EditFile, glob::Glob, grep::Grep, lsp_query::LspQuery,
        notebook_edit::NotebookEdit, read_file::ReadFile, read_image::ReadImage,
        remember::Remember, task::Task, task::TaskOutput, todo::TodoRead, todo::TodoWrite,
        web_fetch::WebFetch, web_search::WebSearch, workflow::WorkflowTool, write_file::WriteFile,
    };

    let task_manager = Arc::new(std::sync::Mutex::new(task::TaskManager::new()));
    let todo_state: todo::TodoState = Arc::new(std::sync::Mutex::new(Vec::new()));

    let mut registry = ToolRegistry::new();

    // Core tools — always registered.
    registry.register(Arc::new(ReadFile::new(
        ctx.path_guard.clone(),
        ctx.minify_write_side,
        ctx.minify_above_bytes,
    )));
    registry.register(Arc::new(WriteFile::new(
        ctx.undo_stack.clone(),
        ctx.path_guard.clone(),
        ctx.minify_write_side,
        ctx.block_edits,
    )));
    registry.register(Arc::new(EditFile::new(
        ctx.undo_stack.clone(),
        ctx.path_guard.clone(),
        ctx.minify_write_side,
        ctx.block_edits,
    )));
    registry.register(Arc::new(NotebookEdit::new(
        ctx.undo_stack.clone(),
        ctx.path_guard.clone(),
    )));
    let mut bash = Bash::new(
        ctx.deny_list.clone(),
        ctx.path_guard.clone(),
        ctx.bash_sandbox_workdir,
        ctx.docker_config.clone(),
        ctx.sandbox_config.clone(),
    );
    // WO 27.1: populate landlock_extra_paths after construction so the
    // Bash::new arity (and its ~20 test call sites) stay unchanged.
    bash.landlock_extra_paths = ctx.landlock_extra_paths.clone();
    registry.register(Arc::new(bash));
    registry.register(Arc::new(BashStatus));
    registry.register(Arc::new(BashCancel));
    registry.register(Arc::new(Grep::new(ctx.path_guard.clone())));
    registry.register(Arc::new(Glob::new(ctx.path_guard.clone())));
    registry.register(Arc::new(WebFetch::new(ctx.deny_list.clone())));
    registry.register(Arc::new(WebSearch::new()));
    registry.register(Arc::new(Task::with_config(
        task_manager.clone(),
        ctx.max_background_tasks,
        ctx.task_concurrency_mode.clone(),
    )));
    registry.register(Arc::new(TaskOutput::new(task_manager)));
    registry.register(Arc::new(WorkflowTool::new(
        ctx.deny_list.clone(),
        ctx.path_guard.clone(),
        ctx.bash_sandbox_workdir,
    )));
    registry.register(Arc::new(TodoWrite::new(todo_state.clone())));
    registry.register(Arc::new(TodoRead::new(todo_state)));
    registry.register(Arc::new(Remember::new()));

    // Conditionally registered tools.
    if ctx.supports_images {
        registry.register(Arc::new(ReadImage::new(ctx.path_guard.clone())));
    }

    if let Some(pool) = ctx.lsp_pool.clone() {
        registry.register(Arc::new(LspQuery::new(pool, ctx.path_guard.clone())));
    }

    if ctx.computer_use_enabled && ctx.supports_images {
        if let Some(config) = ctx.computer_use_config.clone() {
            let tab = ctx
                .chrome_tab
                .clone()
                .unwrap_or_else(|| Arc::new(computer_use::PlaceholderTab));
            registry.register(Arc::new(ComputerUse::new(
                ctx.deny_list.clone(),
                config,
                tab,
                ctx.session_launcher.clone(),
            )));
        }
    }

    registry.build()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_context_default_is_uncancelled() {
        let ctx = ToolContext::default();
        assert!(!ctx.token.is_cancelled());
    }

    #[test]
    fn tool_context_new_is_uncancelled() {
        let ctx = ToolContext::new();
        assert!(!ctx.token.is_cancelled());
    }

    #[test]
    fn tool_context_debug_reports_spawner_presence() {
        let ctx = ToolContext::new();
        let s = format!("{ctx:?}");
        assert!(s.contains("dry_run"));
        assert!(s.contains("task_spawner"));
        assert!(s.contains("false"), "got: {s}");
    }

    #[test]
    fn tool_context_with_dry_run_sets_flag() {
        let ctx = ToolContext::with_dry_run(true);
        assert!(ctx.dry_run);
        assert!(!ctx.token.is_cancelled());
    }

    #[test]
    fn all_tools_returns_core_tools_unconditionally() {
        let ctx = ToolContextBuilder {
            undo_stack: None,
            supports_images: false,
            deny_list: crate::session::access::DenyList::default(),
            path_guard: crate::session::access::PathGuard::default(),
            bash_sandbox_workdir: false,
            minify_write_side: false,
            minify_above_bytes: 0,
            lsp_pool: None,
            computer_use_enabled: false,
            computer_use_config: None,
            chrome_tab: None,
            session_launcher: None,
            docker_config: None,
            sandbox_config: crate::shared::SandboxConfig::default(),
            landlock_extra_paths: Vec::new(),
            block_edits: false,
            max_background_tasks: 4,
            task_concurrency_mode: task::TaskConcurrencyMode::Queue,
        };
        let tools = all_tools(&ctx);
        let names: Vec<String> = tools.iter().map(|t| t.def().name.to_string()).collect();
        for required in [
            "read_file",
            "write_file",
            "edit_file",
            "bash",
            "grep",
            "glob",
            "web_fetch",
            "web_search",
            "task",
            "task_output",
            "todo_write",
            "todo_read",
            "workflow_run",
            "remember",
        ] {
            assert!(
                names.iter().any(|n| n == required),
                "missing {required} in all_tools: {names:?}"
            );
        }
        assert!(
            !names.iter().any(|n| n == "read_image"),
            "read_image must be gated by supports_images=false"
        );
        assert!(
            !names.iter().any(|n| n == "lsp_query"),
            "lsp_query must be gated by lsp_pool=None"
        );
    }

    #[test]
    fn all_tools_includes_read_image_when_supports_images() {
        let ctx = ToolContextBuilder {
            undo_stack: None,
            supports_images: true,
            deny_list: crate::session::access::DenyList::default(),
            path_guard: crate::session::access::PathGuard::default(),
            bash_sandbox_workdir: false,
            minify_write_side: false,
            minify_above_bytes: 0,
            lsp_pool: None,
            computer_use_enabled: false,
            computer_use_config: None,
            chrome_tab: None,
            session_launcher: None,
            docker_config: None,
            sandbox_config: crate::shared::SandboxConfig::default(),
            landlock_extra_paths: Vec::new(),
            block_edits: false,
            max_background_tasks: 4,
            task_concurrency_mode: task::TaskConcurrencyMode::Queue,
        };
        let tools = all_tools(&ctx);
        let names: Vec<String> = tools.iter().map(|t| t.def().name.to_string()).collect();
        assert!(
            names.iter().any(|n| n == "read_image"),
            "read_image should be present when supports_images=true: {names:?}"
        );
    }

    #[test]
    fn all_tools_includes_lsp_query_when_pool_provided() {
        let pool = std::sync::Arc::new(kf_lsp::LspPool::new(
            std::env::current_dir()
                .unwrap()
                .to_string_lossy()
                .to_string(),
            vec![],
        ));
        let ctx = ToolContextBuilder {
            undo_stack: None,
            supports_images: false,
            deny_list: crate::session::access::DenyList::default(),
            path_guard: crate::session::access::PathGuard::default(),
            bash_sandbox_workdir: false,
            minify_write_side: false,
            minify_above_bytes: 0,
            lsp_pool: Some(pool),
            computer_use_enabled: false,
            computer_use_config: None,
            chrome_tab: None,
            session_launcher: None,
            docker_config: None,
            sandbox_config: crate::shared::SandboxConfig::default(),
            landlock_extra_paths: Vec::new(),
            block_edits: false,
            max_background_tasks: 4,
            task_concurrency_mode: task::TaskConcurrencyMode::Queue,
        };
        let tools = all_tools(&ctx);
        let names: Vec<String> = tools.iter().map(|t| t.def().name.to_string()).collect();
        assert!(
            names.iter().any(|n| n == "lsp_query"),
            "lsp_query should be present when pool is Some: {names:?}"
        );
    }

    #[test]
    fn schema_validation_rejects_missing_required_arg() {
        use crate::tools::grep::Grep;
        let grep = Grep::new(crate::session::access::PathGuard::default());
        let err = validate_tool_args(&grep, &serde_json::json!({}));
        assert!(err.is_err(), "should reject missing 'pattern'");
        assert!(err.unwrap_err().contains("pattern"));
    }

    #[test]
    fn schema_validation_accepts_valid_args() {
        use crate::tools::grep::Grep;
        let grep = Grep::new(crate::session::access::PathGuard::default());
        let result = validate_tool_args(
            &grep,
            &serde_json::json!({"pattern": "hello", "path": "/tmp"}),
        );
        assert!(result.is_ok(), "valid args should pass: {result:?}");
    }

    #[test]
    fn schema_validation_rejects_wrong_type() {
        use crate::tools::grep::Grep;
        let grep = Grep::new(crate::session::access::PathGuard::default());
        let err = validate_tool_args(
            &grep,
            &serde_json::json!({"pattern": "hello", "context_lines": "not_a_number"}),
        );
        assert!(err.is_err(), "should reject wrong type for context_lines");
        assert!(err.unwrap_err().contains("context_lines"));
    }
}
