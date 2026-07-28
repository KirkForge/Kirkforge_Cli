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
    fn session_token_reads_from_env() {
        let key = "AWS_SESSION_TOKEN";
        let prev = std::env::var(key).ok();
        std::env::set_var(key, "test-token-value");
        assert_eq!(session_token(), Some("test-token-value".to_string()));
        match prev {
            Some(v) => std::env::set_var(key, v),
            None => std::env::remove_var(key),
        }
    }

    #[test]
    fn session_token_returns_none_when_unset() {
        let key = "AWS_SESSION_TOKEN";
        let prev = std::env::var(key).ok();
        std::env::remove_var(key);
        assert_eq!(session_token(), None);
        if let Some(v) = prev {
            std::env::set_var(key, v)
        }
    }

    #[test]
    fn resolve_credentials_reads_from_env() {
        let access_key = "AWS_ACCESS_KEY_ID";
        let secret_key = "AWS_SECRET_ACCESS_KEY";
        let session_key = "AWS_SESSION_TOKEN";
        let prev_access = std::env::var(access_key).ok();
        let prev_secret = std::env::var(secret_key).ok();
        let prev_session = std::env::var(session_key).ok();
        std::env::set_var(access_key, "AKIATEST");
        std::env::set_var(secret_key, "secretkey");
        std::env::set_var(session_key, "sessiontoken");
        let creds = resolve_credentials().unwrap();
        assert_eq!(creds.access_key_id(), "AKIATEST");
        assert_eq!(creds.secret_access_key(), "secretkey");
        match prev_access {
            Some(v) => std::env::set_var(access_key, v),
            None => std::env::remove_var(access_key),
        }
        match prev_secret {
            Some(v) => std::env::set_var(secret_key, v),
            None => std::env::remove_var(secret_key),
        }
        match prev_session {
            Some(v) => std::env::set_var(session_key, v),
            None => std::env::remove_var(session_key),
        }
    }

    #[test]
    fn resolve_credentials_fails_without_access_key() {
        let access_key = "AWS_ACCESS_KEY_ID";
        let prev = std::env::var(access_key).ok();
        std::env::remove_var(access_key);
        assert!(resolve_credentials().is_err());
        if let Some(v) = prev {
            std::env::set_var(access_key, v)
        }
    }

    #[test]
    fn resolve_credentials_fails_without_secret_key() {
        let access_key = "AWS_ACCESS_KEY_ID";
        let secret_key = "AWS_SECRET_ACCESS_KEY";
        let prev_access = std::env::var(access_key).ok();
        let prev_secret = std::env::var(secret_key).ok();
        std::env::set_var(access_key, "AKIATEST");
        std::env::remove_var(secret_key);
        assert!(resolve_credentials().is_err());
        match prev_access {
            Some(v) => std::env::set_var(access_key, v),
            None => std::env::remove_var(access_key),
        }
        if let Some(v) = prev_secret {
            std::env::set_var(secret_key, v);
        }
    }

    #[test]
    fn sign_request_produces_authorization_header() {
        let access_key = "AWS_ACCESS_KEY_ID";
        let secret_key = "AWS_SECRET_ACCESS_KEY";
        let session_key = "AWS_SESSION_TOKEN";
        let prev_access = std::env::var(access_key).ok();
        let prev_secret = std::env::var(secret_key).ok();
        let prev_session = std::env::var(session_key).ok();
        std::env::set_var(access_key, "AKIATEST");
        std::env::set_var(secret_key, "secretkey");
        std::env::remove_var(session_key);
        let url = "https://bedrock-runtime.us-east-1.amazonaws.com/model/test/invoke-with-response-stream";
        let body = b"{\"hello\":\"world\"}";
        let signed = sign_request(url, body, "us-east-1", "").unwrap();
        assert_eq!(signed.method, reqwest::Method::POST);
        assert_eq!(signed.url, url);
        assert!(signed.headers.contains_key("authorization"));
        assert!(signed.headers.contains_key("x-amz-content-sha256"));
        assert!(signed.headers.contains_key("host"));
        match prev_access {
            Some(v) => std::env::set_var(access_key, v),
            None => std::env::remove_var(access_key),
        }
        match prev_secret {
            Some(v) => std::env::set_var(secret_key, v),
            None => std::env::remove_var(secret_key),
        }
        match prev_session {
            Some(v) => std::env::set_var(session_key, v),
            None => std::env::remove_var(session_key),
        }
    }

    #[test]
    fn sign_request_includes_security_token_when_set() {
        let access_key = "AWS_ACCESS_KEY_ID";
        let secret_key = "AWS_SECRET_ACCESS_KEY";
        let session_key = "AWS_SESSION_TOKEN";
        let prev_access = std::env::var(access_key).ok();
        let prev_secret = std::env::var(secret_key).ok();
        let prev_session = std::env::var(session_key).ok();
        std::env::set_var(access_key, "AKIATEST");
        std::env::set_var(secret_key, "secretkey");
        std::env::set_var(session_key, "mysessiontoken");
        let url =
            "https://bedrock-runtime.us-east-1.amazonaws.com/model/x/invoke-with-response-stream";
        let signed = sign_request(url, b"{}", "us-east-1", "").unwrap();
        assert!(signed.headers.contains_key("x-amz-security-token"));
        match prev_access {
            Some(v) => std::env::set_var(access_key, v),
            None => std::env::remove_var(access_key),
        }
        match prev_secret {
            Some(v) => std::env::set_var(secret_key, v),
            None => std::env::remove_var(secret_key),
        }
        match prev_session {
            Some(v) => std::env::set_var(session_key, v),
            None => std::env::remove_var(session_key),
        }
    }

    #[test]
    fn sign_request_fails_for_invalid_url() {
        let access_key = "AWS_ACCESS_KEY_ID";
        let secret_key = "AWS_SECRET_ACCESS_KEY";
        let prev_access = std::env::var(access_key).ok();
        let prev_secret = std::env::var(secret_key).ok();
        std::env::set_var(access_key, "AKIATEST");
        std::env::set_var(secret_key, "secretkey");
        assert!(sign_request("not a url", b"{}", "us-east-1", "").is_err());
        match prev_access {
            Some(v) => std::env::set_var(access_key, v),
            None => std::env::remove_var(access_key),
        }
        match prev_secret {
            Some(v) => std::env::set_var(secret_key, v),
            None => std::env::remove_var(secret_key),
        }
    }
}
