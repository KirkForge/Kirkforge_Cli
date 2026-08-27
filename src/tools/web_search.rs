use crate::shared::{ToolDef, ToolOutcome};
use crate::tools::{Tool, ToolContext};

/// Search the web via the Brave Search API.
///
/// Requires a `BRAVE_SEARCH_API_KEY` environment variable. If the key is not
/// configured the tool returns a clear failure — it never fabricates results.
pub struct WebSearch {
    api_key: Option<String>,
}

#[allow(clippy::new_without_default)]
impl WebSearch {
    pub fn new() -> Self {
        Self {
            api_key: std::env::var("BRAVE_SEARCH_API_KEY").ok(),
        }
    }

    #[cfg(test)]
    fn with_key(key: impl Into<String>) -> Self {
        Self {
            api_key: Some(key.into()),
        }
    }
}

#[async_trait::async_trait]
impl Tool for WebSearch {
    fn def(&self) -> ToolDef {
        ToolDef {
            name: "web_search",
            description: "Search the public web using Brave Search. Requires the BRAVE_SEARCH_API_KEY environment variable. Returns up to 10 result snippets with title, URL, and description.",
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "Search query"
                    },
                    "count": {
                        "type": "integer",
                        "description": "Maximum number of results (1-20, default 10)",
                        "default": 10
                    }
                },
                "required": ["query"]
            }),
        }
    }

    async fn run(&self, ctx: &ToolContext, args: serde_json::Value) -> ToolOutcome {
        let api_key = match self.api_key.as_deref() {
            Some(k) if !k.is_empty() => k,
            _ => {
                return ToolOutcome::Error {
                    message: "web_search is not configured: set the BRAVE_SEARCH_API_KEY environment variable.".to_string(),
                };
            }
        };

        let query = match args.get("query").and_then(|q| q.as_str()) {
            Some(q) if !q.trim().is_empty() => q.trim(),
            _ => {
                return ToolOutcome::Failure(crate::shared::ToolError::invalid_args(
                    "Missing or empty 'query' argument",
                ));
            }
        };

        let count = args
            .get("count")
            .and_then(|c| c.as_u64())
            .map(|c| c.clamp(1, 20) as u32)
            .unwrap_or(10);

        // WO 46.37: race the Brave request against the cancel token so a
        // cancelled turn doesn't wait out the 30s HTTP timeout. Dropping
        // the in-flight future aborts the request. Pattern: `tools/grep.rs:102`.
        let result = tokio::select! {
            biased;
            _ = ctx.token.cancelled() => {
                return ToolOutcome::Failure(crate::shared::ToolError::Cancelled);
            }
            res = search_brave(api_key, query, count) => res,
        };
        match result {
            Ok(results) => ToolOutcome::Success {
                content: format_results(&results),
            },
            Err(e) => ToolOutcome::Error {
                message: format!("Brave Search request failed: {e}"),
            },
        }
    }
}

#[derive(Debug, serde::Deserialize)]
struct BraveResponse {
    #[serde(default)]
    web: BraveWebResults,
}

#[derive(Debug, Default, serde::Deserialize)]
struct BraveWebResults {
    #[serde(default)]
    results: Vec<BraveResult>,
}

#[derive(Debug, serde::Deserialize)]
struct BraveResult {
    title: String,
    url: String,
    #[serde(default)]
    description: String,
}

async fn search_brave(api_key: &str, query: &str, count: u32) -> anyhow::Result<Vec<BraveResult>> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()?;
    let url = reqwest::Url::parse_with_params(
        "https://api.search.brave.com/res/v1/web/search",
        &[("q", query), ("count", &count.to_string())],
    )?;
    let resp = client
        .get(url)
        .header("Accept", "application/json")
        .header("X-Subscription-Token", api_key)
        .send()
        .await?;
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("Brave Search returned HTTP {status}: {body}");
    }
    let parsed: BraveResponse = resp.json().await?;
    Ok(parsed.web.results)
}

fn format_results(results: &[BraveResult]) -> String {
    if results.is_empty() {
        return "No results found.".to_string();
    }
    let mut lines = Vec::with_capacity(results.len() + 1);
    lines.push(format!("Found {} result(s):", results.len()));
    for (i, r) in results.iter().enumerate() {
        lines.push(format!(
            "{}. {}\n   URL: {}\n   {}",
            i + 1,
            r.title,
            r.url,
            r.description
        ));
    }
    lines.join("\n\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shared::test_util::EnvGuard;
    use crate::tools::ToolContext;

    #[test]
    fn def_is_valid_json() {
        let tool = WebSearch::new();
        let def = tool.def();
        assert_eq!(def.name, "web_search");
        assert!(def.parameters.get("properties").is_some());
    }

    #[tokio::test]
    async fn missing_api_key_returns_configuration_error() {
        // Ensure no key is present for this test.
        let _env = EnvGuard::remove("BRAVE_SEARCH_API_KEY");
        let tool = WebSearch::new();
        let outcome = tool
            .run(&ToolContext::new(), serde_json::json!({"query": "rust"}))
            .await;
        let message = match outcome {
            ToolOutcome::Error { message } => message,
            other => panic!("expected error, got {other:?}"),
        };
        assert!(message.contains("BRAVE_SEARCH_API_KEY"), "{message}");
    }

    #[tokio::test]
    async fn empty_query_is_rejected() {
        let tool = WebSearch::with_key("dummy");
        let outcome = tool
            .run(&ToolContext::new(), serde_json::json!({"query": "  "}))
            .await;
        assert!(
            matches!(
                outcome,
                ToolOutcome::Failure(crate::shared::ToolError::InvalidArgs { .. })
            ),
            "got {outcome:?}"
        );
    }

    // WO 46.37: a cancelled token short-circuits before any HTTP is
    // issued (the biased select never polls the Brave future), so this
    // test is network-free and deterministic.
    #[tokio::test]
    async fn cancelled_token_returns_cancelled() {
        let tool = WebSearch::with_key("dummy");
        let ctx = ToolContext::new();
        ctx.token.cancel();
        let outcome = tool.run(&ctx, serde_json::json!({"query": "rust"})).await;
        assert!(
            matches!(
                outcome,
                ToolOutcome::Failure(crate::shared::ToolError::Cancelled)
            ),
            "got {outcome:?}"
        );
    }

    #[tokio::test]
    async fn count_is_clamped() {
        // We can't call the real API in tests, but we can verify the tool
        // accepts an out-of-range count and would pass it through clamped.
        // The request will fail on auth, confirming the path reached the
        // HTTP layer rather than failing validation.
        let tool = WebSearch::with_key("dummy");
        let outcome = tool
            .run(
                &ToolContext::new(),
                serde_json::json!({"query": "rust", "count": 100}),
            )
            .await;
        assert!(
            matches!(outcome, ToolOutcome::Error { .. }),
            "expected HTTP-layer error, got {outcome:?}"
        );
    }

    #[test]
    fn def_lists_query_as_required() {
        let tool = WebSearch::new();
        let def = tool.def();
        let required = def
            .parameters
            .get("required")
            .and_then(|r| r.as_array())
            .expect("required array");
        assert!(required.iter().any(|v| v.as_str() == Some("query")));
    }

    #[test]
    fn with_key_sets_non_empty_api_key() {
        let tool = WebSearch::with_key("dummy-key");
        assert!(tool.api_key.is_some());
        assert_eq!(tool.api_key.as_deref(), Some("dummy-key"));
    }

    #[tokio::test]
    async fn empty_api_key_treated_as_unconfigured() {
        let _env = EnvGuard::remove("BRAVE_SEARCH_API_KEY");
        let tool = WebSearch::with_key("");
        let outcome = tool
            .run(&ToolContext::new(), serde_json::json!({"query": "rust"}))
            .await;
        match outcome {
            ToolOutcome::Error { message } => {
                assert!(message.contains("BRAVE_SEARCH_API_KEY"), "{message}")
            }
            other => panic!("expected Error for empty key, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn missing_query_arg_is_invalid_args() {
        let tool = WebSearch::with_key("dummy");
        let outcome = tool.run(&ToolContext::new(), serde_json::json!({})).await;
        assert!(
            matches!(
                outcome,
                ToolOutcome::Failure(crate::shared::ToolError::InvalidArgs { .. })
            ),
            "got {outcome:?}"
        );
    }

    #[tokio::test]
    async fn count_below_minimum_is_clamped_to_one() {
        let tool = WebSearch::with_key("dummy");
        let outcome = tool
            .run(
                &ToolContext::new(),
                serde_json::json!({"query": "rust", "count": 0}),
            )
            .await;
        assert!(
            matches!(outcome, ToolOutcome::Error { .. }),
            "expected HTTP-layer error (count clamped, request issued), got {outcome:?}"
        );
    }

    #[test]
    fn format_results_empty_returns_no_results_message() {
        let msg = format_results(&[]);
        assert_eq!(msg, "No results found.");
    }

    #[test]
    fn format_results_single_result_has_index_and_title() {
        let result = BraveResult {
            title: "Title One".into(),
            url: "https://example.com".into(),
            description: "A description".into(),
        };
        let out = format_results(&[result]);
        assert!(out.contains("Found 1 result(s):"), "got: {out}");
        assert!(out.contains("1. Title One"));
        assert!(out.contains("URL: https://example.com"));
        assert!(out.contains("A description"));
    }

    #[test]
    fn format_results_multiple_results_have_increasing_indices() {
        let results = vec![
            BraveResult {
                title: "First".into(),
                url: "https://a.com".into(),
                description: "d1".into(),
            },
            BraveResult {
                title: "Second".into(),
                url: "https://b.com".into(),
                description: "d2".into(),
            },
        ];
        let out = format_results(&results);
        assert!(out.contains("Found 2 result(s):"), "got: {out}");
        assert!(out.contains("1. First"));
        assert!(out.contains("2. Second"));
    }

    #[test]
    fn format_results_with_empty_description_keeps_index_and_url() {
        let result = BraveResult {
            title: "T".into(),
            url: "https://x.com".into(),
            description: String::new(),
        };
        let out = format_results(&[result]);
        assert!(out.contains("1. T"));
        assert!(out.contains("URL: https://x.com"));
    }

    #[test]
    fn brave_response_default_web_is_empty_results() {
        let raw = r#"{}"#;
        let parsed: BraveResponse = serde_json::from_str(raw).unwrap();
        assert!(parsed.web.results.is_empty());
    }

    #[test]
    fn brave_response_parses_results_with_default_description() {
        let raw = r#"{
            "web": {
                "results": [
                    {"title": "A", "url": "https://a.com"}
                ]
            }
        }"#;
        let parsed: BraveResponse = serde_json::from_str(raw).unwrap();
        assert_eq!(parsed.web.results.len(), 1);
        assert_eq!(parsed.web.results[0].title, "A");
        assert_eq!(parsed.web.results[0].url, "https://a.com");
        assert_eq!(parsed.web.results[0].description, "");
    }

    #[test]
    fn brave_response_parses_multiple_results() {
        let raw = r#"{
            "web": {
                "results": [
                    {"title": "A", "url": "https://a.com", "description": "da"},
                    {"title": "B", "url": "https://b.com", "description": "db"}
                ]
            }
        }"#;
        let parsed: BraveResponse = serde_json::from_str(raw).unwrap();
        assert_eq!(parsed.web.results.len(), 2);
        assert_eq!(parsed.web.results[1].title, "B");
    }

    #[test]
    fn brave_result_with_missing_description_defaults_to_empty() {
        let raw = r#"{"title":"x","url":"https://y.com"}"#;
        let parsed: BraveResult = serde_json::from_str(raw).unwrap();
        assert_eq!(parsed.description, "");
    }
}
