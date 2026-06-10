//! `/model <name>` slash command — hot-swap the active model mid-session.
//!
//! Review.md gap #5: previously, the only way to switch models was to
//! restart the session. With `AdapterSwap::force_swap` plus a TUI →
//! executor channel, the user can type `/model qwen2.5:3b` at any time
//! and have the next turn served by the new model.
//!
//! # Flow
//!
//! 1. The user types `/model qwen2.5:3b` in the input box.
//! 2. `keys::handle_input_key` dispatches to `handle_model_command`
//!    (here), passing the model name + the `model_tx` channel sender.
//! 3. This handler sends the name through `model_tx` to the executor
//!    driver (see `Executor::run`'s `tokio::select!` arm).
//! 4. The executor calls `AdapterSwap::force_swap`, drops the old
//!    adapter, installs the new one, and emits a
//!    `TurnEvent::Token("🔀 Switched to qwen2.5:3b\n")` back through
//!    the event channel so the user sees the confirmation land in
//!    the chat.
//! 5. The handler returns a local "Switching to <name>…" message so
//!    the user gets immediate feedback that the request was
//!    accepted (the executor's confirmation arrives ~ms later).
//!
//! # Failure modes
//!
//! - **No args**: we return a usage hint, no channel send. The
//!   executor is untouched.
//! - **Channel closed**: the executor has already exited (probably
//!   because the user typed `/exit`). The `send` returns an error;
//!   we surface a "executor not running" message. The session is
//!   effectively over anyway, so this is a no-op for the user.
//! - **Unknown model name**: `adapters::adapter_for` is permissive —
//!   it falls through to `OpenAiCompatAdapter` for anything it
//!   doesn't recognise (`src/adapters/mod.rs:48-53`). This is the
//!   right behaviour for `/model` because the user knows what
//!   server they pointed the CLI at; we don't second-guess them.
//!   The `🔀 Switched to …` confirmation lets them see the model
//!   name that was registered, so a typo is immediately visible.

use crate::adapters;
use crate::tui::app::AppState;
use tokio::sync::mpsc;

/// Handle `/model <name>`. Returns a user-facing status string.
///
/// `args` is everything after the `/model` token, trimmed by the
/// caller. An empty `args` triggers the usage hint and does NOT
/// send anything through the channel.
pub fn handle_model_command(
    args: &str,
    model_tx: &mpsc::UnboundedSender<String>,
    _state: &AppState,
) -> String {
    let name = args.trim();
    if name.is_empty() {
        return USAGE.to_string();
    }

    // Validate by attempting to construct the adapter. The factory
    // is permissive (falls through to OpenAI-compat for unknowns),
    // but constructing it here proves the executor will be able to
    // do the same. If `adapter_for` ever panics on bad input, this
    // would catch it before we send to the channel.
    let _probe = adapters::adapter_for(name, "http://localhost:11434", None);
    // Drop the probe immediately — the executor will construct a
    // fresh one with the real `ollama_host` from config.
    drop(_probe);

    match model_tx.send(name.to_string()) {
        Ok(()) => format!("Switching to {}…", name),
        Err(_) => "Executor is not running; cannot switch model.".to_string(),
    }
}

const USAGE: &str = r#"Usage: /model <name>

Switches the active model for the rest of the session (or until
you `/model` again). The executor's smart-router suggestions are
bypassed — your choice is authoritative.

Examples:
  /model qwen2.5:3b
  /model deepseek-v4-pro:cloud
  /model my-custom-model:latest

The confirmation "🔀 Switched to <name>" appears in the chat when
the swap completes."#;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shared::Config;

    fn dummy_state() -> AppState {
        AppState::new(Config::default())
    }

    /// Empty args → usage hint, no send.
    #[test]
    fn test_empty_args_returns_usage() {
        let (tx, mut rx) = mpsc::unbounded_channel::<String>();
        let state = dummy_state();
        let out = handle_model_command("", &tx, &state);
        assert!(out.starts_with("Usage"), "got: {}", out);
        assert!(out.contains("/model"), "got: {}", out);
        // No message on the channel.
        assert!(rx.try_recv().is_err());
    }

    /// Whitespace-only args → usage hint, no send.
    #[test]
    fn test_whitespace_args_returns_usage() {
        let (tx, mut rx) = mpsc::unbounded_channel::<String>();
        let state = dummy_state();
        let out = handle_model_command("   \t  ", &tx, &state);
        assert!(out.starts_with("Usage"), "got: {}", out);
        assert!(rx.try_recv().is_err());
    }

    /// Non-empty args → "Switching to <name>…" and the name lands on
    /// the channel for the executor to consume.
    #[test]
    fn test_named_args_sends_to_channel() {
        let (tx, mut rx) = mpsc::unbounded_channel::<String>();
        let state = dummy_state();
        let out = handle_model_command("qwen2.5:3b", &tx, &state);
        assert_eq!(out, "Switching to qwen2.5:3b…");
        let received = rx.try_recv().expect("channel should have a value");
        assert_eq!(received, "qwen2.5:3b");
    }

    /// The name is forwarded verbatim (no normalisation, no
    /// lowercasing). The executor's `adapter_for` does the
    /// routing; the handler is a pass-through.
    #[test]
    fn test_named_args_preserves_case() {
        let (tx, mut rx) = mpsc::unbounded_channel::<String>();
        let state = dummy_state();
        let out = handle_model_command("GPT-OSS-120B", &tx, &state);
        assert!(out.contains("GPT-OSS-120B"), "got: {}", out);
        let received = rx.try_recv().expect("channel should have a value");
        assert_eq!(received, "GPT-OSS-120B");
    }

    /// Channel closed → graceful "executor not running" message.
    /// We simulate this by dropping the receiver before the call.
    #[test]
    fn test_closed_channel_returns_graceful_error() {
        let (tx, rx) = mpsc::unbounded_channel::<String>();
        drop(rx);
        let state = dummy_state();
        let out = handle_model_command("qwen2.5:3b", &tx, &state);
        assert!(out.contains("Executor"), "got: {}", out);
        assert!(out.contains("not running"), "got: {}", out);
    }
}
