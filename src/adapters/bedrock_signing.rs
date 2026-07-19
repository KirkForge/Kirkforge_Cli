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
pub fn sign_request(
    url: &str,
    body: &[u8],
    region: &str,
    _profile: &str,
) -> anyhow::Result<SignedRequest> {
    let creds = resolve_credentials()?;
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

    if let Some(token) = session_token() {
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
        if let Ok(name) = reqwest::header::HeaderName::from_bytes(key.as_ref()) {
            if let Ok(v) = reqwest::header::HeaderValue::from_str(value.to_str().unwrap_or("")) {
                headers.insert(name, v);
            }
        }
    }
    for (key, value) in signing_instructions.headers() {
        if let Ok(name) = reqwest::header::HeaderName::from_bytes(key.as_ref()) {
            if let Ok(v) = reqwest::header::HeaderValue::from_str(value) {
                headers.insert(name, v);
            }
        }
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

fn session_token() -> Option<String> {
    std::env::var("AWS_SESSION_TOKEN").ok()
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
