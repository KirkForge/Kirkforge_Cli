pub mod atomic_write;
pub mod bash;
pub mod bash_cancel;
pub mod bash_minify;
pub mod bash_status;
pub mod computer_use;
pub mod edit_file;
pub mod glob;
pub mod grep;
pub mod lsp_query;
pub mod notebook_edit;
pub mod read_file;
pub mod read_image;
pub mod registry;
pub mod task;
pub mod todo;
pub mod web_fetch;
pub mod web_search;
pub mod workflow;
pub mod write_file;

pub use registry::{ToolContextBuilder, ToolRegistry};

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
    /// Cancellation signal from the executor. When this token is
    /// cancelled, the tool should abort its work as soon as possible.
    pub token: CancellationToken,
    /// When `true`, the tool must not mutate external state. Read-only
    /// validation is still allowed; destructive operations should
    /// synthesize a descriptive success message instead.
    pub dry_run: bool,
    /// Optional spawner for isolated subagent tasks. The `task` tool uses
    /// this to run prompts in a separate executor context. When `None`,
    /// the tool reports that task spawning is unavailable.
    pub task_spawner: Option<Arc<dyn task::TaskSpawner>>,
}

impl std::fmt::Debug for ToolContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ToolContext")
            .field("token", &self.token)
            .field("dry_run", &self.dry_run)
            .field("task_spawner", &self.task_spawner.is_some())
            .finish()
    }
}

impl ToolContext {
    /// Context with a fresh, uncancelled token. Used in tests and in
    /// wrappers that do not need to propagate cancellation.
    pub fn new() -> Self {
        Self {
            token: CancellationToken::new(),
            dry_run: false,
            task_spawner: None,
        }
    }

    /// Context with an explicit dry-run flag. Used by the executor when
    /// `Config::dry_run` is enabled.
    #[cfg(test)]
    pub fn with_dry_run(dry_run: bool) -> Self {
        Self {
            token: CancellationToken::new(),
            dry_run,
            task_spawner: None,
        }
    }

    #[cfg(test)]
    pub fn with_spawner(spawner: Arc<dyn task::TaskSpawner>) -> Self {
        Self {
            token: CancellationToken::new(),
            dry_run: false,
            task_spawner: Some(spawner),
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
        bash::Bash, bash_cancel::BashCancel, bash_status::BashStatus, computer_use::ComputerUse,
        edit_file::EditFile, glob::Glob, grep::Grep, lsp_query::LspQuery,
        notebook_edit::NotebookEdit, read_file::ReadFile, read_image::ReadImage, task::Task,
        task::TaskOutput, todo::TodoRead, todo::TodoWrite, web_fetch::WebFetch,
        web_search::WebSearch, workflow::WorkflowTool, write_file::WriteFile,
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
    )));
    registry.register(Arc::new(EditFile::new(
        ctx.undo_stack.clone(),
        ctx.path_guard.clone(),
        ctx.minify_write_side,
    )));
    registry.register(Arc::new(NotebookEdit::new(
        ctx.undo_stack.clone(),
        ctx.path_guard.clone(),
    )));
    registry.register(Arc::new(Bash::new(
        ctx.deny_list.clone(),
        ctx.path_guard.clone(),
        ctx.bash_sandbox_workdir,
        ctx.docker_config.clone(),
        ctx.sandbox_config.clone(),
    )));
    registry.register(Arc::new(BashStatus));
    registry.register(Arc::new(BashCancel));
    registry.register(Arc::new(Grep::new(ctx.path_guard.clone())));
    registry.register(Arc::new(Glob::new(ctx.path_guard.clone())));
    registry.register(Arc::new(WebFetch::new(ctx.deny_list.clone())));
    registry.register(Arc::new(WebSearch::new()));
    registry.register(Arc::new(Task::with_manager(task_manager.clone())));
    registry.register(Arc::new(TaskOutput::new(task_manager)));
    registry.register(Arc::new(WorkflowTool::new()));
    registry.register(Arc::new(TodoWrite::new(todo_state.clone())));
    registry.register(Arc::new(TodoRead::new(todo_state)));

    // Conditionally registered tools.
    registry.register_if(
        ctx.supports_images,
        Arc::new(ReadImage::new(ctx.path_guard.clone())),
    );

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
        };
        let tools = all_tools(&ctx);
        let names: Vec<String> = tools.iter().map(|t| t.def().name.to_string()).collect();
        assert!(
            names.iter().any(|n| n == "lsp_query"),
            "lsp_query should be present when pool is Some: {names:?}"
        );
    }
}
