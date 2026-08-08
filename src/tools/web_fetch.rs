use crate::session::access::DenyList;
use crate::shared::{ToolDef, ToolError, ToolOutcome};
use crate::tools::{Tool, ToolContext};
use percent_encoding::percent_decode_str;
use std::net::{IpAddr, SocketAddr, ToSocketAddrs};
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
        if host_resolves_to_internal_ip(trimmed) {
            return ToolOutcome::Failure(ToolError::AccessDenied {
                message: "URL host resolves to a private/internal IP".into(),
            });
        }

        // DNS-rebinding guard: resolve the host once, check for internal
        // IPs, and pin DNS to the resolved address so the TCP connect uses
        // the same IP we checked. ponytail: builds a new reqwest::Client
        // per hostname request; cache pinned clients if throughput matters.
        let client = match resolve_and_pin_dns(trimmed) {
            Ok(Some(c)) => c,
            Ok(None) => self.client.clone(),
            Err(()) => {
                return ToolOutcome::Failure(ToolError::AccessDenied {
                    message: "URL host resolves to a private/internal IP (DNS-rebinding guard)"
                        .into(),
                });
            }
        };

        let request = match client.get(trimmed).build() {
            Ok(r) => r,
            Err(e) => {
                return ToolOutcome::Failure(ToolError::Internal {
                    message: format!("Failed to build request for {trimmed}: {e}"),
                });
            }
        };

        let response = match client.execute(request).await {
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
            let preview = String::from_utf8_lossy(&body_bytes)
                .chars()
                .take(200)
                .collect::<String>();
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
/// directly.
///
/// ceiling: this guard only catches *literal* internal IPs. The DNS-rebinding
/// path (a public hostname resolving to an internal IP at lookup time) is now
/// closed by `host_resolves_to_internal_ip`, which resolves the host and
/// re-checks each resolved address against `is_internal_addr`. A residual
/// TOCTOU between this resolve and the actual TCP connect is not pinned (the
/// reqwest client does not expose per-request IP pinning without a custom
/// resolver); the resolve-and-check closes the simple rebinding door.
pub(crate) fn host_is_literal_internal_ip(url: &str) -> bool {
    let Some(host) = extract_host(url) else {
        return true; // malformed URL -> fail closed
    };
    if let Ok(addr) = host.parse::<std::net::IpAddr>() {
        return is_internal_addr(&addr);
    }
    false
}

// WO 15.3: resolve the URL host via the OS resolver and reject if any
// resolved address is internal (loopback / link-local / RFC1918 / RFC4193).
// This closes the DNS-rebinding SSRF path: a public hostname whose A record
// points at 127.0.0.1 or 169.254.169.254 defeats the literal-IP guard above.
//
// Returns false when the host is itself a literal IP (those are already
// handled by `host_is_literal_internal_ip`) so we never re-resolve a pinned
// literal. Returns false on resolution *error* — a hostname that does not
// resolve at all will fail later at the actual fetch, and failing closed on
// every NXDOMAIN would break legitimate clients that pin DNS inside the
// reqwest client (tests) rather than the system resolver. The rebinding
// threat requires the attacker's hostname to actually resolve to an
// internal IP, which this guard catches.
pub(crate) fn host_resolves_to_internal_ip(url: &str) -> bool {
    let Some(host) = extract_host(url) else {
        return true; // malformed -> fail closed
    };
    // Literal IPs are already gated by `host_is_literal_internal_ip`; skip
    // re-resolution so there's no TOCTOU on a pinned literal.
    if host.parse::<std::net::IpAddr>().is_ok() {
        return false;
    }
    // `to_socket_addrs` needs a :port pair; use a dummy port 80 for http.
    let probe = format!("{host}:80");
    match probe.to_socket_addrs() {
        Ok(addrs) => addrs.map(|sa| sa.ip()).any(|addr| is_internal_addr(&addr)),
        Err(_) => false, // resolution error -> let the fetch fail later
    }
}

fn is_internal_addr(addr: &std::net::IpAddr) -> bool {
    match addr {
        std::net::IpAddr::V4(v4) => {
            v4.is_loopback() || v4.is_unspecified() || v4.is_private() || is_link_local_v4(v4)
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
    let host = host
        .trim_start_matches('[')
        .trim_end_matches(']')
        .to_string();
    if host.is_empty() {
        return None;
    }
    // Decode percent-encoded host to prevent bypassing the internal-IP
    // check via encoded addresses (e.g., %31%32%37%2e%30%2e%30%2e%31 = 127.0.0.1).
    let host = percent_decode_str(&host).decode_utf8_lossy().into_owned();
    Some(host)
}

/// Extract the port from a URL, defaulting to 80 for http and 443 for https.
fn extract_port_from_url(url: &str) -> u16 {
    url::Url::parse(url)
        .ok()
        .and_then(|u| u.port_or_known_default())
        .unwrap_or(80)
}

/// Resolve the URL's host, check for internal IPs, and return a `reqwest::Client`
/// with DNS pinned to the resolved address. This prevents DNS rebinding between
/// the check and the TCP connect.
///
/// Returns:
/// - `Ok(Some(client))` for hostnames that resolve to public IPs (pinned client)
/// - `Ok(None)` for literal-IP URLs (no rebinding risk) or resolution failures
/// - `Err(())` if the host resolves to an internal IP (deny the request)
fn resolve_and_pin_dns(url: &str) -> Result<Option<reqwest::Client>, ()> {
    let host = extract_host(url).ok_or(())?;
    // Literal IPs are already pinned in the URL; no rebinding risk.
    if host.parse::<IpAddr>().is_ok() {
        return Ok(None);
    }
    let port = extract_port_from_url(url);
    let probe = format!("{host}:{port}");
    let addrs: Vec<SocketAddr> = match probe.to_socket_addrs() {
        Ok(a) => a.collect(),
        Err(_) => return Ok(None), // resolution failure — let request fail later
    };
    if addrs.is_empty() {
        return Ok(None);
    }
    for addr in &addrs {
        if is_internal_addr(&addr.ip()) {
            return Err(()); // deny: resolves to internal IP
        }
    }
    // Pin to the first resolved address.
    // ponytail: only pins one IP; if that IP is down, the request fails
    // instead of trying alternates. Acceptable for SSRF prevention.
    let pin_addr = addrs[0];
    let client = reqwest::Client::builder()
        .timeout(FETCH_TIMEOUT)
        .user_agent(USER_AGENT)
        .resolve(&host, pin_addr)
        .build()
        .unwrap_or_else(|_| reqwest::Client::new());
    Ok(Some(client))
}

fn looks_like_html(body: &str) -> bool {
    let lower = body.to_ascii_lowercase();
    lower.contains("<!doctype html")
        || lower.contains("<html")
        || lower.contains("<head")
        || lower.contains("<body")
}

/// Lightweight regex-only HTML-to-markdown converter.
///
/// Converts common HTML elements to markdown equivalents; unrecognized tags
/// are stripped. Table content is not converted (regex breaks on nested HTML
/// tables) — table tags are stripped and cell text passes through as plain
/// text.
///
/// ponytail: a real parser (html5ever / scraper) adds a dependency and attack
/// surface. Regex is sufficient for model consumption. Whitespace inside
/// fenced code blocks is collapsed (same as the rest of the output); a real
/// parser would preserve it.
fn html_to_text(html: &str) -> String {
    static HEAD_RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    static SCRIPT_RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    static STYLE_RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    static CB_RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    static PRE_RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    static IC_RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    static H_RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    static LINK_RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    static STRONG_RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    static EM_RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    static HR_RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    static BR_RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    static P_RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    static LIST_RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    static TAG_RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    static WS_RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();

    let head = HEAD_RE.get_or_init(|| regex::Regex::new(r"(?is)<head[^>]*>.*?</head>").unwrap());
    let script =
        SCRIPT_RE.get_or_init(|| regex::Regex::new(r"(?is)<script[^>]*>.*?</script>").unwrap());
    let style =
        STYLE_RE.get_or_init(|| regex::Regex::new(r"(?is)<style[^>]*>.*?</style>").unwrap());

    let s = head.replace_all(html, "");
    let s = script.replace_all(&s, " ");
    let s = style.replace_all(&s, " ");

    let cb = CB_RE.get_or_init(|| {
        regex::Regex::new(r"(?is)<pre[^>]*>\s*<code[^>]*>(.*?)</code>\s*</pre>").unwrap()
    });
    let s = cb.replace_all(&s, |c: &regex::Captures| {
        format!(
            "\n```\n{}\n```\n",
            c.get(1).map(|m| m.as_str()).unwrap_or("")
        )
    });

    let pre = PRE_RE.get_or_init(|| regex::Regex::new(r"(?is)<pre[^>]*>(.*?)</pre>").unwrap());
    let s = pre.replace_all(&s, |c: &regex::Captures| {
        format!(
            "\n```\n{}\n```\n",
            c.get(1).map(|m| m.as_str()).unwrap_or("")
        )
    });

    let ic = IC_RE.get_or_init(|| regex::Regex::new(r"(?i)<code\b[^>]*>(.*?)</code>").unwrap());
    let s = ic.replace_all(&s, |c: &regex::Captures| {
        format!("`{}`", c.get(1).map(|m| m.as_str()).unwrap_or(""))
    });

    let h = H_RE.get_or_init(|| regex::Regex::new(r"(?i)<h([1-6])[^>]*>(.*?)</h[1-6]>").unwrap());
    let s = h.replace_all(&s, |c: &regex::Captures| {
        let n = c
            .get(1)
            .and_then(|m| m.as_str().parse::<usize>().ok())
            .unwrap_or(1);
        format!(
            "{} {}\n",
            "#".repeat(n),
            c.get(2).map(|m| m.as_str()).unwrap_or("").trim()
        )
    });

    let link = LINK_RE.get_or_init(|| {
        regex::Regex::new(r#"(?i)<a\b[^>]*href=["']([^"']+)["'][^>]*>(.*?)</a>"#).unwrap()
    });
    let s = link.replace_all(&s, "[$2]($1)");

    let strong =
        STRONG_RE.get_or_init(|| regex::Regex::new(r"(?i)</?(?:strong|b)\b[^>]*>").unwrap());
    let s = strong.replace_all(&s, "**");

    let em = EM_RE.get_or_init(|| regex::Regex::new(r"(?i)</?(?:em|i)\b[^>]*>").unwrap());
    let s = em.replace_all(&s, "*");

    let hr = HR_RE.get_or_init(|| regex::Regex::new(r"(?i)<hr\b\s*/?>").unwrap());
    let s = hr.replace_all(&s, "\n---\n");

    let br = BR_RE.get_or_init(|| regex::Regex::new(r"(?i)<br\b\s*/?>").unwrap());
    let s = br.replace_all(&s, "\n");

    let p = P_RE.get_or_init(|| regex::Regex::new(r"(?i)</?p\b[^>]*>").unwrap());
    let s = p.replace_all(&s, "\n\n");

    let list = LIST_RE
        .get_or_init(|| regex::Regex::new(r"(?i)<(?:ul|ol|li)\b[^>]*>|</(?:ul|ol)\b>").unwrap());
    let mut stack: Vec<bool> = Vec::new();
    let s = list.replace_all(&s, |c: &regex::Captures| {
        let t = c.get(0).unwrap().as_str().to_ascii_lowercase();
        if t.starts_with("<ol") {
            stack.push(true);
            "\n".into()
        } else if t.starts_with("<ul") {
            stack.push(false);
            "\n".into()
        } else if t == "</ol>" || t == "</ul>" {
            stack.pop();
            "\n".into()
        } else {
            let indent = "  ".repeat(stack.len().saturating_sub(1));
            let bullet = if stack.last().copied().unwrap_or(false) {
                "1."
            } else {
                "-"
            };
            format!("\n{indent}{bullet} ")
        }
    });

    let tag = TAG_RE.get_or_init(|| regex::Regex::new(r"<[^>]+>").unwrap());
    let s = tag.replace_all(&s, " ");

    let s = html_entities::decode(&s);

    let ws = WS_RE.get_or_init(|| regex::Regex::new(r"[ \t]+").unwrap());
    s.lines()
        .map(|line| {
            let leading = line.len() - line.trim_start().len();
            let body = ws.replace_all(&line[leading..], " ").trim_end().to_string();
            if body.is_empty() {
                String::new()
            } else {
                format!("{}{body}", &line[..leading])
            }
        })
        .filter(|line| !line.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n")
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
            .run(
                &ToolContext::new(),
                json!({"url": "http://169.254.169.254/latest/meta-data/"}),
            )
            .await;
        assert!(
            matches!(
                outcome,
                ToolOutcome::Failure(ToolError::AccessDenied { .. })
            ),
            "expected denied metadata endpoint, got {outcome:?}"
        );
    }

    #[tokio::test]
    async fn rejects_literal_internal_ip() {
        let tool = WebFetch::new(DenyList::default());
        let outcome = tool
            .run(
                &ToolContext::new(),
                json!({"url": "http://127.0.0.1:8080/secret"}),
            )
            .await;
        assert!(
            matches!(
                outcome,
                ToolOutcome::Failure(ToolError::AccessDenied { .. })
            ),
            "expected denied internal IP, got {outcome:?}"
        );
    }

    // WO 15.3: a hostname that resolves to an internal IP via the system
    // resolver must be rejected even though it's not a literal IP. `localhost`
    // resolves to 127.0.0.1 on every CI host, so this is a reliable real
    // (non-mocked) DNS-rebinding guard test.
    #[tokio::test]
    async fn rejects_hostname_resolving_to_internal_ip() {
        let tool = WebFetch::new(DenyList::default());
        let outcome = tool
            .run(&ToolContext::new(), json!({"url": "http://localhost/"}))
            .await;
        assert!(
            matches!(
                outcome,
                ToolOutcome::Failure(ToolError::AccessDenied { .. })
            ),
            "expected AccessDenied for localhost (resolves to 127.0.0.1), got {outcome:?}"
        );
    }

    #[test]
    fn host_resolves_to_internal_ip_localhost_is_true() {
        // localhost resolves to 127.0.0.1 (or ::1) on every host.
        assert!(
            host_resolves_to_internal_ip("http://localhost/"),
            "localhost should resolve to an internal IP"
        );
    }

    #[test]
    fn host_resolves_to_internal_ip_literal_ip_is_false() {
        // Literal IPs are handled by `host_is_literal_internal_ip`; the
        // resolver guard must NOT re-resolve them (avoids TOCTOU on a
        // pinned literal and avoids double-denying, which would still be
        // safe but is not this function's job).
        assert!(!host_resolves_to_internal_ip("http://127.0.0.1/"));
        assert!(!host_resolves_to_internal_ip("http://8.8.8.8/"));
    }

    #[test]
    fn host_resolves_to_internal_ip_nonexistent_host_is_false() {
        // A hostname that does not resolve at all should NOT trip the
        // guard — the fetch will fail later at connect time. Failing closed
        // on every NXDOMAIN would break clients that pin DNS inside reqwest
        // (tests) rather than the system resolver.
        assert!(
            !host_resolves_to_internal_ip("http://kf-code-nonexistent-host-zzz.invalid/"),
            "NXDOMAIN should not trip the internal-IP guard"
        );
    }

    #[test]
    fn host_resolves_to_internal_ip_malformed_is_true() {
        // Malformed URL -> extract_host returns None -> fail closed.
        assert!(host_resolves_to_internal_ip(""));
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
            .respond_with(
                wiremock::ResponseTemplate::new(200)
                    .set_body_string(body)
                    .insert_header("content-type", "application/json"),
            )
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
            .respond_with(
                wiremock::ResponseTemplate::new(200)
                    .set_body_string(html)
                    .insert_header("content-type", "text/html"),
            )
            .mount(&server)
            .await;

        let tool = test_tool_for(&server);
        let outcome = tool
            .run(
                &ToolContext::new(),
                json!({"url": "http://test.local/page"}),
            )
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

    #[tokio::test]
    async fn rejects_empty_url() {
        let tool = WebFetch::new(DenyList::default());
        let outcome = tool.run(&ToolContext::new(), json!({"url": "   "})).await;
        match outcome {
            ToolOutcome::Failure(ToolError::InvalidArgs { message }) => {
                assert!(message.contains("URL is empty"), "got {message}");
            }
            other => panic!("expected InvalidArgs, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn rejects_missing_url_arg() {
        let tool = WebFetch::new(DenyList::default());
        let outcome = tool.run(&ToolContext::new(), json!({})).await;
        match outcome {
            ToolOutcome::Failure(ToolError::InvalidArgs { message }) => {
                assert!(message.contains("Missing 'url'"), "got {message}");
            }
            other => panic!("expected InvalidArgs, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn rejects_ftp_scheme() {
        let tool = WebFetch::new(DenyList::default());
        let outcome = tool
            .run(
                &ToolContext::new(),
                json!({"url": "ftp://example.com/file"}),
            )
            .await;
        match outcome {
            ToolOutcome::Failure(ToolError::AccessDenied { message }) => {
                assert!(message.contains("Only http"), "got {message}");
            }
            other => panic!("expected AccessDenied, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn rejects_https_to_metadata_endpoint() {
        let tool = WebFetch::new(DenyList::default());
        let outcome = tool
            .run(
                &ToolContext::new(),
                json!({"url": "http://metadata.google.internal/computeMetadata/v1/"}),
            )
            .await;
        assert!(
            matches!(
                outcome,
                ToolOutcome::Failure(ToolError::AccessDenied { .. })
            ),
            "got {outcome:?}"
        );
    }

    #[tokio::test]
    async fn rejects_link_local_v4_literal() {
        let tool = WebFetch::new(DenyList::default());
        let outcome = tool
            .run(&ToolContext::new(), json!({"url": "http://169.254.10.20/"}))
            .await;
        assert!(
            matches!(
                outcome,
                ToolOutcome::Failure(ToolError::AccessDenied { .. })
            ),
            "got {outcome:?}"
        );
    }

    #[tokio::test]
    async fn rejects_unspecified_v4_literal() {
        let tool = WebFetch::new(DenyList::default());
        let outcome = tool
            .run(&ToolContext::new(), json!({"url": "http://0.0.0.0/"}))
            .await;
        assert!(
            matches!(
                outcome,
                ToolOutcome::Failure(ToolError::AccessDenied { .. })
            ),
            "got {outcome:?}"
        );
    }

    #[tokio::test]
    async fn rejects_private_rfc1918_literal() {
        let tool = WebFetch::new(DenyList::default());
        for url in [
            "http://10.0.0.1/",
            "http://192.168.1.1/",
            "http://172.16.0.1/",
        ] {
            let outcome = tool.run(&ToolContext::new(), json!({"url": url})).await;
            assert!(
                matches!(
                    outcome,
                    ToolOutcome::Failure(ToolError::AccessDenied { .. })
                ),
                "{url} should be denied, got {outcome:?}"
            );
        }
    }

    #[tokio::test]
    async fn rejects_ipv6_loopback_literal() {
        let tool = WebFetch::new(DenyList::default());
        let outcome = tool
            .run(&ToolContext::new(), json!({"url": "http://[::1]/"}))
            .await;
        assert!(
            matches!(
                outcome,
                ToolOutcome::Failure(ToolError::AccessDenied { .. })
            ),
            "got {outcome:?}"
        );
    }

    #[tokio::test]
    async fn rejects_ipv6_link_local_literal() {
        let tool = WebFetch::new(DenyList::default());
        let outcome = tool
            .run(&ToolContext::new(), json!({"url": "http://[fe80::1]/"}))
            .await;
        assert!(
            matches!(
                outcome,
                ToolOutcome::Failure(ToolError::AccessDenied { .. })
            ),
            "got {outcome:?}"
        );
    }

    #[tokio::test]
    async fn rejects_ipv6_unique_local_literal() {
        let tool = WebFetch::new(DenyList::default());
        let outcome = tool
            .run(&ToolContext::new(), json!({"url": "http://[fd00::1]/"}))
            .await;
        assert!(
            matches!(
                outcome,
                ToolOutcome::Failure(ToolError::AccessDenied { .. })
            ),
            "got {outcome:?}"
        );
    }

    #[tokio::test]
    async fn public_hostname_passes_initial_guards() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_string("ok"))
            .mount(&server)
            .await;
        let tool = test_tool_for(&server);
        let outcome = tool
            .run(&ToolContext::new(), json!({"url": "http://test.local/"}))
            .await;
        assert!(
            matches!(outcome, ToolOutcome::Success { .. }),
            "got {outcome:?}"
        );
    }

    #[test]
    fn host_is_literal_internal_ip_empty_url_returns_true() {
        // Empty URL: extract_host returns None -> fail closed (true).
        assert!(host_is_literal_internal_ip(""));
    }

    #[test]
    fn host_is_literal_internal_ip_empty_host_after_scheme_returns_true() {
        // "://nothing" has scheme separator but extract_host returns
        // Some("nothing") which is not an IP -> false. Only truly empty
        // hosts return None and trigger fail-closed.
        assert!(!host_is_literal_internal_ip("://nothing"));
    }

    #[test]
    fn host_is_literal_internal_ip_non_ip_hostname_is_false() {
        // "not a url" has no scheme so extract_host returns the whole string
        // as the host, which is not a parseable IP -> false.
        assert!(!host_is_literal_internal_ip("not a url"));
    }

    #[test]
    fn host_is_literal_internal_ip_public_hostname_is_false() {
        assert!(!host_is_literal_internal_ip("http://example.com/path"));
        assert!(!host_is_literal_internal_ip("https://api.github.com/x"));
        assert!(!host_is_literal_internal_ip("http://8.8.8.8/"));
    }

    #[test]
    fn host_is_literal_internal_ip_internal_v4_is_true() {
        for url in [
            "http://127.0.0.1/x",
            "http://10.0.0.1/x",
            "http://192.168.1.1/x",
            "http://172.16.0.1/x",
            "http://0.0.0.0/x",
            "http://169.254.1.1/x",
        ] {
            assert!(host_is_literal_internal_ip(url), "{url} should be internal");
        }
    }

    #[test]
    fn host_is_literal_internal_ip_internal_v6_is_true() {
        assert!(host_is_literal_internal_ip("http://[::1]/x"));
        assert!(host_is_literal_internal_ip("http://[fe80::1]/x"));
        assert!(host_is_literal_internal_ip("http://[fd00::1]/x"));
    }

    #[test]
    fn host_is_literal_internal_ip_percent_encoded_is_true() {
        // Percent-encoded 127.0.0.1: %31%32%37%2e%30%2e%30%2e%31
        assert!(host_is_literal_internal_ip(
            "http://%31%32%37%2e%30%2e%30%2e%31/x"
        ));
        // Percent-encoded 10.0.0.1: %31%30%2e%30%2e%30%2e%31
        assert!(host_is_literal_internal_ip(
            "http://%31%30%2e%30%2e%30%2e%31/x"
        ));
        // Percent-encoded 169.254.169.254: %31%36%39%2e%32%35%34%2e%31%36%39%2e%32%35%34
        assert!(host_is_literal_internal_ip(
            "http://%31%36%39%2e%32%35%34%2e%31%36%39%2e%32%35%34/"
        ));
    }

    #[test]
    fn extract_host_strips_scheme_and_port() {
        assert_eq!(
            extract_host("http://example.com:8080/p").as_deref(),
            Some("example.com")
        );
        assert_eq!(
            extract_host("https://example.com/p").as_deref(),
            Some("example.com")
        );
    }

    #[test]
    fn extract_host_strips_userinfo() {
        assert_eq!(
            extract_host("http://user:pass@example.com/p").as_deref(),
            Some("example.com")
        );
    }

    #[test]
    fn extract_host_strips_query_and_fragment() {
        assert_eq!(
            extract_host("http://example.com/p?x=1#frag").as_deref(),
            Some("example.com")
        );
        assert_eq!(
            extract_host("http://example.com?x=1").as_deref(),
            Some("example.com")
        );
    }

    #[test]
    fn extract_host_bracketed_ipv6_strips_port() {
        let h = extract_host("http://[::1]:8080/x");
        // The bracket-with-port form is parsed as malformed (returns None)
        // because the host_port does not end with ']' and contains multiple ':'.
        assert!(h.is_none(), "got {h:?}");
    }

    #[test]
    fn extract_host_bracketed_ipv6_without_port() {
        let h = extract_host("http://[::1]/x").unwrap();
        assert_eq!(h, "::1");
    }

    #[test]
    fn extract_host_no_scheme_returns_whole_host() {
        assert_eq!(
            extract_host("example.com/p").as_deref(),
            Some("example.com")
        );
    }

    #[test]
    fn extract_host_empty_after_strip_returns_none() {
        assert_eq!(extract_host("http:///path"), None);
        assert_eq!(extract_host("://"), None);
    }

    #[test]
    fn extract_host_malformed_ipv6_without_brackets_returns_none() {
        assert!(extract_host("http://::1:8080/x").is_none());
    }

    #[test]
    fn looks_like_html_detects_common_tags() {
        assert!(looks_like_html("<!doctype html><html>"));
        assert!(looks_like_html("<html><head></head><body>x</body></html>"));
        assert!(looks_like_html("<body>x</body>"));
        assert!(looks_like_html("<head><title>x</title></head>"));
    }

    #[test]
    fn looks_like_html_rejects_plain_text() {
        assert!(!looks_like_html("just plain text"));
        assert!(!looks_like_html("{ \"json\": \"value\" }"));
        assert!(!looks_like_html(""));
    }

    #[test]
    fn html_to_text_strips_script_and_style_blocks() {
        let html = "<html><head><script>alert(1)</script><style>.x{}</style></head><body>text</body></html>";
        let out = html_to_text(html);
        assert!(out.contains("text"), "got: {out}");
        assert!(
            !out.contains("alert"),
            "script content should be gone: {out}"
        );
        assert!(!out.contains(".x{}"), "style content should be gone: {out}");
    }

    #[test]
    fn html_to_text_strips_all_tags() {
        let html = "<h1>Title</h1><p>Body <b>bold</b></p>";
        let out = html_to_text(html);
        assert!(out.contains("Title"));
        assert!(out.contains("Body"));
        assert!(out.contains("bold"));
        assert!(!out.contains("<h1>"));
        assert!(!out.contains("<p>"));
        assert!(!out.contains("<b>"));
    }

    #[test]
    fn html_to_text_decodes_named_entities() {
        let html = "<p>a &amp; b &lt; c &gt; d &quot; e &apos; f &nbsp; g</p>";
        let out = html_to_text(html);
        assert!(out.contains("a & b"), "got: {out}");
        assert!(out.contains("< c"), "got: {out}");
        assert!(out.contains("> d"), "got: {out}");
        assert!(out.contains("\" e"), "got: {out}");
        assert!(out.contains("' f"), "got: {out}");
    }

    #[test]
    fn html_to_text_decodes_numeric_entities() {
        let html = "<p>&#65;&#66;&#67;</p>";
        let out = html_to_text(html);
        assert!(out.contains("A"), "got: {out}");
        assert!(out.contains("B"), "got: {out}");
        assert!(out.contains("C"), "got: {out}");
    }

    #[test]
    fn html_to_text_preserves_hex_entities_verbatim() {
        // The decoder handles decimal &#NN; but not hex &#xNN;. Hex entities
        // are preserved as-is (the unknown-entity path).
        let html = "<p>&#x42;</p>";
        let out = html_to_text(html);
        assert!(
            out.contains("&#x42;"),
            "hex entity preserved verbatim: {out}"
        );
    }

    #[test]
    fn html_to_text_preserves_unknown_entities_verbatim() {
        let html = "<p>&unknownentity;</p>";
        let out = html_to_text(html);
        assert!(out.contains("&unknownentity;"), "got: {out}");
    }

    #[test]
    fn html_to_text_handles_unterminated_ampersand() {
        let html = "<p>foo & bar</p>";
        let out = html_to_text(html);
        assert!(out.contains("foo & bar"), "got: {out}");
    }

    #[test]
    fn html_to_text_collapses_repeated_inline_whitespace() {
        let html = "<p>line    with     spaces</p>";
        let out = html_to_text(html);
        assert!(out.contains("line with spaces"), "got: {out}");
        assert!(!out.contains("    "), "got: {out}");
    }

    #[test]
    fn html_to_text_drops_empty_lines() {
        let html = "<p>a</p>\n\n\n<p>b</p>";
        let out = html_to_text(html);
        let lines: Vec<&str> = out.lines().collect();
        assert!(lines.iter().all(|l| !l.trim().is_empty()), "got: {out}");
        assert!(lines.iter().any(|l| l.contains("a")));
        assert!(lines.iter().any(|l| l.contains("b")));
    }

    #[test]
    fn html_to_text_strips_head_content() {
        let html = "<html><head><title>Hidden</title><meta charset='utf-8'></head><body>visible</body></html>";
        let out = html_to_text(html);
        assert!(out.contains("visible"), "body text missing: {out}");
        assert!(!out.contains("Hidden"), "title should be stripped: {out}");
    }

    #[test]
    fn html_to_text_converts_headings() {
        let html = "<h1>One</h1><h2>Two</h2><h3>Three</h3><h6>Six</h6>";
        let out = html_to_text(html);
        assert!(out.contains("# One"), "h1: {out}");
        assert!(out.contains("## Two"), "h2: {out}");
        assert!(out.contains("### Three"), "h3: {out}");
        assert!(out.contains("###### Six"), "h6: {out}");
    }

    #[test]
    fn html_to_text_converts_unordered_list() {
        let html = "<ul><li>alpha</li><li>beta</li></ul>";
        let out = html_to_text(html);
        assert!(out.contains("- alpha"), "bullet: {out}");
        assert!(out.contains("- beta"), "bullet: {out}");
    }

    #[test]
    fn html_to_text_converts_ordered_list() {
        let html = "<ol><li>first</li><li>second</li></ol>";
        let out = html_to_text(html);
        assert!(out.contains("1. first"), "numbered: {out}");
        assert!(out.contains("1. second"), "numbered: {out}");
    }

    #[test]
    fn html_to_text_indents_nested_lists() {
        let html = "<ul><li>top<ul><li>nested</li></ul></li></ul>";
        let out = html_to_text(html);
        assert!(out.contains("- top"), "top item: {out}");
        assert!(out.contains("  - nested"), "nested indent: {out}");
    }

    #[test]
    fn html_to_text_converts_code_blocks() {
        let html = "<pre><code>fn main() {\n    println!(\"hi\");\n}</code></pre>";
        let out = html_to_text(html);
        assert!(out.contains("```"), "fenced: {out}");
        assert!(out.contains("fn main()"), "code body: {out}");
    }

    #[test]
    fn html_to_text_converts_inline_code() {
        let html = "<p>use <code>std::fs</code> for files</p>";
        let out = html_to_text(html);
        assert!(out.contains("`std::fs`"), "backtick: {out}");
    }

    #[test]
    fn html_to_text_converts_links() {
        let html = r#"<a href="https://example.com">click here</a>"#;
        let out = html_to_text(html);
        assert!(
            out.contains("[click here](https://example.com)"),
            "link: {out}"
        );
    }

    #[test]
    fn html_to_text_converts_bold_and_italic() {
        let html = "<p><strong>bold</strong> and <b>also bold</b>, <em>italic</em> and <i>also italic</i></p>";
        let out = html_to_text(html);
        assert!(out.contains("**bold**"), "strong: {out}");
        assert!(out.contains("**also bold**"), "b: {out}");
        assert!(out.contains("*italic*"), "em: {out}");
        assert!(out.contains("*also italic*"), "i: {out}");
    }

    #[test]
    fn html_to_text_converts_hr_and_br() {
        let html = "<p>above</p><hr/><p>middle</p><p>line1<br>line2</p>";
        let out = html_to_text(html);
        assert!(out.contains("---"), "hr: {out}");
        let lines: Vec<&str> = out.lines().collect();
        let line_idx = lines.iter().position(|l| l.contains("line1")).unwrap();
        assert_eq!(lines[line_idx + 1], "line2", "br split: {out}");
    }

    #[test]
    fn html_entities_decode_handles_empty_string() {
        assert_eq!(html_entities::decode(""), "");
    }

    #[test]
    fn html_entities_decode_no_entities_returns_input() {
        assert_eq!(html_entities::decode("plain text"), "plain text");
    }

    #[test]
    fn html_entities_decode_decodes_nbsp_and_dashes() {
        assert_eq!(html_entities::decode("a&nbsp;b"), "a b");
        assert_eq!(html_entities::decode("&ndash;"), "\u{2013}");
        assert_eq!(html_entities::decode("&mdash;"), "\u{2014}");
    }

    #[test]
    fn def_is_valid_json_schema() {
        let tool = WebFetch::new(DenyList::default());
        let def = tool.def();
        assert_eq!(def.name, "web_fetch");
        assert!(def.parameters.get("properties").is_some());
        let required = def
            .parameters
            .get("required")
            .and_then(|r| r.as_array())
            .expect("required array");
        assert!(required.iter().any(|v| v.as_str() == Some("url")));
    }
}
