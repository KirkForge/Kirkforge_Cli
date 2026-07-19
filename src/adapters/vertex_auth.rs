//! GCP service-account token fetcher for Anthropic-on-Vertex requests.
//!
//! Uses `yup-oauth2` with a service-account JSON key to obtain an access token
//! scoped to `https://www.googleapis.com/auth/cloud-platform`. If no service
//! account path is configured, the standard `GOOGLE_APPLICATION_CREDENTIALS`
//! environment variable is tried.
//!
//! This module is intentionally small: the full ADC (Application Default
//! Credentials) flow is an extension point for a future iteration.

use anyhow::Context;
use std::path::Path;

/// Request an access token for the configured service account.
///
/// `service_account_path` is the user-configured path; if `None`, the
/// `GOOGLE_APPLICATION_CREDENTIALS` environment variable is used.
pub async fn service_account_token(
    service_account_path: Option<&std::path::Path>,
    scopes: &[&str],
) -> anyhow::Result<String> {
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
    Ok(token.token().unwrap_or_default().to_string())
}

/// Verify that a service-account key file is readable JSON.
pub fn key_file_looks_valid(path: &Path) -> bool {
    if let Ok(content) = std::fs::read_to_string(path) {
        serde_json::from_str::<serde_json::Value>(&content).is_ok()
    } else {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn key_file_looks_valid_accepts_real_json() {
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        tmp.write_all(b"{\"type\":\"service_account\"}").unwrap();
        assert!(key_file_looks_valid(tmp.path()));
    }

    #[test]
    fn key_file_looks_valid_rejects_garbage() {
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        tmp.write_all(b"not json").unwrap();
        assert!(!key_file_looks_valid(tmp.path()));
    }
}
