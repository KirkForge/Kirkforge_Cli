//! `/compact` slash-command handler.
//!
//! The actual compaction work happens asynchronously in the executor
//! (it calls `PromptBuilder::compact_to_budget` and rewrites the NDJSON
//! log atomically). We just kick it off by sending a `CompactRequest` on
//! `compact_tx` and return a status string immediately. When the executor
//! finishes, it emits `TurnEvent::CompactionReport`, which the TUI event
//! loop consumes to rebuild the display list and append a 🧹 status message.

use crate::session::prompt::CompactRequest;
use tokio::sync::mpsc;

/// Handle `/compact` command: trigger a user-driven compaction of the
/// conversation history.
///
/// Supports an optional `keep=N` argument to override
/// `Config::preserve_recent_messages` for this compaction only:
///
///   /compact
///   /compact keep=4
///   /compact keep=1
///
/// Returns a user-facing status string immediately; the executor reports
/// detailed before/after stats via `TurnEvent::CompactionReport`.
pub async fn handle_compact_command(
    args: &str,
    compact_tx: &mpsc::UnboundedSender<CompactRequest>,
) -> String {
    let keep = match parse_compact_args(args) {
        Ok(k) => k,
        Err(e) => return e,
    };

    match compact_tx.send(CompactRequest { keep }) {
        Ok(()) => {
            let base = "🧹 Compaction requested.".to_string();
            match keep {
                Some(n) => format!("{base} Preserving the last {n} message(s) verbatim."),
                None => base,
            }
        }
        Err(e) => format!("❌ Failed to send compact request to executor: {e}"),
    }
}

/// Parse `/compact` arguments.
///
/// Empty input is allowed. The only recognized form is `keep=N` where
/// `N` is a positive integer. Invalid input yields a usage error string.
fn parse_compact_args(args: &str) -> Result<Option<usize>, String> {
    let trimmed = args.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }

    // Accept either "keep=N" or just "N" for convenience.
    let value_part = if let Some(eq_pos) = trimmed.find('=') {
        let (key, value) = trimmed.split_at(eq_pos);
        let key = key.trim();
        if key != "keep" {
            return Err(compact_usage());
        }
        value.strip_prefix('=').unwrap_or(value).trim()
    } else {
        trimmed
    };

    match value_part.parse::<usize>() {
        Ok(0) => Err("keep must be at least 1".to_string()),
        Ok(n) => Ok(Some(n)),
        Err(_) => Err(compact_usage()),
    }
}

fn compact_usage() -> String {
    "Usage: /compact [keep=N]\n\nTrigger a user-driven compaction of the conversation history.\nOptionally override how many recent messages are kept verbatim.".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn empty_args_sends_none_keep() {
        let (tx, mut rx) = mpsc::unbounded_channel::<CompactRequest>();
        let out = handle_compact_command("", &tx).await;
        assert!(out.starts_with("🧹 Compaction requested"), "got: {out}");
        let req = rx.try_recv().expect("request should be sent");
        assert!(req.keep.is_none());
    }

    #[tokio::test]
    async fn keep_arg_parses_and_sends() {
        let (tx, mut rx) = mpsc::unbounded_channel::<CompactRequest>();
        let out = handle_compact_command("keep=4", &tx).await;
        assert!(out.contains("4"), "got: {out}");
        let req = rx.try_recv().expect("request should be sent");
        assert_eq!(req.keep, Some(4));
    }

    #[tokio::test]
    async fn bare_number_treated_as_keep() {
        let (tx, mut rx) = mpsc::unbounded_channel::<CompactRequest>();
        let out = handle_compact_command("2", &tx).await;
        assert!(out.contains("2"), "got: {out}");
        let req = rx.try_recv().expect("request should be sent");
        assert_eq!(req.keep, Some(2));
    }

    #[tokio::test]
    async fn keep_zero_rejected() {
        let (tx, _rx) = mpsc::unbounded_channel::<CompactRequest>();
        let out = handle_compact_command("keep=0", &tx).await;
        assert!(
            out.starts_with("Usage") || out.contains("at least 1"),
            "got: {out}"
        );
    }

    #[tokio::test]
    async fn invalid_arg_returns_usage() {
        let (tx, _rx) = mpsc::unbounded_channel::<CompactRequest>();
        let out = handle_compact_command("foo", &tx).await;
        assert!(out.starts_with("Usage"), "got: {out}");
    }

    #[tokio::test]
    async fn closed_channel_returns_error() {
        let (tx, rx) = mpsc::unbounded_channel::<CompactRequest>();
        drop(rx);
        let out = handle_compact_command("", &tx).await;
        assert!(out.contains("Failed to send"), "got: {out}");
    }
}
