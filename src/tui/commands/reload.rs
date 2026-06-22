//! `/reload` slash-command handler.
//!
//! Re-reads `config.toml` (with environment-variable layering), updates
//! the TUI's shared config in place, and forwards a snapshot to the
//! executor so it rebuilds access-control structures. The executor emits
//! the user-visible confirmation token.

use crate::shared::{read_shared_config, Config};
use crate::tui::app::AppState;
use tokio::sync::mpsc;

/// Handle `/reload` command.
pub async fn handle_reload_command(
    config_tx: &mpsc::UnboundedSender<Config>,
    state: &mut AppState,
) -> String {
    let fresh = crate::session::config::load_config();
    let before = read_shared_config(&state.config).clone();
    let diff_summary = crate::session::config::config_diff_summary(&before, &fresh);

    if let Ok(mut cfg) = state.config.write() {
        *cfg = fresh.clone();
    }

    // Forward the new snapshot to the executor. The executor owns the
    // access-control rebuild and emits the actual confirmation token.
    if config_tx.send(fresh).is_err() {
        return "❌ Config reloaded in TUI, but executor is not running.".into();
    }

    if diff_summary.is_empty() {
        "🔄 Reloaded config (no changes)".into()
    } else {
        format!("🔄 Reloaded config: {}", diff_summary)
    }
}
