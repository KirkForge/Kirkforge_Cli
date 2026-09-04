//! AWS SigV4 signing for Anthropic-on-Bedrock requests.
//!
//! Implemented in-tree (WO 43.20) using `sha2` + `hmac` + `hex` — no
//! `aws-sigv4` dependency, which eliminates the http 0.2 / http-body 0.4
//! duplicate that crate pulled in. The signing process builds a canonical
//! request, hashes the payload, derives the HMAC-SHA256 signing key chain,
//! and produces the `Authorization` header plus any required session headers.
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
use chrono::Utc;
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};
use std::time::SystemTime;

type HmacSha256 = Hmac<Sha256>;

/// A fully signed HTTP request ready for `reqwest`.
pub struct SignedRequest {
    pub method: reqwest::Method,
    pub url: String,
    pub headers: reqwest::header::HeaderMap,
}

/// Env-resolved AWS credentials. Minimal mirror of the `aws-credential-types`
/// `Credentials` accessors that this module (and its tests) use.
struct Credentials {
    access_key: String,
    secret_key: String,
    session_token: Option<String>,
}

impl Credentials {
    fn access_key_id(&self) -> &str {
        &self.access_key
    }
    fn secret_access_key(&self) -> &str {
        &self.secret_key
    }
    fn session_token(&self) -> Option<&str> {
        self.session_token.as_deref()
    }
}

/// Sign a Bedrock InvokeModelWithResponseStream request.
pub fn sign_request(url: &str, body: &[u8], region: &str) -> anyhow::Result<SignedRequest> {
    let creds = resolve_credentials()?;
    let host = host_header(url)?;
    let parsed = url::Url::parse(url).context("invalid URL")?;
    let path = parsed.path();
    let path = if path.is_empty() { "/" } else { path };

    // Bedrock invoke-with-response-stream uses POST with no query string.
    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .context("system time before epoch")?;
    let datetime = chrono::DateTime::<Utc>::from_timestamp(now.as_secs() as i64, 0)
        .context("invalid timestamp")?;
    let amz_date = datetime.format("%Y%m%dT%H%M%SZ").to_string();
    let date = datetime.format("%Y%m%d").to_string();
    let payload_hash = sha256_hex(body);

    // Build the header set in canonical (sorted) order. content-length is
    // signed per the SigV4 spec (mm-H13, WO 47.29): reqwest would add it
    // itself; adding it here first makes the signed value and the sent
    // value identical, and real AWS rejects a signature that omits a
    // header the client actually sends.
    let content_length = body.len().to_string();
    let mut headers: Vec<(&str, String)> = vec![
        ("content-length", content_length),
        ("content-type", "application/json".to_string()),
        ("host", host),
        ("x-amz-content-sha256", payload_hash.clone()),
        ("x-amz-date", amz_date.clone()),
    ];
    if let Some(token) = creds.session_token() {
        headers.push(("x-amz-security-token", token.to_string()));
    }

    // Canonical headers: sorted, lowercased keys, trimmed values, "k:v\n".
    let mut canonical_headers: Vec<(String, String)> = headers
        .iter()
        .map(|(k, v)| (k.to_string(), v.trim().to_string()))
        .collect();
    canonical_headers.sort_by(|a, b| a.0.cmp(&b.0));
    let canonical_headers_str: String = canonical_headers
        .iter()
        .map(|(k, v)| format!("{k}:{v}\n"))
        .collect();
    let signed_headers_str: String = canonical_headers
        .iter()
        .map(|(k, _)| k.as_str())
        .collect::<Vec<_>>()
        .join(";");

    // Canonical request (query string is empty for Bedrock invoke).
    let canonical_request =
        format!("POST\n{path}\n\n{canonical_headers_str}\n{signed_headers_str}\n{payload_hash}");

    // String to sign.
    let credential_scope = format!("{date}/{region}/bedrock/aws4_request");
    let canonical_request_hash = sha256_hex(canonical_request.as_bytes());
    let string_to_sign =
        format!("AWS4-HMAC-SHA256\n{amz_date}\n{credential_scope}\n{canonical_request_hash}");

    // Signing key: kDate -> kRegion -> kService -> kSigning.
    let k_date = hmac_bytes(
        format!("AWS4{}", creds.secret_access_key()).as_bytes(),
        date.as_bytes(),
    );
    let k_region = hmac_bytes(&k_date, region.as_bytes());
    let k_service = hmac_bytes(&k_region, b"bedrock");
    let k_signing = hmac_bytes(&k_service, b"aws4_request");

    let signature = hex::encode(hmac_bytes(&k_signing, string_to_sign.as_bytes()));
    let authorization = format!(
        "AWS4-HMAC-SHA256 Credential={}/{credential_scope}, SignedHeaders={signed_headers_str}, Signature={signature}",
        creds.access_key_id()
    );

    // Assemble the reqwest HeaderMap.
    let mut header_map = reqwest::header::HeaderMap::new();
    for (k, v) in &headers {
        let name = reqwest::header::HeaderName::from_bytes(k.as_bytes())
            .with_context(|| format!("invalid header name: {k:?}"))?;
        let val = reqwest::header::HeaderValue::from_str(v)
            .with_context(|| format!("non-ASCII value for header {k:?}"))?;
        header_map.insert(name, val);
    }
    let auth_name = reqwest::header::HeaderName::from_bytes(b"authorization")
        .context("invalid header name: authorization")?;
    let auth_val = reqwest::header::HeaderValue::from_str(&authorization)
        .context("non-ASCII value for authorization header")?;
    header_map.insert(auth_name, auth_val);

    Ok(SignedRequest {
        method: reqwest::Method::POST,
        url: url.to_string(),
        headers: header_map,
    })
}

/// HMAC-SHA256(key, data) → raw bytes.
fn hmac_bytes(key: &[u8], data: &[u8]) -> Vec<u8> {
    let mut mac = HmacSha256::new_from_slice(key).expect("HMAC accepts any key length");
    mac.update(data);
    mac.finalize().into_bytes().to_vec()
}

fn host_header(url: &str) -> anyhow::Result<String> {
    url.parse::<url::Url>()
        .context("invalid URL")?
        .host_str()
        .map(|h| h.to_string())
        .context("URL has no host")
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex::encode(hasher.finalize())
}

fn resolve_credentials() -> anyhow::Result<Credentials> {
    let access_key = std::env::var("AWS_ACCESS_KEY_ID").context("AWS_ACCESS_KEY_ID not set")?;
    let secret_key =
        std::env::var("AWS_SECRET_ACCESS_KEY").context("AWS_SECRET_ACCESS_KEY not set")?;
    let session_token = std::env::var("AWS_SESSION_TOKEN").ok();
    Ok(Credentials {
        access_key,
        secret_key,
        session_token,
    })
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

    // mm-H13 (WO 47.29): content-length must be sent AND signed — the
    // SignedHeaders list in the Authorization header has to include it.
    #[test]
    fn sign_request_signs_content_length() {
        let _guard = env_lock().lock().unwrap();
        let _g1 = crate::shared::test_util::EnvGuard::set("AWS_ACCESS_KEY_ID", "AKIATEST");
        let _g2 = crate::shared::test_util::EnvGuard::set("AWS_SECRET_ACCESS_KEY", "secretkey");
        let _g3 = crate::shared::test_util::EnvGuard::remove("AWS_SESSION_TOKEN");
        let url =
            "https://bedrock-runtime.us-east-1.amazonaws.com/model/x/invoke-with-response-stream";
        let body = br#"{"a":1}"#;
        let signed = sign_request(url, body, "us-east-1").unwrap();
        assert_eq!(
            signed
                .headers
                .get("content-length")
                .and_then(|v| v.to_str().ok()),
            Some("7"),
            "content-length header must equal the body length"
        );
        let auth_val = signed
            .headers
            .get("authorization")
            .unwrap()
            .to_str()
            .unwrap();
        let signed_headers = auth_val
            .split("SignedHeaders=")
            .nth(1)
            .and_then(|rest| rest.split(',').next())
            .unwrap_or("");
        assert!(
            signed_headers.split(';').any(|h| h == "content-length"),
            "content-length must appear in SignedHeaders, got: {auth_val}"
        );
    }
}
