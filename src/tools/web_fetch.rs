use crate::shared::access::DenyList;
use crate::shared::{ToolDef, ToolError, ToolOutcome};
use crate::tools::{Tool, ToolContext};
use percent_encoding::percent_decode_str;
use std::net::{IpAddr, SocketAddr, ToSocketAddrs};
use std::sync::Arc;
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

// Abstraction over DNS resolution so the SSRF guards can be unit-tested
// without real NXDOMAIN I/O (~5s per lookup). Production uses `SystemResolver`
// (wraps `std::net::ToSocketAddrs`); tests inject a fake. The trait is sync+
// object-safe so a `Box<dyn DnsResolver>` can be cloned cheaply via `Arc`.
// ponytail: only the two guard functions that do real DNS (`host_resolves_`
// `to_internal_ip` and `resolve_and_pin_dns`) consult the resolver; the
// literal-IP guard is pure string parsing and stays resolver-free.
pub trait DnsResolver: Send + Sync {
    // Resolve `host:port` to a list of socket addresses.
    // Mirrors `ToSocketAddrs::to_socket_addrs`: `Ok` yields addrs, `Err` means
    // resolution failure (NXDOMAIN / network error).
    fn resolve(&self, host_port: &str) -> std::io::Result<Vec<SocketAddr>>;
}

// Production resolver: delegates to the OS resolver via `to_socket_addrs`.
// `Arc` inner lets the `SystemResolver` itself be cloned (it's a unit struct,
// so cloning is free, but `Arc` lets us share it via `Box<dyn>`).
#[derive(Clone, Default)]
pub struct SystemResolver;

impl DnsResolver for SystemResolver {
    fn resolve(&self, host_port: &str) -> std::io::Result<Vec<SocketAddr>> {
        host_port.to_socket_addrs().map(|i| i.collect())
    }
}

// A resolver handle held by `WebFetch`. `Arc<dyn>` so cloning the tool clones
// the resolver reference, not the (stateless) resolver itself.
type ResolverHandle = Arc<dyn DnsResolver>;

// WO 38.3: run both resolver-consuming SSRF guards on a blocking thread.
// Returns (host_resolves_to_internal_ip, resolve_and_pin_dns); a panic or
// join failure in the blocking task fails closed on both.
async fn resolve_guards_off_worker(
    url: String,
    resolver: ResolverHandle,
) -> (bool, Result<Option<reqwest::Client>, ()>) {
    tokio::task::spawn_blocking(move || {
        (
            host_resolves_to_internal_ip(&url, &*resolver),
            resolve_and_pin_dns(&url, &*resolver),
        )
    })
    .await
    .unwrap_or((true, Err(())))
}

pub struct WebFetch {
    deny_list: DenyList,
    client: reqwest::Client,
    resolver: ResolverHandle,
}

impl WebFetch {
    pub fn new(deny_list: DenyList) -> Self {
        let client = reqwest::Client::builder()
            .timeout(FETCH_TIMEOUT)
            .user_agent(USER_AGENT)
            // ponytail: do not follow redirects. The top-level SSRF checks
            // (scheme allowlist, literal-internal-IP rejection, DNS resolve,
            // DNS pinning) run only on the initial URL; reqwest would
            // follow a 302 to an internal IP (e.g. 169.254.169.254) without
            // re-validating. Surfacing 3xx to the model costs one extra
            // round-trip on legitimate redirects but closes the bypass.
            // Upgrade path: a custom Policy that re-runs host checks on
            // each Location (requires sync DNS lookup in the policy).
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap_or_else(|_| fallback_client());
        Self {
            deny_list,
            client,
            resolver: Arc::new(SystemResolver),
        }
    }

    #[cfg(test)]
    fn with_resolver(
        deny_list: DenyList,
        client: reqwest::Client,
        resolver: ResolverHandle,
    ) -> Self {
        Self {
            deny_list,
            client,
            resolver,
        }
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

    async fn run(&self, ctx: &ToolContext, args: serde_json::Value) -> ToolOutcome {
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

        // WO 38.3: DNS resolution (getaddrinfo) is blocking — run both
        // guard resolutions on a blocking thread so a wedged resolver
        // cannot stall a runtime worker. A panic in the blocking task
        // fails closed.
        // WO 46.37: race the guard await against the cancel token —
        // getaddrinfo can hang for seconds and a cancelled turn must not
        // wait it out. The blocking-pool task runs to completion; only
        // the tool return is prompt. Pattern: `tools/grep.rs:102`.
        let (resolves_internal, pin) = tokio::select! {
            biased;
            _ = ctx.token.cancelled() => {
                return ToolOutcome::Failure(ToolError::Cancelled);
            }
            g = resolve_guards_off_worker(trimmed.to_string(), self.resolver.clone()) => g,
        };
        if resolves_internal {
            return ToolOutcome::Failure(ToolError::AccessDenied {
                message: "URL host resolves to a private/internal IP".into(),
            });
        }

        // DNS-rebinding guard: resolve the host once, check for internal
        // IPs, and pin DNS to the resolved address so the TCP connect uses
        // the same IP we checked. ponytail: builds a new reqwest::Client
        // per hostname request; cache pinned clients if throughput matters.
        let client = match pin {
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

        // WO 46.37: race the request against the cancel token so a
        // cancelled turn doesn't wait out FETCH_TIMEOUT (30s). Dropping
        // the in-flight future aborts the request.
        let response = tokio::select! {
            biased;
            _ = ctx.token.cancelled() => {
                return ToolOutcome::Failure(ToolError::Cancelled);
            }
            r = client.execute(request) => match r {
                Ok(r) => r,
                Err(e) => {
                    return ToolOutcome::Failure(ToolError::Internal {
                        message: format!("Failed to fetch {trimmed}: {e}"),
                    });
                }
            },
        };

        let status = response.status();
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_ascii_lowercase();

        // Stream the body incrementally and abort the moment we cross
        // MAX_BODY_BYTES. `response.bytes().await` would buffer the entire
        // body first — a server streaming multi-GB within the 30s timeout
        // would OOM the process before the post-hoc cap fired (WO 46.21).
        // WO 46.37: each chunk await is raced against the cancel token so
        // a slow-drip body can't hold a cancelled turn hostage.
        use tokio_stream::StreamExt;
        let mut stream = response.bytes_stream();
        let mut buf: Vec<u8> = Vec::new();
        let mut exceeded = false;
        while let Some(chunk_res) = tokio::select! {
            biased;
            _ = ctx.token.cancelled() => {
                return ToolOutcome::Failure(ToolError::Cancelled);
            }
            c = stream.next() => c,
        } {
            let chunk = match chunk_res {
                Ok(c) => c,
                Err(e) => {
                    return ToolOutcome::Failure(ToolError::Internal {
                        message: format!("Failed to read response body from {trimmed}: {e}"),
                    });
                }
            };
            // Enforce the cap BEFORE extending, so we never hold an
            // oversized buffer even briefly.
            if buf.len() + chunk.len() > MAX_BODY_BYTES {
                exceeded = true;
                break;
            }
            buf.extend_from_slice(&chunk);
        }
        if exceeded {
            return ToolOutcome::Failure(ToolError::Internal {
                message: format!("Response from {trimmed} exceeds {MAX_BODY_BYTES} byte cap"),
            });
        }

        let body_bytes: &[u8] = &buf;

        if !status.is_success() {
            let preview = String::from_utf8_lossy(body_bytes)
                .chars()
                .take(200)
                .collect::<String>();
            return ToolOutcome::Failure(ToolError::Execution {
                message: format!("HTTP {status} from {trimmed}"),
                exit_code: Some(status.as_u16() as i32),
                stderr: preview,
            });
        }

        let raw = String::from_utf8_lossy(body_bytes).into_owned();
        let output = if content_type.contains("text/html") || looks_like_html(&raw) {
            html_to_text(&raw)
        } else {
            raw
        };

        let content = if output.len() > DEFAULT_MAX_TOOL_RESULT_CHARS {
            // ponytail: floor_char_boundary is unstable on this toolchain; find the
            // last char boundary at or before the cap manually. Upgrade path: use
            // `output.floor_char_boundary(cap)` once it stabilizes.
            let cut = output
                .char_indices()
                .take_while(|(i, _)| *i < DEFAULT_MAX_TOOL_RESULT_CHARS)
                .last()
                .map(|(i, _)| i)
                .unwrap_or(0);
            format!(
                "{}\n\n[truncated {} characters]",
                &output[..cut],
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
// literal. Returns TRUE on resolution *error* (WO 38.3: fail closed) — a
// non-literal host whose resolution fails is treated as hostile rather
// than deferred to connect time. The earlier fail-open behavior existed
// only so tests could pin DNS inside the reqwest client; tests now inject
// a resolver that returns `Ok(vec![])` for that path instead.
//
// WO 33.14: the resolver is injected (`DnsResolver` trait) so tests can avoid
// real NXDOMAIN I/O. Production passes `SystemResolver`.
pub(crate) fn host_resolves_to_internal_ip(url: &str, resolver: &dyn DnsResolver) -> bool {
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
    match resolver.resolve(&probe) {
        Ok(addrs) => addrs
            .iter()
            .map(|sa| sa.ip())
            .any(|addr| is_internal_addr(&addr)),
        Err(_) => true, // resolution error -> fail closed (WO 38.3)
    }
}

fn is_internal_addr(addr: &std::net::IpAddr) -> bool {
    match addr {
        std::net::IpAddr::V4(v4) => {
            v4.is_loopback() || v4.is_unspecified() || v4.is_private() || is_link_local_v4(v4)
        }
        std::net::IpAddr::V6(v6) => {
            // WO 38.3: an IPv4-mapped address (::ffff:a.b.c.d) carries an
            // IPv4 internal target — check it with the V4 rules so
            // http://[::ffff:169.254.169.254]/ is denied instead of
            // sailing past the V6-only checks.
            if let Some(v4) = v6.to_ipv4_mapped() {
                return is_internal_addr(&std::net::IpAddr::V4(v4));
            }
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
/// - `Ok(None)` for literal-IP URLs (no rebinding risk) or empty resolutions
/// - `Err(())` if the host resolves to an internal IP (deny the request) or
///   resolution FAILS (WO 38.3: fail closed for non-literal hosts)
fn resolve_and_pin_dns(
    url: &str,
    resolver: &dyn DnsResolver,
) -> Result<Option<reqwest::Client>, ()> {
    let host = extract_host(url).ok_or(())?;
    // Literal IPs are already pinned in the URL; no rebinding risk.
    if host.parse::<IpAddr>().is_ok() {
        return Ok(None);
    }
    let port = extract_port_from_url(url);
    let probe = format!("{host}:{port}");
    let addrs: Vec<SocketAddr> = match resolver.resolve(&probe) {
        Ok(a) => a,
        Err(_) => return Err(()), // resolution failure -> fail closed (WO 38.3)
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
        .unwrap_or_else(|_| fallback_client());
    Ok(Some(client))
}

/// Fallback client when the pinned-client builder fails: keeps the
/// fetch timeout instead of reverting to reqwest's unbounded default
/// (WO 38.3 — fallback clients get a plain timeout too).
fn fallback_client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(FETCH_TIMEOUT)
        .build()
        .unwrap_or_else(|_| reqwest::Client::new())
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

    let head = HEAD_RE.get_or_init(|| {
        regex::Regex::new(r"(?is)<head[^>]*>.*?</head>").expect("static regex literal")
    });
    let script = SCRIPT_RE.get_or_init(|| {
        regex::Regex::new(r"(?is)<script[^>]*>.*?</script>").expect("static regex literal")
    });
    let style = STYLE_RE.get_or_init(|| {
        regex::Regex::new(r"(?is)<style[^>]*>.*?</style>").expect("static regex literal")
    });

    let s = head.replace_all(html, "");
    let s = script.replace_all(&s, " ");
    let s = style.replace_all(&s, " ");

    let cb = CB_RE.get_or_init(|| {
        regex::Regex::new(r"(?is)<pre[^>]*>\s*<code[^>]*>(.*?)</code>\s*</pre>")
            .expect("static regex literal")
    });
    let s = cb.replace_all(&s, |c: &regex::Captures| {
        format!(
            "\n```\n{}\n```\n",
            c.get(1).map(|m| m.as_str()).unwrap_or("")
        )
    });

    let pre = PRE_RE.get_or_init(|| {
        regex::Regex::new(r"(?is)<pre[^>]*>(.*?)</pre>").expect("static regex literal")
    });
    let s = pre.replace_all(&s, |c: &regex::Captures| {
        format!(
            "\n```\n{}\n```\n",
            c.get(1).map(|m| m.as_str()).unwrap_or("")
        )
    });

    let ic = IC_RE.get_or_init(|| {
        regex::Regex::new(r"(?i)<code\b[^>]*>(.*?)</code>").expect("static regex literal")
    });
    let s = ic.replace_all(&s, |c: &regex::Captures| {
        format!("`{}`", c.get(1).map(|m| m.as_str()).unwrap_or(""))
    });

    let h = H_RE.get_or_init(|| {
        regex::Regex::new(r"(?i)<h([1-6])[^>]*>(.*?)</h[1-6]>").expect("static regex literal")
    });
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
        regex::Regex::new(r#"(?i)<a\b[^>]*href=["']([^"']+)["'][^>]*>(.*?)</a>"#)
            .expect("static regex literal")
    });
    let s = link.replace_all(&s, "[$2]($1)");

    let strong = STRONG_RE.get_or_init(|| {
        regex::Regex::new(r"(?i)</?(?:strong|b)\b[^>]*>").expect("static regex literal")
    });
    let s = strong.replace_all(&s, "**");

    let em = EM_RE.get_or_init(|| {
        regex::Regex::new(r"(?i)</?(?:em|i)\b[^>]*>").expect("static regex literal")
    });
    let s = em.replace_all(&s, "*");

    let hr =
        HR_RE.get_or_init(|| regex::Regex::new(r"(?i)<hr\b\s*/?>").expect("static regex literal"));
    let s = hr.replace_all(&s, "\n---\n");

    let br =
        BR_RE.get_or_init(|| regex::Regex::new(r"(?i)<br\b\s*/?>").expect("static regex literal"));
    let s = br.replace_all(&s, "\n");

    let p =
        P_RE.get_or_init(|| regex::Regex::new(r"(?i)</?p\b[^>]*>").expect("static regex literal"));
    let s = p.replace_all(&s, "\n\n");

    let list = LIST_RE.get_or_init(|| {
        regex::Regex::new(r"(?i)<(?:ul|ol|li)\b[^>]*>|</(?:ul|ol)\b>")
            .expect("static regex literal")
    });
    let mut stack: Vec<bool> = Vec::new();
    let s = list.replace_all(&s, |c: &regex::Captures| {
        let t = c
            .get(0)
            .expect("group 0 always present on match")
            .as_str()
            .to_ascii_lowercase();
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

    let tag = TAG_RE.get_or_init(|| regex::Regex::new(r"<[^>]+>").expect("static regex literal"));
    let s = tag.replace_all(&s, " ");

    let s = html_entities::decode(&s);

    let ws = WS_RE.get_or_init(|| regex::Regex::new(r"[ \t]+").expect("static regex literal"));
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

    // Fake resolver that always errors — stands in for a wedged/failing
    // OS resolver. WO 38.3 made resolver errors fail CLOSED, so this is
    // only usable for tests that assert denial.
    struct ErroringResolver;

    impl DnsResolver for ErroringResolver {
        fn resolve(&self, _host_port: &str) -> std::io::Result<Vec<SocketAddr>> {
            Err(std::io::Error::new(
                std::io::ErrorKind::AddrNotAvailable,
                "fake resolver error",
            ))
        }
    }

    // Fake resolver that "resolves" to zero addresses. Used by the
    // wiremock-backed fetch tests so the SSRF guards take the
    // "Ok(empty) → no internal IP / no pinning" fast path instead of a
    // real ~5s OS DNS lookup of the non-resolving `test.local` host. The
    // reqwest client is already pinned to the mock server via
    // `.resolve("test.local", addr)`, so the actual HTTP connect never
    // hits the OS resolver either. (WO 38.3: the old NxdomainResolver
    // returned Err, which now fails closed — empty-Ok is the supported
    // test seam.)
    // WO 33.14: replaced 5 `#[ignore = "real DNS NXDOMAIN I/O ~10s"]` tests.
    struct EmptyResolver;

    impl DnsResolver for EmptyResolver {
        fn resolve(&self, _host_port: &str) -> std::io::Result<Vec<SocketAddr>> {
            Ok(Vec::new())
        }
    }

    fn empty_resolver_handle() -> ResolverHandle {
        Arc::new(EmptyResolver)
    }

    fn erroring_handle() -> ResolverHandle {
        Arc::new(ErroringResolver)
    }

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
        // Uses the real SystemResolver — localhost resolution is instant
        // and reliable on every CI host. This is the 1 real DNS smoke test
        // for the SSRF guard (WO 33.14: keep 1 real DNS smoke test).
        assert!(
            host_resolves_to_internal_ip("http://localhost/", &SystemResolver),
            "localhost should resolve to an internal IP"
        );
    }

    #[test]
    fn host_resolves_to_internal_ip_literal_ip_is_false() {
        // Literal IPs are handled by `host_is_literal_internal_ip`; the
        // resolver guard must NOT re-resolve them (avoids TOCTOU on a
        // pinned literal and avoids double-denying, which would still be
        // safe but is not this function's job). Short-circuits before
        // consulting the resolver, so the fake resolver is fine here.
        let r: ResolverHandle = empty_resolver_handle();
        assert!(!host_resolves_to_internal_ip("http://127.0.0.1/", &*r));
        assert!(!host_resolves_to_internal_ip("http://8.8.8.8/", &*r));
    }

    #[test]
    fn host_resolves_to_internal_ip_resolver_error_fails_closed() {
        // WO 38.3: a non-literal host whose resolution fails is denied,
        // not deferred to connect time. The earlier fail-open behavior
        // existed only for tests that pinned DNS inside the reqwest
        // client; those now use EmptyResolver (Ok(vec![])).
        let r: ResolverHandle = erroring_handle();
        assert!(
            host_resolves_to_internal_ip("http://kf-code-nonexistent-host-zzz.invalid/", &*r),
            "resolver error must fail closed"
        );
        // And the pinning guard denies too.
        assert!(resolve_and_pin_dns("http://kf-code-nonexistent-host-zzz.invalid/", &*r).is_err());
    }

    #[test]
    fn host_resolves_to_internal_ip_malformed_is_true() {
        // Malformed URL -> extract_host returns None -> fail closed.
        // Short-circuits before consulting the resolver.
        let r: ResolverHandle = empty_resolver_handle();
        assert!(host_resolves_to_internal_ip("", &*r));
    }

    #[test]
    fn empty_resolution_passes_guards_unpinned() {
        // The wiremock test seam: Ok(vec![]) means "resolved, nothing
        // internal, nothing to pin" — guards pass and the fetch falls
        // back to the tool's own (test-pinned) client. `reqwest::Client`
        // does not impl `PartialEq`, so assert the shape without `assert_eq!`.
        let r: ResolverHandle = empty_resolver_handle();
        assert!(!host_resolves_to_internal_ip("http://test.local/", &*r));
        let pin = resolve_and_pin_dns("http://test.local/", &*r);
        assert!(pin.is_ok(), "empty resolution must not deny: {pin:?}");
        assert!(
            pin.as_ref().unwrap().is_none(),
            "empty resolution must not build a pinned client"
        );
    }

    // WO 38.3: IPv4-mapped IPv6 literals carry IPv4 internal targets.
    #[test]
    fn is_internal_addr_ipv4_mapped_v6_is_internal() {
        let mapped = |v4: &str| {
            std::net::IpAddr::V6(
                format!("::ffff:{v4}")
                    .parse::<std::net::Ipv6Addr>()
                    .unwrap(),
            )
        };
        assert!(is_internal_addr(&mapped("169.254.169.254")));
        assert!(is_internal_addr(&mapped("127.0.0.1")));
        assert!(is_internal_addr(&mapped("10.0.0.1")));
        assert!(is_internal_addr(&mapped("192.168.1.1")));
        assert!(!is_internal_addr(&mapped("8.8.8.8")));
        // Non-mapped V6 classification is unchanged.
        assert!(is_internal_addr(&std::net::IpAddr::V6(
            "fe80::1".parse().unwrap()
        )));
        assert!(!is_internal_addr(&std::net::IpAddr::V6(
            "2606:4700::1111".parse().unwrap()
        )));
    }

    #[tokio::test]
    async fn rejects_ipv4_mapped_v6_metadata_literal() {
        let tool = WebFetch::new(DenyList::default());
        let outcome = tool
            .run(
                &ToolContext::new(),
                json!({"url": "http://[::ffff:169.254.169.254]/latest/meta-data/"}),
            )
            .await;
        assert!(
            matches!(
                outcome,
                ToolOutcome::Failure(ToolError::AccessDenied { .. })
            ),
            "expected denied IPv4-mapped metadata IP, got {outcome:?}"
        );
    }

    #[tokio::test]
    async fn rejects_when_resolver_errors() {
        // WO 38.3: resolver failure on a non-literal host denies the
        // fetch instead of deferring to connect time. The client is
        // never consulted — the guard denies first.
        let tool = WebFetch::with_resolver(
            DenyList::default(),
            reqwest::Client::new(),
            erroring_handle(),
        );
        let outcome = tool
            .run(&ToolContext::new(), json!({"url": "http://test.local/"}))
            .await;
        assert!(
            matches!(
                outcome,
                ToolOutcome::Failure(ToolError::AccessDenied { .. })
            ),
            "expected AccessDenied on resolver error, got {outcome:?}"
        );
    }

    fn test_tool_for(server: &wiremock::MockServer) -> WebFetch {
        // The fetch tool blocks literal internal IPs. Wiremock binds to
        // 127.0.0.1, so point a non-internal hostname at it via reqwest's
        // resolver override for tests. The SSRF guards use the injected
        // `EmptyResolver` (instant empty resolution) so they don't do a
        // real ~5s OS DNS lookup of `test.local`; the actual HTTP connect
        // uses the reqwest client's pinned `.resolve("test.local", addr)`.
        let addr: std::net::SocketAddr = *server.address();
        let client = reqwest::Client::builder()
            .timeout(FETCH_TIMEOUT)
            .user_agent(USER_AGENT)
            .resolve("test.local", addr)
            .build()
            .unwrap_or_else(|_| fallback_client());
        WebFetch::with_resolver(DenyList::default(), client, empty_resolver_handle())
    }

    // WO 33.14: was `#[ignore = "real DNS NXDOMAIN I/O ~10s"]` — now uses the
    // injected `EmptyResolver` (instant) instead of a real OS DNS lookup.
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

    // WO 33.14: was `#[ignore = "real DNS NXDOMAIN I/O ~10s"]` — now uses
    // `EmptyResolver`. See `fetches_json_successfully`.
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

    // WO 33.14: was `#[ignore = "real DNS NXDOMAIN I/O ~10s"]` — now uses
    // `EmptyResolver`. See `fetches_json_successfully`.
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

    // WO 33.14: was `#[ignore = "real DNS NXDOMAIN I/O ~10s"]` — now uses
    // `EmptyResolver`. See `fetches_json_successfully`.
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

    // WO 33.14: was `#[ignore = "real DNS NXDOMAIN I/O ~10s"]` — now uses
    // `EmptyResolver`. See `fetches_json_successfully`.
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

    // WO 46.37: a cancelled token must abort an in-flight fetch instead
    // of waiting out the server delay (previously the full 30s
    // FETCH_TIMEOUT). The mock delays 15s; without the cancel race the
    // 5s timeout below fails the test.
    #[tokio::test]
    async fn cancelled_fetch_returns_promptly() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .respond_with(
                wiremock::ResponseTemplate::new(200).set_delay(std::time::Duration::from_secs(15)),
            )
            .mount(&server)
            .await;

        let tool = test_tool_for(&server);
        let ctx = ToolContext::new();
        let token = ctx.token.clone();
        let run =
            tokio::spawn(async move { tool.run(&ctx, json!({"url": "http://test.local/"})).await });
        // Let the request reach the wire, then cancel mid-flight.
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        token.cancel();
        let outcome = tokio::time::timeout(std::time::Duration::from_secs(5), run)
            .await
            .expect("cancel must return within 5s, not wait the 15s server delay")
            .expect("spawned run must not panic");
        assert!(
            matches!(outcome, ToolOutcome::Failure(ToolError::Cancelled)),
            "expected Cancelled, got {outcome:?}"
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
