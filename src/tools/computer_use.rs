//! Headless-Chrome computer-use tool.
//!
//! Gives the model the ability to navigate a web page, click, type, scroll,
//! and take screenshots. This is the KirkForge equivalent of Anthropic's
//! `computer_use` capability, but implemented locally with Chrome DevTools
//! Protocol so it works with any vision model.
//!
//! The tool is registered only when:
//!   - `Config::computer_use::enabled` is `true`
//!   - the active adapter reports `supports_images: true`
//!
//! The screenshot result is returned as `ToolOutcome::Image` so the executor's
//! `handle_tool_outcome` splices it back into the conversation as a vision
//! input.

use crate::session::access::DenyList;
use crate::shared::{ComputerUseConfig, ToolDef, ToolError, ToolOutcome};
use crate::tools::{Tool, ToolContext};
use base64::Engine as _;
use std::sync::Arc;
use std::time::Duration;
/// Trait that abstracts the actual Chrome tab so tests can inject a fake.
/// Exported so the launcher in `main/mod.rs` can hand a real tab handle to
/// the tool. `RealChromeTab` lives next to the launcher to keep headless_chrome
/// imports in one place.
pub trait ChromeTab: Send + Sync {
    fn navigate(&self, url: &str) -> anyhow::Result<()>;
    fn click(&self, selector: &str) -> anyhow::Result<()>;
    fn click_xy(&self, x: f64, y: f64) -> anyhow::Result<()>;
    fn type_text(&self, selector: &str, text: &str) -> anyhow::Result<()>;
    fn keypress(&self, key: &str) -> anyhow::Result<()>;
    fn scroll(&self, amount: i32) -> anyhow::Result<()>;
    fn screenshot(&self) -> anyhow::Result<Vec<u8>>;
    fn wait_for(&self, selector: &str, timeout: Duration) -> anyhow::Result<()>;
    fn evaluate(&self, expression: &str) -> anyhow::Result<String>;
}

/// Synchronous driver that owns a `ChromeTab` implementation.
pub struct ComputerUse {
    deny_list: DenyList,
    config: ComputerUseConfig,
    tab: Arc<dyn ChromeTab>,
}

impl ComputerUse {
    /// Constructor used in production. Receives a tab handle produced by the
    /// Chrome launcher in `main/mod.rs` (or a placeholder if Chrome is unavailable).
    pub fn new(deny_list: DenyList, config: ComputerUseConfig, tab: Arc<dyn ChromeTab>) -> Self {
        Self {
            deny_list,
            config,
            tab,
        }
    }

    /// Constructor for tests with an injected tab.
    #[cfg(test)]
    fn with_tab(deny_list: DenyList, config: ComputerUseConfig, tab: Arc<dyn ChromeTab>) -> Self {
        Self {
            deny_list,
            config,
            tab,
        }
    }
}

/// Placeholder returned when Chrome is unavailable. It keeps the toolset
/// construction cheap and lets the tool fail gracefully at runtime.
#[derive(Debug, Clone, Copy)]
pub struct PlaceholderTab;

impl ChromeTab for PlaceholderTab {
    fn navigate(&self, _url: &str) -> anyhow::Result<()> {
        Err(anyhow::anyhow!("Chrome tab not initialized"))
    }
    fn click(&self, _selector: &str) -> anyhow::Result<()> {
        Err(anyhow::anyhow!("Chrome tab not initialized"))
    }
    fn click_xy(&self, _x: f64, _y: f64) -> anyhow::Result<()> {
        Err(anyhow::anyhow!("Chrome tab not initialized"))
    }
    fn type_text(&self, _selector: &str, _text: &str) -> anyhow::Result<()> {
        Err(anyhow::anyhow!("Chrome tab not initialized"))
    }
    fn keypress(&self, _key: &str) -> anyhow::Result<()> {
        Err(anyhow::anyhow!("Chrome tab not initialized"))
    }
    fn scroll(&self, _amount: i32) -> anyhow::Result<()> {
        Err(anyhow::anyhow!("Chrome tab not initialized"))
    }
    fn screenshot(&self) -> anyhow::Result<Vec<u8>> {
        Err(anyhow::anyhow!("Chrome tab not initialized"))
    }
    fn wait_for(&self, _selector: &str, _timeout: Duration) -> anyhow::Result<()> {
        Err(anyhow::anyhow!("Chrome tab not initialized"))
    }
    fn evaluate(&self, _expression: &str) -> anyhow::Result<String> {
        Err(anyhow::anyhow!("Chrome tab not initialized"))
    }
}

#[async_trait::async_trait]
impl Tool for ComputerUse {
    fn def(&self) -> ToolDef {
        ToolDef {
            name: "computer_use",
            description: "Control a headless Chrome browser: navigate, click, type, scroll, and screenshot web pages. Returns a screenshot after each action. Only public http(s) URLs are allowed; internal/metadata endpoints are denied.",
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["navigate", "click", "click_xy", "type", "keypress", "scroll", "screenshot", "wait_for", "evaluate"],
                        "description": "The browser action to perform."
                    },
                    "url": {
                        "type": "string",
                        "description": "URL to navigate to (required for navigate)."
                    },
                    "selector": {
                        "type": "string",
                        "description": "CSS selector (required for click, type, wait_for)."
                    },
                    "x": {
                        "type": "number",
                        "description": "X coordinate for click_xy."
                    },
                    "y": {
                        "type": "number",
                        "description": "Y coordinate for click_xy."
                    },
                    "text": {
                        "type": "string",
                        "description": "Text to type (required for type)."
                    },
                    "key": {
                        "type": "string",
                        "description": "Key to press, e.g. 'Enter', 'Tab' (required for keypress)."
                    },
                    "amount": {
                        "type": "integer",
                        "description": "Pixels to scroll; positive down, negative up (required for scroll)."
                    },
                    "expression": {
                        "type": "string",
                        "description": "JavaScript expression to evaluate (required for evaluate)."
                    }
                },
                "required": ["action"]
            }),
        }
    }

    async fn run(&self, _ctx: &ToolContext, args: serde_json::Value) -> ToolOutcome {
        let action = match args.get("action").and_then(|v| v.as_str()) {
            Some(a) => a,
            None => return ToolOutcome::Failure(ToolError::invalid_args("Missing 'action'")),
        };

        if matches!(action, "navigate") {
            let url = match args.get("url").and_then(|v| v.as_str()) {
                Some(u) => u,
                None => return ToolOutcome::Failure(ToolError::invalid_args("Missing 'url'")),
            };
            let lower = url.trim().to_ascii_lowercase();
            if !(lower.starts_with("http://") || lower.starts_with("https://")) {
                return ToolOutcome::Failure(ToolError::AccessDenied {
                    message: "Only http:// and https:// URLs are allowed".into(),
                });
            }
            if self.deny_list.is_url_denied(url) {
                return ToolOutcome::Failure(ToolError::AccessDenied {
                    message: "URL is denied by the security policy".into(),
                });
            }
            if crate::tools::web_fetch::host_is_literal_internal_ip(url) {
                return ToolOutcome::Failure(ToolError::AccessDenied {
                    message: "URL resolves to a private/internal IP by literal host".into(),
                });
            }
        }

        run_on_tab(&*self.tab, action, &args, &self.config).await
    }
}

async fn run_on_tab(
    tab: &dyn ChromeTab,
    action: &str,
    args: &serde_json::Value,
    config: &ComputerUseConfig,
) -> ToolOutcome {
    let wait = Duration::from_secs(config.wait_timeout_secs);
    let result = match action {
        "navigate" => {
            let url = args["url"].as_str().unwrap_or("");
            tab.navigate(url).map(|_| format!("Navigated to {url}"))
        }
        "click" => {
            let selector = args["selector"].as_str().unwrap_or("");
            tab.click(selector).map(|_| format!("Clicked {selector}"))
        }
        "click_xy" => {
            let x = args["x"].as_f64().unwrap_or(0.0);
            let y = args["y"].as_f64().unwrap_or(0.0);
            tab.click_xy(x, y).map(|_| format!("Clicked at ({x}, {y})"))
        }
        "type" => {
            let selector = args["selector"].as_str().unwrap_or("");
            let text = args["text"].as_str().unwrap_or("");
            tab.type_text(selector, text)
                .map(|_| format!("Typed into {selector}"))
        }
        "keypress" => {
            let key = args["key"].as_str().unwrap_or("");
            tab.keypress(key).map(|_| format!("Pressed {key}"))
        }
        "scroll" => {
            let amount = args["amount"].as_i64().unwrap_or(0) as i32;
            tab.scroll(amount)
                .map(|_| format!("Scrolled {amount} pixels"))
        }
        "wait_for" => {
            let selector = args["selector"].as_str().unwrap_or("");
            tab.wait_for(selector, wait)
                .map(|_| format!("Element {selector} is present"))
        }
        "evaluate" => {
            let expression = args["expression"].as_str().unwrap_or("");
            tab.evaluate(expression)
        }
        "screenshot" => {
            return match tab.screenshot() {
                Ok(data) => ToolOutcome::Image {
                    path: std::path::PathBuf::from("screenshot.png"),
                    mime: "image/png".to_string(),
                    data_base64: base64::prelude::BASE64_STANDARD.encode(&data),
                },
                Err(e) => ToolOutcome::Failure(ToolError::Internal {
                    message: format!("screenshot failed: {e:#}"),
                }),
            }
        }
        other => Err(anyhow::anyhow!("unknown action: {other}")),
    };

    match result {
        Ok(content) => ToolOutcome::Success { content },
        Err(e) => ToolOutcome::Failure(ToolError::Internal {
            message: format!("{action} failed: {e:#}"),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::Tool;
    use serde_json::json;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct FakeTab {
        navigations: AtomicUsize,
    }

    impl ChromeTab for FakeTab {
        fn navigate(&self, url: &str) -> anyhow::Result<()> {
            self.navigations.fetch_add(1, Ordering::SeqCst);
            assert_eq!(url, "https://example.com");
            Ok(())
        }
        fn click(&self, _selector: &str) -> anyhow::Result<()> {
            Ok(())
        }
        fn click_xy(&self, _x: f64, _y: f64) -> anyhow::Result<()> {
            Ok(())
        }
        fn type_text(&self, _selector: &str, _text: &str) -> anyhow::Result<()> {
            Ok(())
        }
        fn keypress(&self, _key: &str) -> anyhow::Result<()> {
            Ok(())
        }
        fn scroll(&self, _amount: i32) -> anyhow::Result<()> {
            Ok(())
        }
        fn screenshot(&self) -> anyhow::Result<Vec<u8>> {
            Ok(vec![0x89, 0x50, 0x4e, 0x47]) // PNG magic bytes
        }
        fn wait_for(&self, _selector: &str, _timeout: Duration) -> anyhow::Result<()> {
            Ok(())
        }
        fn evaluate(&self, _expression: &str) -> anyhow::Result<String> {
            Ok("42".into())
        }
    }

    fn fake_tool() -> ComputerUse {
        ComputerUse::with_tab(
            DenyList::default(),
            ComputerUseConfig::default(),
            Arc::new(FakeTab {
                navigations: AtomicUsize::new(0),
            }),
        )
    }

    #[tokio::test]
    async fn rejects_non_http_url() {
        let tool = fake_tool();
        let outcome = tool
            .run(
                &ToolContext::new(),
                json!({"action": "navigate", "url": "file:///etc/passwd"}),
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
    async fn rejects_missing_url() {
        let tool = fake_tool();
        let outcome = tool
            .run(&ToolContext::new(), json!({"action": "navigate"}))
            .await;
        assert!(
            matches!(outcome, ToolOutcome::Failure(ToolError::InvalidArgs { .. })),
            "got {outcome:?}"
        );
    }

    #[tokio::test]
    async fn navigate_returns_success() {
        let tool = fake_tool();
        let outcome = tool
            .run(
                &ToolContext::new(),
                json!({"action": "navigate", "url": "https://example.com"}),
            )
            .await;
        let ToolOutcome::Success { content } = outcome else {
            panic!("expected Success, got {outcome:?}");
        };
        assert!(content.contains("example.com"));
    }

    #[tokio::test]
    async fn screenshot_returns_image_outcome() {
        let tool = fake_tool();
        let outcome = tool
            .run(&ToolContext::new(), json!({"action": "screenshot"}))
            .await;
        let ToolOutcome::Image {
            mime, data_base64, ..
        } = outcome
        else {
            panic!("expected Image, got {outcome:?}");
        };
        assert_eq!(mime, "image/png");
        assert!(!data_base64.is_empty());
    }

    #[tokio::test]
    async fn evaluate_returns_text_result() {
        let tool = fake_tool();
        let outcome = tool
            .run(
                &ToolContext::new(),
                json!({"action": "evaluate", "expression": "1+1"}),
            )
            .await;
        let ToolOutcome::Success { content } = outcome else {
            panic!("expected Success, got {outcome:?}");
        };
        assert_eq!(content, "42");
    }
}
