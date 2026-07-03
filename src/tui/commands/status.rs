//! `/status` slash-command handler — show model, cost, tokens, context pressure.
//!
//! One-shot read of the current session state. Doesn't mutate anything.
//! Replaces the previous stub that returned a placeholder string;
//! `/status` is in the help text and on the keys.rs dispatch line,
//! so a real implementation was overdue.

use crate::tui::app::{AppState, ConnectionState};

/// Render the current session's status as a multi-line string.
///
/// Includes:
///   - model + connection state (from the status bar)
///   - last-turn prompt tokens, total tokens sent/received
///   - per-turn and cumulative cost
///   - context-window pressure (last_turn_prompt_tokens / max_context)
///   - elapsed session time
///   - session id
pub async fn handle_status_command(_args: &str, state: &AppState) -> String {
    let model = match &state.connection {
        ConnectionState::Connected { model, .. } => model.clone(),
        ConnectionState::Disconnected => "(disconnected)".to_string(),
        ConnectionState::Connecting => "(connecting)".to_string(),
        ConnectionState::Error(e) => format!("(error: {e})"),
    };

    // Context pressure: only meaningful once we've done at least one
    // turn AND the adapter has reported a `max_context_tokens`.
    let pressure = match (state.model_info.as_ref(), state.last_turn_prompt_tokens) {
        (Some(info), n) if info.max_context_tokens > 0 && n > 0 => {
            let pct = (n as f64 / info.max_context_tokens as f64) * 100.0;
            let band = if pct < 50.0 {
                "comfortable"
            } else if pct < 80.0 {
                "consider /compact"
            } else {
                "compact now"
            };
            format!(
                "  Context:  {} / {} tokens ({:.1}%) — {}",
                n, info.max_context_tokens, pct, band
            )
        }
        (Some(info), _) => {
            format!(
                "  Context:  (no turn yet) / {} tokens",
                info.max_context_tokens
            )
        }
        (None, _) => "  Context:  (model info unavailable)".to_string(),
    };

    let elapsed = state.session_started.elapsed();
    let elapsed_str = format_duration(elapsed.as_secs());

    format!(
        "Status:\n\
         \n\
         \x20 Model:    {model}\n\
         \x20 Session:  {session_id}\n\
         \x20 Uptime:   {elapsed}\n\
         \n\
         \x20 Tokens sent:     {sent}\n\
         \x20 Tokens received: {recv}\n\
         \x20 Last turn:       {turn}\n\
         \n\
         \x20 Turn cost:       ${turn_cost:.4}\n\
         \x20 Cumulative:      ${cum_cost:.4}\n\
         \n\
         {pressure}\n",
        model = model,
        session_id = state.session_id,
        elapsed = elapsed_str,
        sent = state.tokens_sent,
        recv = state.tokens_received,
        turn = state.last_turn_prompt_tokens,
        turn_cost = state.turn_cost,
        cum_cost = state.cumulative_cost,
        pressure = pressure,
    )
}

/// Render a duration in seconds as a human-readable `Hh Mm` / `Mm Ss` string.
fn format_duration(secs: u64) -> String {
    if secs >= 3600 {
        format!("{}h {:02}m", secs / 3600, (secs % 3600) / 60)
    } else if secs >= 60 {
        format!("{}m {:02}s", secs / 60, secs % 60)
    } else {
        format!("{secs}s")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_duration_sub_minute() {
        assert_eq!(format_duration(0), "0s");
        assert_eq!(format_duration(45), "45s");
    }

    #[test]
    fn format_duration_minutes() {
        assert_eq!(format_duration(60), "1m 00s");
        assert_eq!(format_duration(125), "2m 05s");
    }

    #[test]
    fn format_duration_hours() {
        assert_eq!(format_duration(3600), "1h 00m");
        assert_eq!(format_duration(3725), "1h 02m");
    }
}
