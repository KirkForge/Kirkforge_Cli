//! GCP service-account token fetcher for Anthropic-on-Vertex requests.
//!
//! Uses `yup-oauth2` with a service-account JSON key to obtain an access token
//! scoped to `https://www.googleapis.com/auth/cloud-platform`. If no service
//! account path is configured, the standard `GOOGLE_APPLICATION_CREDENTIALS`
//! environment variable is tried.
//!
//! This module is intentionally small: the full ADC (Application Default
//! Credentials) flow is an extension point for a future iteration.
//!
//! ponytail: unit-testing the `Authorization: Bearer <token>` header
//! construction is not possible without mocking `yup_oauth2`'s async
//! authenticator — the token fetch hits Google's OAuth server. The
//! `anthropic_vertex.rs` integration test validates the header format
//! end-to-end when credentials are present. Upgrade path: inject an
//! `Authenticator` trait so tests can provide a fake token.

use anyhow::Context;

/// Request an access token for the configured service account.
///
/// `service_account_path` is the user-configured path; if `None`, the
/// `GOOGLE_APPLICATION_CREDENTIALS` environment variable is used.
/// Returns the full `AccessToken` so callers can consult `is_expired()`
/// (which carries a 1-minute safety margin) instead of re-fetching per
/// request.
pub async fn service_account_token(
    service_account_path: Option<&std::path::Path>,
    scopes: &[&str],
) -> anyhow::Result<yup_oauth2::AccessToken> {
    let path = service_account_path
        .map(|p| p.to_path_buf())
        .or_else(|| {
            std::env::var("GOOGLE_APPLICATION_CREDENTIALS")
                .ok()
                .map(std::path::PathBuf::from)
        })
        .context(
            "no GCP service account configured; set gcp_service_account_path or GOOGLE_APPLICATION_CREDENTIALS",
        )?;

    let key = yup_oauth2::read_service_account_key(&path)
        .await
        .context("failed to read GCP service-account key")?;
    let auth = yup_oauth2::ServiceAccountAuthenticator::builder(key)
        .build()
        .await
        .context("failed to build GCP service-account authenticator")?;
    let token = auth
        .token(scopes)
        .await
        .context("failed to fetch GCP access token")?;
    Ok(token)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shared::test_util::EnvGuard;

    #[tokio::test]
    async fn service_account_token_fails_without_path_or_env() {
        let _env = EnvGuard::remove("GOOGLE_APPLICATION_CREDENTIALS");
        let result =
            service_account_token(None, &["https://www.googleapis.com/auth/cloud-platform"]).await;
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("no GCP service account configured"));
    }

    #[tokio::test]
    async fn service_account_token_fails_with_nonexistent_explicit_path() {
        let path = std::path::PathBuf::from("/tmp/definitely-no-such-key-file.json");
        let result = service_account_token(
            Some(&path),
            &["https://www.googleapis.com/auth/cloud-platform"],
        )
        .await;
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("failed to read GCP service-account key"));
    }
}
