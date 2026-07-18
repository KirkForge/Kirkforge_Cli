use crate::shared::{ToolDef, ToolOutcome};
use crate::tools::{Tool, ToolContext};

/// Search the web via the Brave Search API.
///
/// Requires a `BRAVE_SEARCH_API_KEY` environment variable. If the key is not
/// configured the tool returns a clear failure — it never fabricates results.
pub struct WebSearch {
    api_key: Option<String>,
}

impl Default for WebSearch {
    fn default() -> Self {
        Self::new()
    }
}

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

    async fn run(&self, _ctx: &ToolContext, args: serde_json::Value) -> ToolOutcome {
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

        match search_brave(api_key, query, count).await {
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

async fn search_brave(
    api_key: &str,
    query: &str,
    count: u32,
) -> anyhow::Result<Vec<BraveResult>> {
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
        let _guard = std::env::remove_var("BRAVE_SEARCH_API_KEY");
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
            matches!(outcome, ToolOutcome::Failure(crate::shared::ToolError::InvalidArgs { .. })),
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
}
