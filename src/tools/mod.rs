pub mod read_file;
pub mod write_file;
pub mod edit_file;
pub mod bash;
pub mod bash_status;
pub mod bash_cancel;
pub mod grep;
pub mod glob;

use crate::shared::{ToolDef, ToolOutcome};
use std::sync::Arc;

/// A tool that can be invoked by the model.
/// Each tool provides its definition (name, description, JSON schema)
/// and an async run function.
#[async_trait::async_trait]
pub trait Tool: Send + Sync {
    fn def(&self) -> ToolDef;
    async fn run(&self, args: serde_json::Value) -> ToolOutcome;
}

/// All built-in tools.
pub fn all_tools() -> Vec<Arc<dyn Tool>> {
    vec![
        Arc::new(read_file::ReadFile),
        Arc::new(write_file::WriteFile),
        Arc::new(edit_file::EditFile),
        Arc::new(bash::Bash),
        Arc::new(bash_status::BashStatus),
        Arc::new(bash_cancel::BashCancel),
        Arc::new(grep::Grep),
        Arc::new(glob::Glob),
    ]
}