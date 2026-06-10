//! `/status` slash-command handler — show model, cost, tokens, context pressure.
//!
//! Stub: deepseek's in-flight work referenced this from both
//! `tui/commands/mod.rs` and `tui/keys.rs` (line 298) but the file
//! wasn't created. Added at push time with a minimal placeholder so
//! `cargo check` passes; the real implementation is a follow-up.

use crate::tui::app::AppState;

pub async fn handle_status_command(_args: &str, _state: &AppState) -> String {
    "Status: (stub — see status.rs)".to_string()
}
