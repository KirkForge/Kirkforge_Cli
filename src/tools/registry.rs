use crate::session::access::{DenyList, PathGuard};
use crate::shared::{ComputerUseConfig, DockerConfig, SandboxConfig};
use crate::tools::computer_use::{ChromeTab, SessionLauncher};
use crate::tools::task::TaskConcurrencyMode;
use crate::tools::{Tool, UndoStackRef};
use kf_lsp::LspPool;
use std::path::PathBuf;
use std::sync::Arc;

/// Holds the shared resources needed to construct all built-in tools.
///
/// Instead of passing 13 individual arguments to `all_tools()`, callers
/// build a `ToolContextBuilder` once and pass a reference. Each field
/// corresponds to a resource that one or more tools need at construction
/// time.
pub struct ToolContextBuilder {
    pub undo_stack: Option<UndoStackRef>,
    pub supports_images: bool,
    pub deny_list: DenyList,
    pub path_guard: PathGuard,
    pub bash_sandbox_workdir: bool,
    pub minify_write_side: bool,
    pub minify_above_bytes: usize,
    pub lsp_pool: Option<Arc<LspPool>>,
    pub computer_use_enabled: bool,
    pub computer_use_config: Option<ComputerUseConfig>,
    pub chrome_tab: Option<Arc<dyn ChromeTab>>,
    pub session_launcher: Option<SessionLauncher>,
    pub docker_config: Option<DockerConfig>,
    pub sandbox_config: SandboxConfig,
    /// Extra landlock allow-list paths (WO 27.1), sourced from
    /// `config.security.landlock_extra_paths` (converted to PathBuf).
    pub landlock_extra_paths: Vec<PathBuf>,
    pub block_edits: bool,
    pub max_background_tasks: usize,
    pub task_concurrency_mode: TaskConcurrencyMode,
}

/// A builder that collects tools and produces the final tool list.
///
/// Usage:
/// ```ignore
/// let tools = ToolRegistry::new()
///     .register(Arc::new(ReadFile::new(...)))
///     .register(Arc::new(Bash::new(...)))
///     .build();
/// ```
pub struct ToolRegistry {
    tools: Vec<Arc<dyn Tool>>,
}

#[allow(clippy::new_without_default)]
impl ToolRegistry {
    pub fn new() -> Self {
        Self { tools: Vec::new() }
    }

    pub fn register(&mut self, tool: Arc<dyn Tool>) -> &mut Self {
        self.tools.push(tool);
        self
    }

    pub fn build(self) -> Vec<Arc<dyn Tool>> {
        self.tools
    }
}
