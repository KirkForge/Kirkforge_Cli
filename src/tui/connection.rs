//! Ollama connection probing. Extracted from the parent module so the
//! TUI entry point stays focused on orchestration. The probe hits the
//! configured Ollama host's `/api/tags` endpoint within a short budget
//! and reports a `ConnectionState`; a background task re-probes on an
//! interval and forwards state to the event loop.

use super::app::ConnectionState;
use crate::shared::{read_shared_config, Config, SharedConfig};
use std::time::Instant;

/// One-shot probe of the configured Ollama endpoint with a caller-chosen
/// timeout.
///
/// Returns `Connected { model, since: now }` if `${ollama_host}/api/tags`
/// responds with 2xx within the budget, `Error(msg)` on transport failure or
/// non-2xx status, and `Disconnected` only if the host string is empty.
pub(super) async fn probe_ollama_connection_with_timeout(
    config: &Config,
    model: &str,
    timeout: std::time::Duration,
) -> ConnectionState {
    let host = config.ollama_host.trim_end_matches('/');
    if host.is_empty() {
        return ConnectionState::Error("empty ollama_host in config".into());
    }
    let url = format!("{host}/api/tags");
    let model = model.to_string();
    let since = Instant::now();

    let client = match reqwest::Client::builder().timeout(timeout).build() {
        Ok(c) => c,
        Err(e) => return ConnectionState::Error(format!("client build failed: {e}")),
    };

    match client.get(&url).send().await {
        Ok(resp) if resp.status().is_success() => ConnectionState::Connected { model, since },
        Ok(resp) => ConnectionState::Error(format!("{}: HTTP {}", url, resp.status().as_u16())),
        Err(e) => ConnectionState::Error(format!("{url}: {e}")),
    }
}

/// Startup probe with a generous 2-second budget.
pub(super) async fn probe_ollama_connection(config: &Config, model: &str) -> ConnectionState {
    probe_ollama_connection_with_timeout(config, model, std::time::Duration::from_secs(2)).await
}

/// Background task that probes the Ollama endpoint every `interval` and
/// reports the resulting `ConnectionState` back to the TUI event loop. The
/// probe uses a short timeout so a flaky/unreachable host does not block
/// the task for long.
pub(super) async fn connection_probe_task(
    config: SharedConfig,
    tx: tokio::sync::mpsc::Sender<ConnectionState>,
    interval: std::time::Duration,
) {
    let mut tick = tokio::time::interval(interval);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tick.tick().await;
        let cfg = {
            let guard = read_shared_config(&config);
            guard.clone()
        };
        let state = probe_ollama_connection_with_timeout(
            &cfg,
            &cfg.default_model,
            std::time::Duration::from_secs(1),
        )
        .await;
        if tx.send(state).await.is_err() {
            // TUI loop has shut down; stop probing.
            break;
        }
    }
}
