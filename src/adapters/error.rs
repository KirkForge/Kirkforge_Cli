// Typed adapter failure categories (WO 43.1).
//
// Adapters return bare `anyhow::Error` from `stream()`, so the process-exit
// classifier in `src/main/error.rs` historically re-derived the failure mode
// from Display strings ("connection refused", "model not found", ...) —
// fragile against provider phrasing changes. These typed variants let an
// adapter tag its failure at the source so the classifier can downcast
// instead of string-matching. All five adapters (ollama, openai_compat,
// anthropic, anthropic_bedrock, anthropic_vertex) now wrap their stream()
// transport errors via classify_transport_error. The string-probe fallback
// remains only for the session-layer sandbox/path-policy denials, whose
// producers are not yet typed.

use thiserror::Error;

/// Typed failure categories a model adapter can surface.
///
/// Each variant wraps the original `anyhow::Error` as its `#[source]` so the
/// full error chain is preserved when displayed, while the variant tag lets
/// the process-exit classifier route by type instead of by message text.
///
/// `Other` is the "I couldn't classify it" bucket: it is re-raised as bare
/// `anyhow` so the string-probe fallback in `error.rs` still gets a chance
/// to classify it for the categories that have no typed source yet.
#[derive(Debug, Error)]
pub enum AdapterError {
    /// Provider host unreachable: connection refused, DNS failure, timeout.
    #[error("{0:#}")]
    Unreachable(#[source] anyhow::Error),
    /// Provider recognised the request but the named model is absent.
    #[error("{0:#}")]
    ModelNotFound(#[source] anyhow::Error),
    /// Auth or policy denial (401/403, sandbox block, quota gate).
    #[error("{0:#}")]
    Denied(#[source] anyhow::Error),
    /// An adapter failure that doesn't fit the categories above. Re-raised
    /// as bare `anyhow` by [`classify_transport_error`] so the string-probe
    /// fallback in the exit classifier still runs.
    #[error("{0:#}")]
    Other(#[source] anyhow::Error),
}

impl AdapterError {
    /// `true` for the variants that mean "the model can't be reached", i.e.
    /// `Unreachable` and `ModelNotFound`. The exit classifier maps both to
    /// `ModelUnreachable` (exit 3).
    pub fn is_model_unreachable(&self) -> bool {
        matches!(self, Self::Unreachable(_) | Self::ModelNotFound(_))
    }
}

/// Inspect an error raised by a model-adapter `stream()` call and wrap it in
/// the appropriate [`AdapterError`] variant. The returned error is again
/// `anyhow::Error` (so the existing `stream()` signatures are unchanged), but
/// carries the typed `AdapterError` as its root for the exit classifier to
/// downcast.
///
/// Classification rules (applied to the error chain's `reqwest::Error` when
/// present, else to the error message as a fallback):
/// - connect / timeout / DNS error → [`AdapterError::Unreachable`]
/// - HTTP 404 → [`AdapterError::ModelNotFound`]
/// - HTTP 401 / 403 → [`AdapterError::Denied`]
/// - everything else → [`AdapterError::Other`]
///
/// `Other` is unwrapped back to bare `anyhow` so the string-probe fallback in
/// `error.rs` keeps working for unmigrated failure modes during the
/// incremental migration.
pub fn classify_transport_error(e: anyhow::Error) -> anyhow::Error {
    // If the root is already a reqwest::Error, classify by kind/status.
    if let Some(req_err) = e.downcast_ref::<reqwest::Error>() {
        if req_err.is_connect() || req_err.is_timeout() {
            return AdapterError::Unreachable(e).into();
        }
        if let Some(status) = req_err.status() {
            let code = status.as_u16();
            if code == 404 {
                return AdapterError::ModelNotFound(e).into();
            }
            if code == 401 || code == 403 {
                return AdapterError::Denied(e).into();
            }
        }
        // reqwest error we couldn't pin to a category — keep the chain
        // intact via Other so the string-probe fallback still sees the
        // reqwest Display text (e.g. "dns error", "failed to connect").
        return AdapterError::Other(e).into();
    }

    // No typed reqwest root: some call sites wrap reqwest in a context that
    // anyhow flattens. Fall back to the error message for the connect/timeout
    // phrasings the string-probe in error.rs already knows. This keeps
    // ollama's wrapped errors classified as Unreachable even when the
    // reqwest root is hidden behind a context layer.
    let msg = format!("{e:#}").to_lowercase();
    if msg.contains("connection refused")
        || msg.contains("failed to connect")
        || msg.contains("dns error")
        || msg.contains("timed out")
        || msg.contains("connect error")
    {
        return AdapterError::Unreachable(e).into();
    }
    if msg.contains("model not found") {
        return AdapterError::ModelNotFound(e).into();
    }
    // Unmatched: return as bare anyhow (no AdapterError wrapper) so the
    // string-probe fallback in error.rs handles the remaining categories.
    e
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_model_unreachable_covers_unreachable_and_model_not_found() {
        assert!(AdapterError::Unreachable(anyhow::anyhow!("x")).is_model_unreachable());
        assert!(AdapterError::ModelNotFound(anyhow::anyhow!("x")).is_model_unreachable());
        assert!(!AdapterError::Denied(anyhow::anyhow!("x")).is_model_unreachable());
        assert!(!AdapterError::Other(anyhow::anyhow!("x")).is_model_unreachable());
    }

    #[test]
    fn classify_model_not_found_message_is_model_not_found() {
        // Ollama surfaces "model not found" in its error payload; when the
        // reqwest root is hidden behind a context layer, the message
        // fallback must still tag it as ModelNotFound.
        let e = anyhow::anyhow!("model not found: qwen");
        let classified = classify_transport_error(e);
        match classified.downcast_ref::<AdapterError>() {
            Some(AdapterError::ModelNotFound(_)) => {}
            other => panic!("expected ModelNotFound, got {other:?}"),
        }
    }

    #[test]
    fn classify_connection_refused_message_is_unreachable() {
        // reqwest's Display for a connect failure includes "connection
        // refused"; the message fallback covers the case where the typed
        // reqwest root is flattened into an anyhow context.
        let e = anyhow::anyhow!("connection refused (os error 111)");
        let classified = classify_transport_error(e);
        match classified.downcast_ref::<AdapterError>() {
            Some(AdapterError::Unreachable(_)) => {}
            other => panic!("expected Unreachable, got {other:?}"),
        }
    }

    #[test]
    fn classify_unmatched_message_passes_through_unwrapped() {
        // An unmatched message must NOT be wrapped in AdapterError::Other
        // here — it should pass through as bare anyhow so the string-probe
        // fallback in error.rs can still classify it (e.g. "denied").
        let e = anyhow::anyhow!("permission denied");
        let classified = classify_transport_error(e);
        assert!(
            classified.downcast_ref::<AdapterError>().is_none(),
            "unmatched error should stay bare anyhow, got {classified:#?}"
        );
        assert!(classified.to_string().contains("permission denied"));
    }
}
