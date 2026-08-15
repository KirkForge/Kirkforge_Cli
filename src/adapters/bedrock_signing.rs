//! AWS SigV4 signing for Anthropic-on-Bedrock requests.
//!
//! We avoid pulling in the full AWS SDK by using `aws-sigv4` directly. The
//! signing process builds a canonical request, hashes the payload, and
//! produces the `Authorization` header plus any required session headers.
//!
//! Credentials are resolved from:
//! 1. `profile` if non-empty (via `aws_config` profile chain; not implemented in
//!    this MVP — falls through).
//! 2. `AWS_ACCESS_KEY_ID` / `AWS_SECRET_ACCESS_KEY` / `AWS_SESSION_TOKEN`.
//! 3. EC2/ECS/SSO instance metadata (not implemented in this MVP).
//!
//! The MVP resolves env-only credentials; profile/instance support is a
//! documented extension point.

use anyhow::Context;
use aws_credential_types::Credentials as AwsCredentials;
use aws_sigv4::http_request::{sign as sigv4_sign, SignableBody, SignableRequest, SigningSettings};
use aws_sigv4::sign::v4;
use aws_smithy_runtime_api::client::identity::Identity;
use std::time::SystemTime;

/// A fully signed HTTP request ready for `reqwest`.
pub struct SignedRequest {
    pub method: reqwest::Method,
    pub url: String,
    pub headers: reqwest::header::HeaderMap,
}

/// Sign a Bedrock InvokeModelWithResponseStream request.
pub fn sign_request(url: &str, body: &[u8], region: &str) -> anyhow::Result<SignedRequest> {
    let creds = resolve_credentials()?;
    let session_token = creds.session_token().map(|s| s.to_string());
    let identity: Identity = creds.into();
    let signing_params = v4::SigningParams::builder()
        .identity(&identity)
        .region(region)
        .name("bedrock")
        .time(SystemTime::now())
        .settings(SigningSettings::default())
        .build()
        .context("failed to build signing params")?;

    let mut request_builder = http::Request::builder()
        .method(http::Method::POST)
        .uri(url)
        .header("host", host_header(url)?)
        .header("content-type", "application/json")
        .header("x-amz-content-sha256", sha256_hex(body));

    if let Some(token) = session_token {
        request_builder = request_builder.header("x-amz-security-token", token);
    }

    let request = request_builder
        .body(body.to_vec())
        .context("failed to build signable request")?;

    let signing_params: aws_sigv4::http_request::SigningParams<'_> =
        aws_sigv4::http_request::SigningParams::V4(signing_params);

    let signing_output = sigv4_sign(
        SignableRequest::new(
            "POST",
            url,
            request
                .headers()
                .iter()
                .map(|(k, v)| (k.as_str(), v.to_str().unwrap_or(""))),
            SignableBody::Bytes(body),
        )
        .context("invalid signable request")?,
        &signing_params,
    )
    .context("signing failed")?;
    let signing_instructions = signing_output.output();

    let mut headers = reqwest::header::HeaderMap::new();
    for (key, value) in request.headers() {
        let name = reqwest::header::HeaderName::from_bytes(key.as_ref())
            .with_context(|| format!("invalid header name from SigV4 request: {key:?}"))?;
        let v_str = value
            .to_str()
            .with_context(|| format!("non-ASCII value for SigV4 header {key:?}"))?;
        let v = reqwest::header::HeaderValue::from_str(v_str)
            .with_context(|| format!("non-ASCII value for SigV4 header {key:?}"))?;
        headers.insert(name, v);
    }
    for (key, value) in signing_instructions.headers() {
        let name = reqwest::header::HeaderName::from_bytes(key.as_ref())
            .with_context(|| format!("invalid header name from signing instructions: {key:?}"))?;
        let v = reqwest::header::HeaderValue::from_str(value)
            .with_context(|| format!("non-ASCII value for signing header {key:?}"))?;
        headers.insert(name, v);
    }

    Ok(SignedRequest {
        method: reqwest::Method::POST,
        url: url.to_string(),
        headers,
    })
}

fn host_header(url: &str) -> anyhow::Result<String> {
    url.parse::<url::Url>()
        .context("invalid URL")?
        .host_str()
        .map(|h| h.to_string())
        .context("URL has no host")
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex::encode(hasher.finalize())
}

fn resolve_credentials() -> anyhow::Result<AwsCredentials> {
    let access_key = std::env::var("AWS_ACCESS_KEY_ID").context("AWS_ACCESS_KEY_ID not set")?;
    let secret_key =
        std::env::var("AWS_SECRET_ACCESS_KEY").context("AWS_SECRET_ACCESS_KEY not set")?;
    let session_token = std::env::var("AWS_SESSION_TOKEN").ok();
    Ok(AwsCredentials::new(
        access_key,
        secret_key,
        session_token,
        None,
        "env",
    ))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    // Shared crate-wide lock for tests that mutate the AWS env vars, so the
    // bedrock_vertex_mocks wiremock tests and the offline signing tests don't
    // race on AWS_ACCESS_KEY_ID / AWS_SECRET_ACCESS_KEY.
    pub(crate) fn env_lock() -> &'static std::sync::Mutex<()> {
        static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
        LOCK.get_or_init(|| std::sync::Mutex::new(()))
    }

    #[test]
    fn host_header_extracts_hostname() {
        assert_eq!(
            host_header("https://bedrock-runtime.us-east-1.amazonaws.com/").unwrap(),
            "bedrock-runtime.us-east-1.amazonaws.com"
        );
    }

    #[test]
    fn sha256_hex_is_stable() {
        let a = sha256_hex(b"hello");
        let b = sha256_hex(b"hello");
        let c = sha256_hex(b"world");
        assert_eq!(a, b);
        assert_ne!(a, c);
    }

    #[test]
    fn sha256_hex_empty_input() {
        let h = sha256_hex(b"");
        assert_eq!(
            h,
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn sha256_hex_known_value() {
        let h = sha256_hex(b"hello");
        assert_eq!(
            h,
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        );
    }

    #[test]
    fn host_header_extracts_hostname_from_path() {
        assert_eq!(
            host_header("https://bedrock-runtime.us-west-2.amazonaws.com/model/x").unwrap(),
            "bedrock-runtime.us-west-2.amazonaws.com"
        );
    }

    #[test]
    fn host_header_fails_for_invalid_url() {
        assert!(host_header("not a url").is_err());
    }

    #[test]
    fn host_header_fails_for_url_without_host() {
        assert!(host_header("file:///local/path").is_err());
    }

    #[test]
    fn resolve_credentials_reads_from_env() {
        let _guard = env_lock().lock().unwrap();
        let _g1 = crate::shared::test_util::EnvGuard::set("AWS_ACCESS_KEY_ID", "AKIATEST");
        let _g2 = crate::shared::test_util::EnvGuard::set("AWS_SECRET_ACCESS_KEY", "secretkey");
        let _g3 = crate::shared::test_util::EnvGuard::set("AWS_SESSION_TOKEN", "sessiontoken");
        let creds = resolve_credentials().unwrap();
        assert_eq!(creds.access_key_id(), "AKIATEST");
        assert_eq!(creds.secret_access_key(), "secretkey");
    }

    #[test]
    fn resolve_credentials_fails_without_access_key() {
        let _guard = env_lock().lock().unwrap();
        let _g = crate::shared::test_util::EnvGuard::remove("AWS_ACCESS_KEY_ID");
        assert!(resolve_credentials().is_err());
    }

    #[test]
    fn resolve_credentials_fails_without_secret_key() {
        let _guard = env_lock().lock().unwrap();
        let _g1 = crate::shared::test_util::EnvGuard::set("AWS_ACCESS_KEY_ID", "AKIATEST");
        let _g2 = crate::shared::test_util::EnvGuard::remove("AWS_SECRET_ACCESS_KEY");
        assert!(resolve_credentials().is_err());
    }

    #[test]
    fn sign_request_produces_authorization_header() {
        let _guard = env_lock().lock().unwrap();
        let _g1 = crate::shared::test_util::EnvGuard::set("AWS_ACCESS_KEY_ID", "AKIATEST");
        let _g2 = crate::shared::test_util::EnvGuard::set("AWS_SECRET_ACCESS_KEY", "secretkey");
        let _g3 = crate::shared::test_util::EnvGuard::remove("AWS_SESSION_TOKEN");
        let url = "https://bedrock-runtime.us-east-1.amazonaws.com/model/test/invoke-with-response-stream";
        let body = b"{\"hello\":\"world\"}";
        let signed = sign_request(url, body, "us-east-1").unwrap();
        assert_eq!(signed.method, reqwest::Method::POST);
        assert_eq!(signed.url, url);
        assert!(signed.headers.contains_key("authorization"));
        let auth_val = signed
            .headers
            .get("authorization")
            .unwrap()
            .to_str()
            .unwrap();
        assert!(
            auth_val.starts_with("AWS4-HMAC-SHA256"),
            "authorization header must start with AWS4-HMAC-SHA256, got: {auth_val}"
        );
        assert!(signed.headers.contains_key("x-amz-content-sha256"));
        assert!(signed.headers.contains_key("host"));
        assert!(signed.headers.contains_key("x-amz-date"));
    }

    #[test]
    fn sign_request_includes_security_token_when_set() {
        let _guard = env_lock().lock().unwrap();
        let _g1 = crate::shared::test_util::EnvGuard::set("AWS_ACCESS_KEY_ID", "AKIATEST");
        let _g2 = crate::shared::test_util::EnvGuard::set("AWS_SECRET_ACCESS_KEY", "secretkey");
        let _g3 = crate::shared::test_util::EnvGuard::set("AWS_SESSION_TOKEN", "mysessiontoken");
        let url =
            "https://bedrock-runtime.us-east-1.amazonaws.com/model/x/invoke-with-response-stream";
        let signed = sign_request(url, b"{}", "us-east-1").unwrap();
        assert!(signed.headers.contains_key("x-amz-security-token"));
    }

    #[test]
    fn sign_request_fails_for_invalid_url() {
        let _guard = env_lock().lock().unwrap();
        let _g1 = crate::shared::test_util::EnvGuard::set("AWS_ACCESS_KEY_ID", "AKIATEST");
        let _g2 = crate::shared::test_util::EnvGuard::set("AWS_SECRET_ACCESS_KEY", "secretkey");
        assert!(sign_request("not a url", b"{}", "us-east-1").is_err());
    }
}
