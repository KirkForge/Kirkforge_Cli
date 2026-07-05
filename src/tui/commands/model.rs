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
//! 3. This handler validates the model when it is routed through an
//!    Ollama endpoint, then sends the name through `model_tx` to the
//!    executor driver (see `Executor::run`'s `tokio::select!` arm).
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
//! - **OpenAI-compatible models**: validation is skipped. The name
//!   is forwarded to the executor just as before.
//! - **Ollama-routed models**: we query `OLLAMA_HOST/api/tags` with a
//!   2s timeout. If the model is not present locally we refuse to
//!   switch and suggest similar names. If the check itself fails
//!   (transport/parse/timeout) we still switch but warn the user.

use crate::adapters::{self, AdapterKind};
use crate::session::executor::TurnEvent;
use crate::shared::read_shared_config;
use crate::tui::app::AppState;
use tokio::sync::mpsc;
use tokio_stream::StreamExt;

/// Result of validating a requested model against the Ollama `/api/tags`
/// local model list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelValidation {
    /// Model name appears in the local list.
    Valid,
    /// Model name not found; `similar` holds up to 5 closest names.
    NotFound { similar: Vec<String> },
    /// Model is routed through the OpenAI-compatible path; no local check.
    SkippedOpenAiCompat,
    /// Transport, timeout, or parse error. The caller may still proceed
    /// but should warn the user.
    CheckFailed(String),
}

/// Handle `/model <name>`. Returns a user-facing status string.
///
/// `args` is everything after the `/model` token, trimmed by the
/// caller. An empty `args` triggers the usage hint and does NOT
/// send anything through the channel.
pub async fn handle_model_command(
    args: &str,
    model_tx: &mpsc::UnboundedSender<String>,
    event_tx: &mpsc::UnboundedSender<TurnEvent>,
    state: &AppState,
) -> String {
    let name = args.trim();
    if name.is_empty() {
        return USAGE.to_string();
    }

    match adapter_kind_for_model(name) {
        AdapterKind::OpenAiCompat => {
            // OpenAI-compatible endpoints may be remote/cloud APIs;
            // a local `/api/tags` check would be meaningless here.
            let _validation = ModelValidation::SkippedOpenAiCompat;
            match model_tx.send(name.to_string()) {
                Ok(()) => format!("Switching to {name}…"),
                Err(_) => "Executor is not running; cannot switch model.".to_string(),
            }
        }
        AdapterKind::Ollama => {
            let client = reqwest::Client::new();
            let ollama_host = read_shared_config(&state.config).ollama_host.clone();
            let validation = validate_ollama_model(&client, &ollama_host, name).await;
            match validation {
                ModelValidation::Valid => match model_tx.send(name.to_string()) {
                    Ok(()) => format!("Switching to {name}…"),
                    Err(_) => "Executor is not running; cannot switch model.".to_string(),
                },
                ModelValidation::NotFound { similar } => {
                    // The model is missing locally. Instead of refusing,
                    // start an Ollama `/api/pull` in the background and
                    // stream progress into the chat. Once the pull finishes
                    // successfully we ask the executor to switch to it.
                    let host = ollama_host.clone();
                    let model = name.to_string();
                    let event_tx = event_tx.clone();
                    let switch_tx = model_tx.clone();
                    tokio::spawn(async move {
                        run_ollama_pull(&host, &model, &event_tx, &switch_tx).await;
                    });
                    let mut msg = format!(
                        "Model '{name}' is not present locally. Starting Ollama pull in the background; progress will appear in the chat. Once complete, the session will switch to it automatically."
                    );
                    if !similar.is_empty() {
                        msg.push_str("\nDid you mean:\n");
                        for s in similar {
                            msg.push_str(&format!("  - {s}\n"));
                        }
                        msg.pop(); // remove trailing newline
                    }
                    msg
                }
                ModelValidation::SkippedOpenAiCompat => {
                    // Defensive: should not happen for an Ollama kind.
                    match model_tx.send(name.to_string()) {
                        Ok(()) => format!("Switching to {name}…"),
                        Err(_) => "Executor is not running; cannot switch model.".to_string(),
                    }
                }
                ModelValidation::CheckFailed(err) => {
                    let base = match model_tx.send(name.to_string()) {
                        Ok(()) => format!("Switching to {name}…"),
                        Err(_) => "Executor is not running; cannot switch model.".to_string(),
                    };
                    format!("{base}\nWarning: could not validate model availability: {err}")
                }
            }
        }
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

/// Parse the model names out of an Ollama `/api/tags` JSON body.
///
/// Returns `models[].name` values in order; an unparseable body
/// yields an empty list.
pub fn parse_model_list(body: &str) -> Vec<String> {
    let mut names = Vec::new();
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(body) {
        if let Some(models) = value.get("models").and_then(|m| m.as_array()) {
            for model in models {
                if let Some(name) = model.get("name").and_then(|n| n.as_str()) {
                    names.push(name.to_string());
                }
            }
        }
    }
    names
}

/// Hand-rolled Levenshtein distance between two strings.
///
/// We deliberately don't pull in a string-similarity crate just for
/// this ~30-line helper — the dependency cost outweighs the benefit
/// for a single suggestion ranking used only in `/model` validation.
fn levenshtein(a: &str, b: &str) -> usize {
    let a_len = a.chars().count();
    let b_len = b.chars().count();

    if a_len == 0 {
        return b_len;
    }
    if b_len == 0 {
        return a_len;
    }

    let mut prev = vec![0; b_len + 1];
    let mut curr = vec![0; b_len + 1];

    for (j, item) in prev.iter_mut().enumerate().take(b_len + 1) {
        *item = j;
    }

    for (i, a_ch) in a.chars().enumerate() {
        curr[0] = i + 1;
        for (j, b_ch) in b.chars().enumerate() {
            let cost = if a_ch == b_ch { 0 } else { 1 };
            curr[j + 1] = (curr[j] + 1).min(prev[j + 1] + 1).min(prev[j] + cost);
        }
        std::mem::swap(&mut prev, &mut curr);
    }

    prev[b_len]
}

/// Return up to 5 closest model names from `available` for `name`.
pub fn similar_models(name: &str, available: &[String]) -> Vec<String> {
    let mut scored: Vec<(usize, String)> = available
        .iter()
        .map(|m| (levenshtein(name, m), m.clone()))
        .collect();
    scored.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
    scored.into_iter().take(5).map(|(_, m)| m).collect()
}

/// Validate `name` against `available` and produce a `ModelValidation`.
pub fn validate_model_against_list(name: &str, available: &[String]) -> ModelValidation {
    if available.iter().any(|m| m == name) {
        ModelValidation::Valid
    } else {
        ModelValidation::NotFound {
            similar: similar_models(name, available),
        }
    }
}

/// Classify a model name into an `AdapterKind` (no override).
pub fn adapter_kind_for_model(name: &str) -> AdapterKind {
    adapters::adapter_kind_for(name, None)
}

/// Fetch the local model list from an Ollama host.
pub async fn fetch_model_list(
    client: &reqwest::Client,
    ollama_host: &str,
) -> anyhow::Result<Vec<String>> {
    let url = format!("{}/api/tags", ollama_host.trim_end_matches('/'));
    let body = client
        .get(&url)
        .send()
        .await?
        .error_for_status()?
        .text()
        .await?;
    Ok(parse_model_list(&body))
}

/// Validate an Ollama-routed model name against the host's local list.
///
/// The `/api/tags` call is capped at 2 seconds so a slow/unreachable
/// host does not block the UI.
pub async fn validate_ollama_model(
    client: &reqwest::Client,
    ollama_host: &str,
    name: &str,
) -> ModelValidation {
    let list = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        fetch_model_list(client, ollama_host),
    )
    .await;

    match list {
        Ok(Ok(models)) => validate_model_against_list(name, &models),
        Ok(Err(e)) => ModelValidation::CheckFailed(e.to_string()),
        Err(_) => ModelValidation::CheckFailed("validation timed out".to_string()),
    }
}

/// A single NDJSON line from Ollama's `/api/pull` stream.
#[derive(Debug, Clone, Default)]
struct PullLine {
    status: String,
    completed: Option<u64>,
    total: Option<u64>,
}

/// Parse one `/api/pull` NDJSON line. Returns `None` when the line is empty
/// or lacks a status field.
fn parse_pull_line(line: &str) -> Option<PullLine> {
    let value: serde_json::Value = serde_json::from_str(line).ok()?;
    let status = value.get("status")?.as_str()?.to_string();
    let completed = value.get("completed").and_then(|v| v.as_u64());
    let total = value.get("total").and_then(|v| v.as_u64());
    Some(PullLine {
        status,
        completed,
        total,
    })
}

/// Convert a byte count into a compact human-readable string.
fn human_bytes(bytes: u64) -> String {
    const UNITS: &[&str] = &["B", "KB", "MB", "GB", "TB"];
    let mut size = bytes as f64;
    let mut unit = UNITS[0];
    for u in UNITS.iter().take(4) {
        unit = u;
        if size < 1024.0 {
            break;
        }
        size /= 1024.0;
    }
    format!("{size:.1} {unit}")
}

/// Format a `PullLine` into a one-line user-facing progress message.
fn format_pull_line(model: &str, line: &PullLine) -> String {
    if let (Some(completed), Some(total)) = (line.completed, line.total) {
        let pct = completed
            .checked_mul(100)
            .and_then(|v| v.checked_div(total))
            .unwrap_or(0)
            .min(100);
        format!(
            "📥 Pulling {model}: {} ({} / {}, {}%)",
            line.status,
            human_bytes(completed),
            human_bytes(total),
            pct
        )
    } else {
        format!("📥 Pulling {model}: {}", line.status)
    }
}

/// Run an Ollama `/api/pull` stream, posting progress to the TUI as
/// `TurnEvent::Token` messages and switching models when the pull
/// completes successfully. Errors are also surfaced as tokens.
pub async fn run_ollama_pull(
    ollama_host: &str,
    model: &str,
    event_tx: &mpsc::UnboundedSender<TurnEvent>,
    switch_tx: &mpsc::UnboundedSender<String>,
) {
    let url = format!("{}/api/pull", ollama_host.trim_end_matches('/'));
    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(300))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            let _ = event_tx.send(TurnEvent::Token(format!(
                "❌ Could not start pull for {model}: {e}"
            )));
            return;
        }
    };

    let body = serde_json::json!({"model": model, "stream": true});
    let response = match client.post(&url).json(&body).send().await {
        Ok(resp) => resp,
        Err(e) => {
            let _ = event_tx.send(TurnEvent::Token(format!(
                "❌ Pull request for {model} failed: {e}"
            )));
            return;
        }
    };

    let status = response.status();
    if !status.is_success() {
        let _ = event_tx.send(TurnEvent::Token(format!(
            "❌ Pull request for {model} returned HTTP {}",
            status.as_u16()
        )));
        return;
    }

    let mut stream = response.bytes_stream();
    let mut buffer = Vec::<u8>::new();

    while let Some(chunk) = stream.next().await {
        let chunk = match chunk {
            Ok(c) => c,
            Err(e) => {
                let _ = event_tx.send(TurnEvent::Token(format!(
                    "⚠️ Pull stream for {model} interrupted: {e}"
                )));
                continue;
            }
        };
        buffer.extend_from_slice(&chunk);
        while let Some(pos) = buffer.iter().position(|&b| b == b'\n') {
            let line_bytes: Vec<u8> = buffer.drain(..pos).collect();
            buffer.drain(..1); // drop the newline itself
            let line_bytes = if line_bytes.ends_with(b"\r") {
                line_bytes[..line_bytes.len().saturating_sub(1)].to_vec()
            } else {
                line_bytes
            };
            let line = match std::str::from_utf8(&line_bytes) {
                Ok(s) => s,
                Err(_) => continue,
            };

            if let Some(parsed) = parse_pull_line(line) {
                // Ollama marks completion with status strings like
                // "success" or by repeating the model name; we treat
                // any status containing "success" as done.
                if parsed.status.to_ascii_lowercase().contains("success") {
                    let _ = event_tx.send(TurnEvent::Token(format!(
                        "✅ Pull for {model} complete. Switching now…"
                    )));
                    let _ = switch_tx.send(model.to_string());
                    return;
                }

                let msg = format_pull_line(model, &parsed);
                let _ = event_tx.send(TurnEvent::Token(msg + "\n"));
            }
        }
    }

    // Stream ended without an explicit success line; the pull may still
    // have completed. Try to switch anyway and let the executor surface
    // any remaining problem on the next turn.
    let _ = event_tx.send(TurnEvent::Token(format!(
        "✅ Pull stream for {model} ended. Switching now…"
    )));
    let _ = switch_tx.send(model.to_string());
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shared::Config;

    fn dummy_state() -> AppState {
        AppState::new(std::sync::Arc::new(std::sync::RwLock::new(
            Config::default(),
        )))
    }

    #[test]
    fn parse_pull_line_extracts_status_and_progress() {
        let line = r#"{"status":"pulling manifest","completed":1024,"total":4096}"#;
        let parsed = parse_pull_line(line).expect("valid line");
        assert_eq!(parsed.status, "pulling manifest");
        assert_eq!(parsed.completed, Some(1024));
        assert_eq!(parsed.total, Some(4096));
    }

    #[test]
    fn parse_pull_line_returns_none_for_missing_status() {
        assert!(parse_pull_line(r#"{"completed":100}"#).is_none());
        assert!(parse_pull_line("").is_none());
        assert!(parse_pull_line("not json").is_none());
    }

    #[test]
    fn format_pull_line_includes_percent_and_human_bytes() {
        let line = PullLine {
            status: "pulling layer".to_string(),
            completed: Some(1_500_000),
            total: Some(3_000_000),
        };
        let out = format_pull_line("qwen2.5:3b", &line);
        assert!(out.contains("Pulling qwen2.5:3b"), "{out}");
        assert!(out.contains("1.4 MB / 2.9 MB"), "{out}");
        assert!(out.contains("50%"), "{out}");
    }

    #[test]
    fn format_pull_line_without_progress_shows_status_only() {
        let line = PullLine {
            status: "verifying manifest".to_string(),
            completed: None,
            total: None,
        };
        let out = format_pull_line("llama3.2:latest", &line);
        assert!(out.contains("verifying manifest"), "{out}");
        assert!(!out.contains("/"), "{out}");
    }

    #[test]
    fn human_bytes_scales_units() {
        assert_eq!(human_bytes(512), "512.0 B");
        assert_eq!(human_bytes(2048), "2.0 KB");
        assert_eq!(human_bytes(2_000_000), "1.9 MB");
    }

    /// Empty args → usage hint, no send.
    #[tokio::test]
    async fn test_empty_args_returns_usage() {
        let (tx, mut rx) = mpsc::unbounded_channel::<String>();
        let (_event_tx, _event_rx) = mpsc::unbounded_channel::<TurnEvent>();
        let state = dummy_state();
        let out = handle_model_command("", &tx, &_event_tx, &state).await;
        assert!(out.starts_with("Usage"), "got: {out}");
        assert!(out.contains("/model"), "got: {out}");
        // No message on the channel.
        assert!(rx.try_recv().is_err());
    }

    /// Whitespace-only args → usage hint, no send.
    #[tokio::test]
    async fn test_whitespace_args_returns_usage() {
        let (tx, mut rx) = mpsc::unbounded_channel::<String>();
        let (_event_tx, _event_rx) = mpsc::unbounded_channel::<TurnEvent>();
        let state = dummy_state();
        let out = handle_model_command("   \t  ", &tx, &_event_tx, &state).await;
        assert!(out.starts_with("Usage"), "got: {out}");
        assert!(rx.try_recv().is_err());
    }

    /// Non-empty args → "Switching to <name>…" and the name lands on
    /// the channel for the executor to consume.
    #[tokio::test]
    async fn test_named_args_sends_to_channel() {
        let (tx, mut rx) = mpsc::unbounded_channel::<String>();
        let (_event_tx, _event_rx) = mpsc::unbounded_channel::<TurnEvent>();
        let state = dummy_state();
        let out = handle_model_command("qwen2.5:3b", &tx, &_event_tx, &state).await;
        assert_eq!(out, "Switching to qwen2.5:3b…");
        let received = rx.try_recv().expect("channel should have a value");
        assert_eq!(received, "qwen2.5:3b");
    }

    /// The name is forwarded verbatim (no normalisation, no
    /// lowercasing). The executor's `adapter_for` does the
    /// routing; the handler is a pass-through.
    #[tokio::test]
    async fn test_named_args_preserves_case() {
        let (tx, mut rx) = mpsc::unbounded_channel::<String>();
        let (_event_tx, _event_rx) = mpsc::unbounded_channel::<TurnEvent>();
        let state = dummy_state();
        let out = handle_model_command("GPT-OSS-120B", &tx, &_event_tx, &state).await;
        assert!(out.contains("GPT-OSS-120B"), "got: {out}");
        let received = rx.try_recv().expect("channel should have a value");
        assert_eq!(received, "GPT-OSS-120B");
    }

    /// Channel closed → graceful "executor not running" message.
    /// We simulate this by dropping the receiver before the call.
    #[tokio::test]
    async fn test_closed_channel_returns_graceful_error() {
        let (tx, rx) = mpsc::unbounded_channel::<String>();
        drop(rx);
        let (_event_tx, _event_rx) = mpsc::unbounded_channel::<TurnEvent>();
        let state = dummy_state();
        let out = handle_model_command("qwen2.5:3b", &tx, &_event_tx, &state).await;
        assert!(out.contains("Executor"), "got: {out}");
        assert!(out.contains("not running"), "got: {out}");
    }

    /// `parse_model_list` extracts `models[].name` from `/api/tags` JSON.
    #[test]
    fn test_parse_model_list_extracts_names() {
        let body = serde_json::json!({
            "models": [
                { "name": "qwen2.5:3b", "size": 1234 },
                { "name": "llama3.2:latest" },
                { "model": "missing-name-key" }
            ]
        })
        .to_string();
        assert_eq!(
            parse_model_list(&body),
            vec!["qwen2.5:3b".to_string(), "llama3.2:latest".to_string()]
        );
    }

    #[test]
    fn test_parse_model_list_returns_empty_on_bad_json() {
        assert!(parse_model_list("not json").is_empty());
        assert!(parse_model_list(r#"{"models": "nope"}"#).is_empty());
    }

    /// `similar_models` returns the closest names, up to 5.
    #[test]
    fn test_similar_models_returns_closest() {
        let available = vec![
            "qwen2.5:3b".to_string(),
            "qwen2.5:7b".to_string(),
            "llama3.2:latest".to_string(),
            "llama3.1:latest".to_string(),
            "deepseek-v4-pro:cloud".to_string(),
            "gemini-3-flash-1m".to_string(),
        ];
        let similar = similar_models("qwen2.5:7", &available);
        assert_eq!(
            similar,
            vec![
                "qwen2.5:7b",
                "qwen2.5:3b",
                "llama3.1:latest",
                "llama3.2:latest",
                "gemini-3-flash-1m"
            ]
        );
    }

    #[test]
    fn test_similar_models_caps_at_five() {
        let available: Vec<String> = (0..10).map(|i| format!("model-{i}")).collect();
        assert_eq!(similar_models("model", &available).len(), 5);
    }

    /// `validate_model_against_list` resolves exact matches and
    /// supplies suggestions otherwise.
    #[test]
    fn test_validate_model_against_list() {
        let available = vec!["qwen2.5:3b".to_string(), "llama3.2:latest".to_string()];
        assert_eq!(
            validate_model_against_list("qwen2.5:3b", &available),
            ModelValidation::Valid
        );
        assert_eq!(
            validate_model_against_list("qwen2.5:7b", &available),
            ModelValidation::NotFound {
                similar: vec!["qwen2.5:3b".to_string(), "llama3.2:latest".to_string()]
            }
        );
    }

    /// `adapter_kind_for_model` classifies the built-in special models
    /// as Ollama and everything else as OpenAI-compatible.
    #[test]
    fn test_adapter_kind_for_model() {
        assert_eq!(
            adapter_kind_for_model("qwen2.5:3b"),
            AdapterKind::OpenAiCompat
        );
        assert_eq!(
            adapter_kind_for_model("GPT-OSS-120B"),
            AdapterKind::OpenAiCompat
        );
        assert_eq!(
            adapter_kind_for_model("deepseek-v4-pro:cloud"),
            AdapterKind::Ollama
        );
        assert_eq!(adapter_kind_for_model("glm-5.1:cloud"), AdapterKind::Ollama);
        assert_eq!(
            adapter_kind_for_model("gemini-3-flash-1m"),
            AdapterKind::Ollama
        );
    }
}
