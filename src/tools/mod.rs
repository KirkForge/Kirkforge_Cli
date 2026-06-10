pub mod bash;
pub mod bash_cancel;
pub mod bash_minify;
pub mod bash_status;
pub mod edit_file;
pub mod glob;
pub mod grep;
pub mod read_file;
pub mod write_file;

use crate::shared::{ToolDef, ToolOutcome};
use std::sync::{Arc, Mutex};

/// A tool that can be invoked by the model.
/// Each tool provides its definition (name, description, JSON schema)
/// and an async run function.
#[async_trait::async_trait]
pub trait Tool: Send + Sync {
    fn def(&self) -> ToolDef;
    async fn run(&self, args: serde_json::Value) -> ToolOutcome;
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
pub fn all_tools(undo_stack: Option<UndoStackRef>) -> Vec<Arc<dyn Tool>> {
    vec![
        Arc::new(read_file::ReadFile),
        Arc::new(write_file::WriteFile::new(undo_stack.clone())),
        Arc::new(edit_file::EditFile::new(undo_stack)),
        Arc::new(bash::Bash),
        Arc::new(bash_status::BashStatus),
        Arc::new(bash_cancel::BashCancel),
        Arc::new(grep::Grep),
        Arc::new(glob::Glob),
    ]
}
