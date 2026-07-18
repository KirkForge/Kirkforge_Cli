use crate::session::access::DenyList;
use crate::shared::{ToolDef, ToolError, ToolOutcome};
use crate::tools::{Tool, ToolContext};
use std::time::Duration;

/// Maximum response body we will accept (1 MiB). This caps both memory usage
/// and the size of the string we later feed into the model context.
const MAX_BODY_BYTES: usize = 1024 * 1024;

/// Default cap on how much of a fetched body is returned to the model. Matches
/// Config::max_tool_result_chars default; the tool does not need runtime
/// config access for this MVP.
const DEFAULT_MAX_TOOL_RESULT_CHARS: usize = 4_000;

/// Network fetch timeout. 30s matches the vix reference implementation.
const FETCH_TIMEOUT: Duration = Duration::from_secs(30);

/// Explicit, honest user agent so targets know a bot is calling.
const USER_AGENT: &str = "KirkForge-Cli/0.1.0 (https://github.com/KirkForge/KirkForge-Cli)";

pub struct WebFetch {
    deny_list: DenyList,
    client: reqwest::Client,
}

impl WebFetch {
    pub fn new(deny_list: DenyList) -> Self {
        let client = reqwest::Client::builder()
            .timeout(FETCH_TIMEOUT)
            .user_agent(USER_AGENT)
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Self { deny_list, client }
    }

    #[cfg(test)]
    fn with_client(deny_list: DenyList, client: reqwest::Client) -> Self {
        Self { deny_list, client }
    }
}

#[async_trait::async_trait]
impl Tool for WebFetch {
    fn def(&self) -> ToolDef {
        ToolDef {
            name: "web_fetch",
            description: "Fetch a public URL and return its body as plain text. Supports HTML, JSON, and text. HTML is stripped to readable text. Blocked URLs include cloud metadata endpoints and any configured deny_list entries.",
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "url": {
                        "type": "string",
                        "description": "The URL to fetch. Only http:// and https:// URLs are allowed."
                    }
                },
                "required": ["url"]
            }),
        }
    }

    async fn run(&self, _ctx: &ToolContext, args: serde_json::Value) -> ToolOutcome {
        let url = match args.get("url").and_then(|u| u.as_str()) {
            Some(u) => u,
            None => {
                return ToolOutcome::Failure(ToolError::invalid_args("Missing 'url' argument"));
            }
        };

        let trimmed = url.trim();
        if trimmed.is_empty() {
            return ToolOutcome::Failure(ToolError::invalid_args("URL is empty"));
        }

        // Scheme guard: only http(s).
        let lower = trimmed.to_ascii_lowercase();
        if !(lower.starts_with("http://") || lower.starts_with("https://")) {
            return ToolOutcome::Failure(ToolError::AccessDenied {
                message: "Only http:// and https:// URLs are allowed".into(),
            });
        }

        // Deny-list guard. Reuses the same list that protects bash/grep/etc.
        // from cloud metadata endpoints. We also reject literal loopback / link
        // local / private IP hosts to close the obvious DNS-rebinding SSRF
        // path where a public hostname resolves to 127.0.0.1 at call time.
        if self.deny_list.is_url_denied(trimmed) {
            return ToolOutcome::Failure(ToolError::AccessDenied {
                message: "URL is denied by the security policy".into(),
            });
        }
        if host_is_literal_internal_ip(trimmed) {
            return ToolOutcome::Failure(ToolError::AccessDenied {
                message: "URL resolves to a private/internal IP by literal host".into(),
            });
        }

        let request = match self.client.get(trimmed).build() {
            Ok(r) => r,
            Err(e) => {
                return ToolOutcome::Failure(ToolError::Internal {
                    message: format!("Failed to build request for {trimmed}: {e}"),
                });
            }
        };

        let response = match self.client.execute(request).await {
            Ok(r) => r,
            Err(e) => {
                return ToolOutcome::Failure(ToolError::Internal {
                    message: format!("Failed to fetch {trimmed}: {e}"),
                });
            }
        };

        let status = response.status();
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_ascii_lowercase();

        let body_bytes = match response.bytes().await {
            Ok(b) => b,
            Err(e) => {
                return ToolOutcome::Failure(ToolError::Internal {
                    message: format!("Failed to read response body from {trimmed}: {e}"),
                });
            }
        };

        if body_bytes.len() > MAX_BODY_BYTES {
            return ToolOutcome::Failure(ToolError::Internal {
                message: format!(
                    "Response from {trimmed} is {} bytes, exceeds {MAX_BODY_BYTES} byte cap",
                    body_bytes.len()
                ),
            });
        }

        if !status.is_success() {
            let preview = String::from_utf8_lossy(&body_bytes).chars().take(200).collect::<String>();
            return ToolOutcome::Failure(ToolError::Execution {
                message: format!("HTTP {status} from {trimmed}"),
                exit_code: Some(status.as_u16() as i32),
                stderr: preview,
            });
        }

        let raw = String::from_utf8_lossy(&body_bytes).into_owned();
        let output = if content_type.contains("text/html") || looks_like_html(&raw) {
            html_to_text(&raw)
        } else {
            raw
        };

            let content = if output.len() > DEFAULT_MAX_TOOL_RESULT_CHARS {
                format!(
                    "{}\n\n[truncated {} characters]",
                    &output[..DEFAULT_MAX_TOOL_RESULT_CHARS],
                    output.len().saturating_sub(DEFAULT_MAX_TOOL_RESULT_CHARS)
                )
            } else {
                output
            };

        ToolOutcome::Success { content }
    }
}

/// Reject URLs whose host is a literal loopback, link-local, or RFC1918 / RFC4193
/// address. This is a lightweight complement to the deny-list; it does not do
/// DNS resolution, but it stops the model from passing `http://127.0.0.1/...`
/// directly. DNS-rebinding at lookup time remains a hard problem for a client
/// tool; a future iteration should pin the resolved IP and re-check it.
fn host_is_literal_internal_ip(url: &str) -> bool {
    let Some(host) = extract_host(url) else {
        return true; // malformed URL -> fail closed
    };
    if let Ok(addr) = host.parse::<std::net::IpAddr>() {
        return is_internal_addr(&addr);
    }
    false
}

fn is_internal_addr(addr: &std::net::IpAddr) -> bool {
    match addr {
        std::net::IpAddr::V4(v4) => {
            v4.is_loopback()
                || v4.is_unspecified()
                || v4.is_private()
                || is_link_local_v4(v4)
        }
        std::net::IpAddr::V6(v6) => {
            // loopback ::1; unique local fc00::/7; link-local fe80::/10
            *v6 == std::net::Ipv6Addr::LOCALHOST
                || (v6.segments()[0] & 0xfe00) == 0xfc00
                || (v6.segments()[0] & 0xffc0) == 0xfe80
        }
    }
}

fn is_link_local_v4(addr: &std::net::Ipv4Addr) -> bool {
    // 169.254.0.0/16
    let octets = addr.octets();
    octets[0] == 169 && octets[1] == 254
}

fn extract_host(url: &str) -> Option<String> {
    // Strip scheme.
    let without_scheme = if let Some(pos) = url.find("://") {
        &url[pos + 3..]
    } else {
        url
    };
    // Take up to next path/query/fragment.
    let end = without_scheme
        .find('/')
        .or_else(|| without_scheme.find('?'))
        .or_else(|| without_scheme.find('#'))
        .unwrap_or(without_scheme.len());
    let host_port = &without_scheme[..end];
    // Remove optional userinfo.
    let after_userinfo = host_port.rsplit('@').next().unwrap_or(host_port);
    // Remove optional port, carefully: IPv6 literals are bracketed, so only
    // split on the last ':' if it follows a ']' or if there is exactly one ':'.
    let host = if after_userinfo.ends_with(']') {
        after_userinfo.to_string()
    } else if let Some(colon) = after_userinfo.rfind(':') {
        // For IPv4 or hostnames, the last colon introduces the port.
        if after_userinfo[..colon].contains(':') {
            // IPv6 without brackets — malformed, fail closed.
            return None;
        }
        after_userinfo[..colon].to_string()
    } else {
        after_userinfo.to_string()
    };
    let host = host.trim_start_matches('[').trim_end_matches(']').to_string();
    if host.is_empty() {
        return None;
    }
    Some(host)
}

fn looks_like_html(body: &str) -> bool {
    let lower = body.to_ascii_lowercase();
    lower.contains("<!doctype html")
        || lower.contains("<html")
        || lower.contains("<head")
        || lower.contains("<body")
}

/// Lightweight regex-only HTML-to-text converter.
///
/// ponytail: the project already uses regex for the graph-emitter's
/// heuristics; a real parser (html5ever / scraper) is the obvious upgrade but
/// adds a dependency and attack surface. Regex stripping is sufficient for
/// model consumption and keeps the tool self-contained.
fn html_to_text(html: &str) -> String {
    static SCRIPT_RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    static STYLE_RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    static TAG_RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    static WS_RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();

    let script_re = SCRIPT_RE
        .get_or_init(|| regex::Regex::new(r"(?is)<script[^>]*>.*?</script>").unwrap());
    let style_re = STYLE_RE
        .get_or_init(|| regex::Regex::new(r"(?is)<style[^>]*>.*?</style>").unwrap());
    let tag_re = TAG_RE.get_or_init(|| regex::Regex::new(r"<[^>]+>").unwrap());
    let ws_re = WS_RE.get_or_init(|| regex::Regex::new(r"[ \t]+").unwrap());

    let no_scripts = script_re.replace_all(html, " ");
    let no_styles = style_re.replace_all(&no_scripts, " ");
    let no_tags = tag_re.replace_all(&no_styles, " ");
    let decoded = html_entities::decode(&no_tags);
    // Normalize line-oriented whitespace without allocating many times.
    let collapsed: String = decoded
        .lines()
        .map(|line| ws_re.replace_all(line, " ").trim().to_string())
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    collapsed
}

// Minimal HTML entity decoder for the tokens most likely to appear in web pages.
// A real parser would be more complete; this is the regex-only ceiling.
mod html_entities {
    pub fn decode(input: &str) -> String {
        let mut out = String::with_capacity(input.len());
        let mut rest = input;
        while let Some(pos) = rest.find('&') {
            out.push_str(&rest[..pos]);
            rest = &rest[pos..];
            if let Some(semi) = rest.find(';') {
                let entity = &rest[1..semi];
                let replacement = match entity {
                    "amp" => "&",
                    "lt" => "<",
                    "gt" => ">",
                    "quot" => "\"",
                    "apos" => "'",
                    "nbsp" => " ",
                    "ndash" => "–",
                    "mdash" => "—",
                    _ => {
                        if let Some(code) = entity.strip_prefix('#') {
                            if let Ok(n) = code.parse::<u32>() {
                                if let Some(c) = char::from_u32(n) {
                                    out.push(c);
                                    rest = &rest[semi + 1..];
                                    continue;
                                }
                            }
                        }
                        // Unknown entity: preserve the original text.
                        out.push('&');
                        out.push_str(entity);
                        out.push(';');
                        rest = &rest[semi + 1..];
                        continue;
                    }
                };
                out.push_str(replacement);
                rest = &rest[semi + 1..];
            } else {
                // Unterminated ampersand.
                out.push('&');
                rest = &rest[1..];
            }
        }
        out.push_str(rest);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::Tool;
    use serde_json::json;

    #[tokio::test]
    async fn rejects_non_http_scheme() {
        let tool = WebFetch::new(DenyList::default());
        let outcome = tool
            .run(&ToolContext::new(), json!({"url": "file:///etc/passwd"}))
            .await;
        let ToolOutcome::Failure(ToolError::AccessDenied { message }) = outcome else {
            panic!("expected AccessDenied, got {outcome:?}");
        };
        assert!(message.contains("Only http:// and https://"));
    }

    #[tokio::test]
    async fn rejects_metadata_endpoint() {
        let tool = WebFetch::new(DenyList::default());
        let outcome = tool
            .run(&ToolContext::new(), json!({"url": "http://169.254.169.254/latest/meta-data/"}))
            .await;
        assert!(
            matches!(outcome, ToolOutcome::Failure(ToolError::AccessDenied { .. })),
            "expected denied metadata endpoint, got {outcome:?}"
        );
    }

    #[tokio::test]
    async fn rejects_literal_internal_ip() {
        let tool = WebFetch::new(DenyList::default());
        let outcome = tool
            .run(&ToolContext::new(), json!({"url": "http://127.0.0.1:8080/secret"}))
            .await;
        assert!(
            matches!(outcome, ToolOutcome::Failure(ToolError::AccessDenied { .. })),
            "expected denied internal IP, got {outcome:?}"
        );
    }

    fn test_tool_for(server: &wiremock::MockServer) -> WebFetch {
        // The fetch tool blocks literal internal IPs. Wiremock binds to
        // 127.0.0.1, so point a non-internal hostname at it via reqwest's
        // resolver override for tests.
        let addr: std::net::SocketAddr = *server.address();
        let client = reqwest::Client::builder()
            .timeout(FETCH_TIMEOUT)
            .user_agent(USER_AGENT)
            .resolve("test.local", addr)
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        WebFetch::with_client(DenyList::default(), client)
    }

    #[tokio::test]
    async fn fetches_json_successfully() {
        let body = r#"{"hello": "world"}"#;
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .respond_with(wiremock::ResponseTemplate::new(200)
                .set_body_string(body)
                .insert_header("content-type", "application/json"))
            .mount(&server)
            .await;

        let tool = test_tool_for(&server);
        let outcome = tool
            .run(&ToolContext::new(), json!({"url": "http://test.local/"}))
            .await;
        let ToolOutcome::Success { content } = outcome else {
            panic!("expected Success, got {outcome:?}");
        };
        assert!(content.contains("hello"));
        assert!(content.contains("world"));
    }

    #[tokio::test]
    async fn html_is_stripped_to_text() {
        let html = r#"<!DOCTYPE html><html><head><title>Hi</title><script>alert(1)</script></head><body><h1>  Hello  </h1><p>World &amp; more.</p></body></html>"#;
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .respond_with(wiremock::ResponseTemplate::new(200)
                .set_body_string(html)
                .insert_header("content-type", "text/html"))
            .mount(&server)
            .await;

        let tool = test_tool_for(&server);
        let outcome = tool
            .run(&ToolContext::new(), json!({"url": "http://test.local/page"}))
            .await;
        let ToolOutcome::Success { content } = outcome else {
            panic!("expected Success, got {outcome:?}");
        };
        assert!(
            content.contains("Hello"),
            "heading text should survive stripping: {content}"
        );
        assert!(
            content.contains("World & more"),
            "entity decoding failed: {content}"
        );
        assert!(
            !content.contains("<script>"),
            "script tags should be stripped: {content}"
        );
        assert!(
            !content.contains("alert(1)"),
            "script content should be stripped: {content}"
        );
    }

    #[tokio::test]
    async fn non_2xx_returns_failure() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .respond_with(wiremock::ResponseTemplate::new(503).set_body_string("overloaded"))
            .mount(&server)
            .await;

        let tool = test_tool_for(&server);
        let outcome = tool
            .run(&ToolContext::new(), json!({"url": "http://test.local/"}))
            .await;
        assert!(
            matches!(outcome, ToolOutcome::Failure(ToolError::Execution { .. })),
            "expected HTTP execution failure, got {outcome:?}"
        );
    }

    #[tokio::test]
    async fn oversized_response_is_rejected() {
        let big = "x".repeat(MAX_BODY_BYTES + 1);
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_string(big))
            .mount(&server)
            .await;

        let tool = test_tool_for(&server);
        let outcome = tool
            .run(&ToolContext::new(), json!({"url": "http://test.local/"}))
            .await;
        assert!(
            matches!(outcome, ToolOutcome::Failure(ToolError::Internal { .. })),
            "expected oversized failure, got {outcome:?}"
        );
    }
}
