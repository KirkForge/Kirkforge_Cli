//! Wiremock-backed contract tests for the AWS Bedrock wire path (WO 28.13).
//!
//! The offline unit tests in `anthropic_bedrock.rs` / `bedrock_signing.rs`
//! assert URL strings and parse static byte fixtures, but nothing drove the
//! signing + HTTP transport + event-stream decode loop against a real server.
//! These tests spin a `wiremock::MockServer`, sign a real SigV4 request with
//! fake env credentials, send it, and assert:
//!
//! 1. The mock only answers when the `Authorization` header is
//!    SigV4-well-formed (`AWS4-HMAC-SHA256 ... SignedHeaders=...`) — a
//!    custom matcher makes the gate a contract check, not theater.
//! 2. The model id appears in the request path.
//! 3. A real event-stream frame sequence served by the mock decodes into
//!    `StreamEvent::Text` deltas through `parse_bedrock_event_stream`.
//!
//! All tests run with zero AWS credentials in the real environment — the
//! fake `AWS_ACCESS_KEY_ID` / `AWS_SECRET_ACCESS_KEY` values are set under a
//! shared env lock (`bedrock_signing::tests::env_lock`) so they don't race
//! with the offline signing tests.
//!
//! Vertex (WO 28.13 R2) is DEFERRED: `AnthropicVertexAdapter::access_token`
//! calls `yup_oauth2`'s `ServiceAccountAuthenticator`, which hits Google's
//! real OAuth endpoint and cannot be redirected at a wiremock server without
//! an authenticator injection (the upgrade path named in `vertex_auth.rs`).
//! Remaining work: inject an `Authenticator` trait so tests can supply a
//! fake bearer token; tracked in WO 28.13 R2-later + state.md.

use crate::adapters::anthropic_bedrock::parse_bedrock_event_stream;
use crate::adapters::bedrock_signing::{sign_request, tests::env_lock, SignedRequest};
use crate::shared::StreamEvent;
use wiremock::matchers::{header_exists, method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

/// Custom wiremock matcher: the request must carry a SigV4-well-formed
/// `Authorization` header. This is what turns the mock from theater into a
/// contract gate — an unsigned or mangled signature does not match, so the
/// server returns its default (non-2xx) and the test fails.
struct SigV4Authorization;

impl wiremock::Match for SigV4Authorization {
    fn matches(&self, request: &Request) -> bool {
        request
            .headers
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .map(|s| s.starts_with("AWS4-HMAC-SHA256") && s.contains("SignedHeaders="))
            .unwrap_or(false)
    }
}

/// Hold fake AWS env credentials for the duration of a test, restoring the
/// previous values (or unsetting) on drop. sign_request reads these from env.
struct AwsCredsGuard {
    prev_access: Option<String>,
    prev_secret: Option<String>,
    prev_session: Option<String>,
}

impl AwsCredsGuard {
    fn install() -> Self {
        let prev_access = std::env::var("AWS_ACCESS_KEY_ID").ok();
        let prev_secret = std::env::var("AWS_SECRET_ACCESS_KEY").ok();
        let prev_session = std::env::var("AWS_SESSION_TOKEN").ok();
        std::env::set_var("AWS_ACCESS_KEY_ID", "AKIATEST");
        std::env::set_var("AWS_SECRET_ACCESS_KEY", "secretkey");
        std::env::remove_var("AWS_SESSION_TOKEN");
        Self {
            prev_access,
            prev_secret,
            prev_session,
        }
    }
}

impl Drop for AwsCredsGuard {
    fn drop(&mut self) {
        match self.prev_access.take() {
            Some(v) => std::env::set_var("AWS_ACCESS_KEY_ID", v),
            None => std::env::remove_var("AWS_ACCESS_KEY_ID"),
        }
        match self.prev_secret.take() {
            Some(v) => std::env::set_var("AWS_SECRET_ACCESS_KEY", v),
            None => std::env::remove_var("AWS_SECRET_ACCESS_KEY"),
        }
        match self.prev_session.take() {
            Some(v) => std::env::set_var("AWS_SESSION_TOKEN", v),
            None => std::env::remove_var("AWS_SESSION_TOKEN"),
        }
    }
}

/// Sign a request with fake env credentials, holding the shared env lock only
/// for the synchronous sign call (never across an `.await`). The guard is
/// dropped before return, so the caller's subsequent HTTP awaits never touch
/// the env — the request is already signed.
fn sign_with_fake_creds(url: &str, body: &[u8], region: &str) -> SignedRequest {
    let _env = env_lock().lock().unwrap();
    let _creds = AwsCredsGuard::install();
    sign_request(url, body, region).expect("sign_request")
}

const MODEL_ID: &str = "anthropic.claude-3-5-sonnet-20240620-v1:0";

/// The canonical two-text-delta event-stream body the offline parse tests
/// use. `extract_payload` scans the byte stream for `{"type":...}` objects,
/// so concatenated JSON frames decode without binary envelope framing.
fn two_delta_body() -> Vec<u8> {
    let frames = [
        r#"{"type":"message_start","message":{}}"#,
        r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hello "}}"#,
        r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Bedrock"}}"#,
        r#"{"type":"message_stop"}"#,
    ];
    frames.join("\n").into_bytes()
}

/// WO 28.13 R1: a properly SigV4-signed request is accepted (200) by a mock
/// that requires a well-formed `Authorization` header, and the wire bytes
/// carry the SigV4 markers + model id in the path. Zero live AWS credentials.
#[tokio::test]
async fn bedrock_signed_request_passes_sigv4_contract_gate() {
    let server = MockServer::start().await;
    let url = format!(
        "{server}/model/{MODEL_ID}/invoke-with-response-stream",
        server = server.uri()
    );

    Mock::given(method("POST"))
        .and(path(format!(
            "/model/{MODEL_ID}/invoke-with-response-stream"
        )))
        .and(header_exists("authorization"))
        .and(header_exists("x-amz-content-sha256"))
        .and(SigV4Authorization)
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "application/vnd.amazon.eventstream")
                .set_body_bytes(two_delta_body()),
        )
        .mount(&server)
        .await;

    let body = br#"{"anthropic_version":"bedrock-2023-05-31","messages":[]}"#;
    let signed = sign_with_fake_creds(&url, body, "us-east-1");

    let client = reqwest::Client::new();
    let resp = client
        .request(signed.method, &signed.url)
        .headers(signed.headers)
        .body(body.to_vec())
        .send()
        .await
        .expect("send");

    assert_eq!(
        resp.status(),
        reqwest::StatusCode::OK,
        "well-formed SigV4 request should be accepted by the contract mock"
    );

    // Inspect the wire bytes the mock actually received.
    let received = server.received_requests().await.expect("recorded");
    assert_eq!(received.len(), 1, "exactly one request should have matched");
    let auth = received[0]
        .headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .expect("authorization header present");
    assert!(
        auth.starts_with("AWS4-HMAC-SHA256"),
        "Authorization must be SigV4-shaped, got: {auth}"
    );
    assert!(
        auth.contains("SignedHeaders="),
        "Authorization must list SignedHeaders, got: {auth}"
    );
    // The model id must appear in the request path.
    assert!(
        received[0].url.path().contains(MODEL_ID),
        "model id must be in the path, got: {}",
        received[0].url.path()
    );
    assert!(
        received[0].headers.contains_key("x-amz-content-sha256"),
        "x-amz-content-sha256 digest header must be present"
    );
}

/// WO 28.13 R1: a request with NO Authorization header is rejected by the
/// contract mock (wiremock returns its default non-2xx when nothing matches).
/// This proves the mock enforces SigV4 conformance rather than accepting any
/// well-formed HTTP — the WO's explicit anti-theater requirement.
#[tokio::test]
async fn bedrock_unsigned_request_rejected_by_contract_gate() {
    let server = MockServer::start().await;
    let url = format!(
        "{server}/model/{MODEL_ID}/invoke-with-response-stream",
        server = server.uri()
    );

    // The mock ONLY responds 200 to a SigV4-signed request.
    Mock::given(method("POST"))
        .and(SigV4Authorization)
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;

    let client = reqwest::Client::new();
    // Deliberately UNSIGNED request — no Authorization header at all.
    let resp = client
        .post(&url)
        .header("content-type", "application/json")
        .body(br#"{"messages":[]}"#.to_vec())
        .send()
        .await
        .expect("send");

    assert_ne!(
        resp.status(),
        reqwest::StatusCode::OK,
        "unsigned request must NOT be accepted by the contract mock (got {})",
        resp.status()
    );

    // And a properly signed request to the same mock IS accepted — proving
    // the rejection above was the SigV4 gate, not a malformed URL.
    let body = br#"{"messages":[]}"#;
    let signed = sign_with_fake_creds(&url, body, "us-east-1");
    let signed_resp = client
        .request(signed.method, &signed.url)
        .headers(signed.headers)
        .body(body.to_vec())
        .send()
        .await
        .expect("send");
    assert_eq!(
        signed_resp.status(),
        reqwest::StatusCode::OK,
        "signed request should pass the gate that rejected the unsigned one"
    );
}

/// WO 28.13 R1: a real event-stream frame sequence served by the mock
/// decodes into Text deltas through the Bedrock parse path. Exercises
/// sign -> HTTP transport -> response bytes -> frame extraction ->
/// Anthropic stream parse, end to end against a mock server.
#[tokio::test]
async fn bedrock_signed_response_decodes_event_stream_frames() {
    let server = MockServer::start().await;
    let url = format!(
        "{server}/model/{MODEL_ID}/invoke-with-response-stream",
        server = server.uri()
    );

    Mock::given(method("POST"))
        .and(SigV4Authorization)
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "application/vnd.amazon.eventstream")
                .set_body_bytes(two_delta_body()),
        )
        .mount(&server)
        .await;

    let body = br#"{"anthropic_version":"bedrock-2023-05-31","messages":[]}"#;
    let signed = sign_with_fake_creds(&url, body, "us-east-1");
    let client = reqwest::Client::new();
    let response = client
        .request(signed.method, &signed.url)
        .headers(signed.headers)
        .body(body.to_vec())
        .send()
        .await
        .expect("send")
        .error_for_status()
        .expect("2xx");

    let (tx, mut rx) = tokio::sync::mpsc::channel::<StreamEvent>(4096);
    // parse_bedrock_event_stream completes once the body stream ends.
    parse_bedrock_event_stream(
        tx,
        response.bytes_stream(),
        crate::adapters::STREAM_IDLE_TIMEOUT,
    )
    .await;

    let mut text = String::new();
    let drained = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        while let Some(ev) = rx.recv().await {
            match ev {
                StreamEvent::Text(t) => text.push_str(&t),
                StreamEvent::Done { .. } => break,
                StreamEvent::Error(e) => panic!("stream error: {e}"),
                _ => {}
            }
        }
    })
    .await;
    assert!(drained.is_ok(), "stream did not drain within 10s");
    assert!(
        text.contains("Hello") && text.contains("Bedrock"),
        "both deltas should decode, got {text:?}"
    );
}
