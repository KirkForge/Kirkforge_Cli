pub mod bash;
pub mod bash_cancel;
pub mod bash_minify;
pub mod bash_status;
pub mod edit_file;
pub mod glob;
pub mod grep;
pub mod read_file;
pub mod read_image;
pub mod write_file;

use crate::shared::{ToolDef, ToolOutcome};
use std::sync::{Arc, Mutex};
use tokio_util::sync::CancellationToken;

/// Per-invocation context passed to every tool.
///
/// This is the seam for cross-cutting concerns: cancellation,
/// per-call deadlines, dry-run mode, and request-scoped metadata. Tools
/// should respect `token` by selecting on it (or on a derived child
/// token) so a user cancel or turn timeout stops work promptly.
#[derive(Debug, Clone)]
pub struct ToolContext {
    /// Cancellation signal from the executor. When this token is
    /// cancelled, the tool should abort its work as soon as possible.
    pub token: CancellationToken,
    /// When `true`, the tool must not mutate external state. Read-only
    /// validation is still allowed; destructive operations should
    /// synthesize a descriptive success message instead.
    pub dry_run: bool,
}

impl ToolContext {
    /// Context with a fresh, uncancelled token. Used in tests and in
    /// wrappers that do not need to propagate cancellation.
    pub fn new() -> Self {
        Self {
            token: CancellationToken::new(),
            dry_run: false,
        }
    }

    /// Context with an explicit dry-run flag. Used by the executor when
    /// `Config::dry_run` is enabled.
    #[cfg(test)]
    pub fn with_dry_run(dry_run: bool) -> Self {
        Self {
            token: CancellationToken::new(),
            dry_run,
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
/// `undo_stack` is passed only to the file-mutating tools
/// (`edit_file`, `write_file`). Read-only tools don't need it.
/// Pass `None` to disable undo — the tools still work, they just
/// don't snapshot.
///
/// `supports_images` gates the `read_image` tool: a non-vision model
/// never sees the tool in its available-tool list, and any
/// hand-crafted `<tool_call>` invocation in the prompt is the user's
/// problem rather than a server-side 400. The default is `false`
/// (conservative — most Ollama-local models aren't vision-capable).
pub fn all_tools(
    undo_stack: Option<UndoStackRef>,
    supports_images: bool,
    deny_list: crate::session::access::DenyList,
    path_guard: crate::session::access::PathGuard,
    bash_sandbox_workdir: bool,
) -> Vec<Arc<dyn Tool>> {
    let mut tools: Vec<Arc<dyn Tool>> = vec![
        Arc::new(read_file::ReadFile),
        Arc::new(write_file::WriteFile::new(undo_stack.clone())),
        Arc::new(edit_file::EditFile::new(undo_stack)),
        Arc::new(bash::Bash::new(
            deny_list.clone(),
            path_guard.clone(),
            bash_sandbox_workdir,
        )),
        Arc::new(bash_status::BashStatus),
        Arc::new(bash_cancel::BashCancel),
        Arc::new(grep::Grep::new(path_guard.clone())),
        Arc::new(glob::Glob::new(path_guard)),
    ];
    if supports_images {
        tools.push(Arc::new(read_image::ReadImage));
    }
    tools
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
}
