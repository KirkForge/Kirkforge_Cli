/// `/carryover` slash command — inspect and reset cross-session profile.
///
/// The carryover profile is a tiny JSON file (~200 bytes) that stores a
/// handful of high-signal facts from the previous session so the model can
/// pick up context quickly after restart. This command lets the user see what
/// is being carried over or clear it if they want a clean slate.
///
/// Subcommands:
/// - `/carryover show`   — display the current carryover profile
/// - `/carryover clear`  — delete the persisted profile and reset to default
use crate::session::carryover::{clear_carryover, load_carryover};
use crate::tui::app::AppState;

/// Handle `/carryover [show|clear]`.
pub fn handle_carryover_command(args: &str, _state: &mut AppState) -> String {
    let args = args.trim();
    let sub = args.split_whitespace().next().unwrap_or("show");

    match sub {
        "show" | "" => show_carryover(),
        "clear" => {
            clear_carryover();
            "Carryover profile cleared. Next session will start with a blank profile.".to_string()
        }
        "help" | "--help" | "-h" => CARRYOVER_HELP.to_string(),
        other => format!("Unknown /carryover subcommand: {other}\n\n{CARRYOVER_HELP}"),
    }
}

const CARRYOVER_HELP: &str = "/carryover — inspect and reset cross-session memory

Usage:
  /carryover show    Display the persisted carryover profile
  /carryover clear   Delete the profile and start fresh

Carryover is stored in ~/.local/share/kirkforge/carryover.json.";

/// Render the current carryover profile as human-readable text.
fn show_carryover() -> String {
    let profile = load_carryover();
    if profile.session_count == 0 {
        return "No carryover profile yet. Start and finish a session to build one.".to_string();
    }

    let mut lines = Vec::new();
    lines.push(format!(
        "Carryover profile (session #{}, {})",
        profile.session_count, profile.last_session_time
    ));
    if !profile.last_user_message.is_empty() {
        lines.push(format!("Last topic: {}", profile.last_user_message));
    }
    if !profile.recent_paths.is_empty() {
        lines.push(format!(
            "Active paths: {}",
            profile.recent_paths.join(", ")
        ));
    }
    if !profile.tool_usage.is_empty() {
        let mut tools: Vec<(&String, &u64)> = profile.tool_usage.iter().collect();
        tools.sort_by(|a, b| b.1.cmp(a.1));
        let tool_str = tools
            .iter()
            .map(|(name, count)| format!("{name} ({count})"))
            .collect::<Vec<_>>()
            .join(", ");
        lines.push(format!("Top tools: {tool_str}"));
    }

    let active_patterns: Vec<&str> = profile
        .work_patterns
        .iter()
        .filter(|(_, &v)| v > 0.3)
        .map(|(k, _)| k.as_str())
        .collect();
    if !active_patterns.is_empty() {
        lines.push(format!("Patterns: {}", active_patterns.join(", ")));
    }

    let common_warnings: Vec<String> = profile
        .verifier_warnings
        .iter()
        .filter(|(_, &v)| v >= 2)
        .map(|(k, _)| k.clone())
        .collect();
    if !common_warnings.is_empty() {
        lines.push(format!("Recurring: {}", common_warnings.join(", ")));
    }

    lines.push(format!(
        "Estimated prompt tokens: {}",
        profile.estimated_tokens()
    ));
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handle_carryover_unknown_subcommand_returns_usage() {
        let mut state = AppState::new(std::sync::Arc::new(std::sync::RwLock::new(
            crate::shared::Config::default(),
        )));
        let out = handle_carryover_command("foo", &mut state);
        assert!(out.contains("Unknown /carryover subcommand"), "got: {out}");
        assert!(out.contains("Usage"), "got: {out}");
    }
}
