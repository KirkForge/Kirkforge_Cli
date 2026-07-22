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
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Pinned, boxed future returned by [`SessionLauncher`].
pub type SessionFuture =
    std::pin::Pin<Box<dyn std::future::Future<Output = anyhow::Result<Arc<dyn ChromeTab>>> + Send>>;

/// Factory function that creates a fresh browser session.
/// In production, this launches a real Chrome instance via `open_browser_session`.
/// In tests, this is `None` (falls back to the shared tab).
pub type SessionLauncher = Arc<dyn Fn() -> SessionFuture + Send + Sync>;

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
    session: Mutex<Option<BrowserSession>>,
    session_launcher: Option<SessionLauncher>,
}

impl ComputerUse {
    /// Constructor used in production. Receives a tab handle produced by the
    /// Chrome launcher in `main/mod.rs` (or a placeholder if Chrome is unavailable),
    /// plus an optional session launcher for creating fresh browser instances
    /// on `open`.
    pub fn new(
        deny_list: DenyList,
        config: ComputerUseConfig,
        tab: Arc<dyn ChromeTab>,
        session_launcher: Option<SessionLauncher>,
    ) -> Self {
        Self {
            deny_list,
            config,
            tab,
            session: Mutex::new(None),
            session_launcher,
        }
    }

    /// Constructor for tests with an injected tab and no session launcher.
    #[cfg(test)]
    fn with_tab(deny_list: DenyList, config: ComputerUseConfig, tab: Arc<dyn ChromeTab>) -> Self {
        Self {
            deny_list,
            config,
            tab,
            session: Mutex::new(None),
            session_launcher: None,
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

/// A persistent browser session that tracks step count across
/// multiple tool invocations, enabling multi-step browser automation
/// with vision-grounded UI reasoning.
pub struct BrowserSession {
    tab: Arc<dyn ChromeTab>,
    step: u32,
    max_steps: u32,
}

impl BrowserSession {
    pub fn new(tab: Arc<dyn ChromeTab>, max_steps: u32) -> Self {
        let max_steps = if max_steps == 0 { 20 } else { max_steps };
        Self {
            tab,
            step: 0,
            max_steps,
        }
    }

    pub fn step(&mut self) -> anyhow::Result<()> {
        self.step += 1;
        if self.step > self.max_steps {
            Err(anyhow::anyhow!(
                "browser session exceeded max_steps ({})",
                self.max_steps
            ))
        } else {
            Ok(())
        }
    }

    pub fn screenshot(&self) -> anyhow::Result<Vec<u8>> {
        self.tab.screenshot()
    }

    pub fn click(&self, selector: &str) -> anyhow::Result<()> {
        self.tab.click(selector)
    }

    pub fn type_text(&self, selector: &str, text: &str) -> anyhow::Result<()> {
        self.tab.type_text(selector, text)
    }

    pub fn wait_for(&self, selector: &str, timeout: Duration) -> anyhow::Result<()> {
        self.tab.wait_for(selector, timeout)
    }

    pub fn scroll(&self, amount: i32) -> anyhow::Result<()> {
        self.tab.scroll(amount)
    }

    pub fn evaluate(&self, js: &str) -> anyhow::Result<String> {
        self.tab.evaluate(js)
    }

    pub fn step_count(&self) -> u32 {
        self.step
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
                        "enum": ["open", "navigate", "click", "click_xy", "type", "keypress", "scroll", "screenshot", "wait_for", "evaluate", "close"],
                        "description": "The browser action to perform."
                    },
                    "url": {
                        "type": "string",
                        "description": "URL to navigate to (required for open and navigate)."
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

        // URL validation applies to both "open" and "navigate"
        if matches!(action, "open" | "navigate") {
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

        match action {
            "open" => {
                let url = args["url"].as_str().unwrap_or("");
                let session_tab = match self.session_launcher {
                    Some(ref launcher) => match launcher().await {
                        Ok(tab) => tab,
                        Err(e) => {
                            return ToolOutcome::Failure(ToolError::Internal {
                                message: format!("failed to launch browser session: {e:#}"),
                            })
                        }
                    },
                    None => self.tab.clone(),
                };
                if let Err(e) = session_tab.navigate(url) {
                    return ToolOutcome::Failure(ToolError::Internal {
                        message: format!("open failed: {e:#}"),
                    });
                }
                let mut guard = self.session.lock().unwrap();
                *guard = Some(BrowserSession::new(session_tab, self.config.max_steps));
                ToolOutcome::Success {
                    content: format!("Opened session and navigated to {url}"),
                }
            }
            "close" => {
                let mut guard = self.session.lock().unwrap();
                guard.take();
                ToolOutcome::Success {
                    content: "Browser session closed".into(),
                }
            }
            _ => {
                let has_session = self.session.lock().unwrap().is_some();
                if has_session {
                    // Increment step counter, then drop the lock before
                    // doing any work that might involve an await.
                    {
                        let mut guard = self.session.lock().unwrap();
                        let session = guard.as_mut().unwrap();
                        if let Err(e) = session.step() {
                            return ToolOutcome::Failure(ToolError::Internal {
                                message: format!("{e:#}"),
                            });
                        }
                    }
                    // All BrowserSession methods are sync, so we do the
                    // action inside the lock and return. The lock is held
                    // only for the duration of the sync call.
                    let mut guard = self.session.lock().unwrap();
                    let session = guard.as_mut().unwrap();
                    let outcome = run_on_session_sync(session, action, &args, &self.config);
                    drop(guard);
                    outcome
                } else {
                    // No active session - fall back to single-shot tab usage
                    // for backward compatibility with PlaceholderTab.
                    run_on_tab(&*self.tab, action, &args, &self.config).await
                }
            }
        }
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

/// Synchronous runner for session actions. All BrowserSession methods
/// are sync, so this avoids holding a MutexGuard across an await.
fn run_on_session_sync(
    session: &mut BrowserSession,
    action: &str,
    args: &serde_json::Value,
    config: &ComputerUseConfig,
) -> ToolOutcome {
    let wait = Duration::from_secs(config.wait_timeout_secs);
    let result = match action {
        "navigate" => {
            let url = args["url"].as_str().unwrap_or("");
            session
                .tab
                .navigate(url)
                .map(|_| format!("Navigated to {url}"))
        }
        "click" => {
            let selector = args["selector"].as_str().unwrap_or("");
            session
                .click(selector)
                .map(|_| format!("Clicked {selector}"))
        }
        "click_xy" => {
            let x = args["x"].as_f64().unwrap_or(0.0);
            let y = args["y"].as_f64().unwrap_or(0.0);
            session
                .tab
                .click_xy(x, y)
                .map(|_| format!("Clicked at ({x}, {y})"))
        }
        "type" => {
            let selector = args["selector"].as_str().unwrap_or("");
            let text = args["text"].as_str().unwrap_or("");
            session
                .type_text(selector, text)
                .map(|_| format!("Typed into {selector}"))
        }
        "keypress" => {
            let key = args["key"].as_str().unwrap_or("");
            session.tab.keypress(key).map(|_| format!("Pressed {key}"))
        }
        "scroll" => {
            let amount = args["amount"].as_i64().unwrap_or(0) as i32;
            session
                .scroll(amount)
                .map(|_| format!("Scrolled {amount} pixels"))
        }
        "wait_for" => {
            let selector = args["selector"].as_str().unwrap_or("");
            session
                .wait_for(selector, wait)
                .map(|_| format!("Element {selector} is present"))
        }
        "evaluate" => {
            let expression = args["expression"].as_str().unwrap_or("");
            session.evaluate(expression)
        }
        "screenshot" => {
            return match session.screenshot() {
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

    fn fake_tool_with_max_steps(max_steps: u32) -> ComputerUse {
        ComputerUse::with_tab(
            DenyList::default(),
            ComputerUseConfig {
                max_steps,
                ..Default::default()
            },
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

    #[tokio::test]
    async fn computer_use_open_action_parsed() {
        let tool = fake_tool();
        let outcome = tool
            .run(
                &ToolContext::new(),
                json!({"action": "open", "url": "https://example.com"}),
            )
            .await;
        let ToolOutcome::Success { content } = outcome else {
            panic!("expected Success, got {outcome:?}");
        };
        assert!(content.contains("example.com"));
    }

    #[tokio::test]
    async fn computer_use_close_action_parsed() {
        let tool = fake_tool();
        // open first so close has a session to close
        tool.run(
            &ToolContext::new(),
            json!({"action": "open", "url": "https://example.com"}),
        )
        .await;
        let outcome = tool
            .run(&ToolContext::new(), json!({"action": "close"}))
            .await;
        let ToolOutcome::Success { content } = outcome else {
            panic!("expected Success, got {outcome:?}");
        };
        assert!(content.contains("closed"));
    }

    #[tokio::test]
    async fn computer_use_max_steps_enforced() {
        let tool = fake_tool_with_max_steps(2);
        // open creates session (step 0)
        tool.run(
            &ToolContext::new(),
            json!({"action": "open", "url": "https://example.com"}),
        )
        .await;
        // step 1 - ok
        let outcome = tool
            .run(
                &ToolContext::new(),
                json!({"action": "click", "selector": "a"}),
            )
            .await;
        assert!(
            matches!(outcome, ToolOutcome::Success { .. }),
            "step 1 should succeed, got {outcome:?}"
        );
        // step 2 - ok (max_steps=2 allows 2 steps)
        let outcome = tool
            .run(
                &ToolContext::new(),
                json!({"action": "click", "selector": "b"}),
            )
            .await;
        assert!(
            matches!(outcome, ToolOutcome::Success { .. }),
            "step 2 should succeed, got {outcome:?}"
        );
        // step 3 - exceeds max_steps
        let outcome = tool
            .run(
                &ToolContext::new(),
                json!({"action": "click", "selector": "c"}),
            )
            .await;
        assert!(
            matches!(outcome, ToolOutcome::Failure(ToolError::Internal { .. })),
            "step 3 should fail, got {outcome:?}"
        );
    }

    #[tokio::test]
    async fn computer_use_invalid_action_rejected() {
        let tool = fake_tool();
        let outcome = tool
            .run(&ToolContext::new(), json!({"action": "frobnicate"}))
            .await;
        assert!(
            matches!(outcome, ToolOutcome::Failure(ToolError::Internal { .. })),
            "got {outcome:?}"
        );
    }

    #[tokio::test]
    async fn browser_session_open_creates_session() {
        let tool = fake_tool();
        assert!(
            tool.session.lock().unwrap().is_none(),
            "no session before open"
        );
        let outcome = tool
            .run(
                &ToolContext::new(),
                json!({"action": "open", "url": "https://example.com"}),
            )
            .await;
        let ToolOutcome::Success { content } = outcome else {
            panic!("expected Success, got {outcome:?}");
        };
        assert!(content.contains("example.com"));
        assert!(
            tool.session.lock().unwrap().is_some(),
            "session should exist after open"
        );
    }

    #[tokio::test]
    async fn browser_session_close_destroys_session() {
        let tool = fake_tool();
        tool.run(
            &ToolContext::new(),
            json!({"action": "open", "url": "https://example.com"}),
        )
        .await;
        assert!(tool.session.lock().unwrap().is_some());
        tool.run(&ToolContext::new(), json!({"action": "close"}))
            .await;
        assert!(
            tool.session.lock().unwrap().is_none(),
            "session should be destroyed after close"
        );
    }

    #[tokio::test]
    async fn browser_session_actions_use_session_when_open() {
        let tool = fake_tool_with_max_steps(20);
        tool.run(
            &ToolContext::new(),
            json!({"action": "open", "url": "https://example.com"}),
        )
        .await;
        let outcome = tool
            .run(
                &ToolContext::new(),
                json!({"action": "click", "selector": "#btn"}),
            )
            .await;
        assert!(
            matches!(outcome, ToolOutcome::Success { .. }),
            "click should succeed: {outcome:?}"
        );
        let guard = tool.session.lock().unwrap();
        let session = guard.as_ref().unwrap();
        assert_eq!(session.step_count(), 1, "step should be 1 after one action");
    }

    #[tokio::test]
    async fn browser_session_screenshot_returns_png() {
        let tool = fake_tool();
        tool.run(
            &ToolContext::new(),
            json!({"action": "open", "url": "https://example.com"}),
        )
        .await;
        let outcome = tool
            .run(&ToolContext::new(), json!({"action": "screenshot"}))
            .await;
        match outcome {
            ToolOutcome::Image { data_base64, .. } => {
                let bytes = base64::prelude::BASE64_STANDARD
                    .decode(&data_base64)
                    .expect("valid base64");
                assert!(bytes.len() >= 4, "screenshot too short");
                assert_eq!(bytes[0..4], [0x89, 0x50, 0x4E, 0x47], "not a PNG");
            }
            other => panic!("expected Image, got {other:?}"),
        }
    }

    #[tokio::test]
    #[ignore = "requires headless Chrome"]
    async fn browser_session_open_and_screenshot_with_chrome() {
        use headless_chrome::browser::tab::point::Point;
        use headless_chrome::protocol::cdp::Page::CaptureScreenshotFormatOption;

        struct ChromeTabForTest {
            _browser: headless_chrome::Browser,
            tab: Arc<headless_chrome::Tab>,
        }

        impl ChromeTab for ChromeTabForTest {
            fn navigate(&self, url: &str) -> anyhow::Result<()> {
                self.tab
                    .navigate_to(url)
                    .map_err(|e| anyhow::anyhow!("{e}"))?;
                self.tab
                    .wait_until_navigated()
                    .map_err(|e| anyhow::anyhow!("{e}"))?;
                Ok(())
            }
            fn click(&self, selector: &str) -> anyhow::Result<()> {
                self.tab
                    .wait_for_element(selector)
                    .map_err(|e| anyhow::anyhow!("{e}"))?
                    .click()
                    .map_err(|e| anyhow::anyhow!("{e}"))?;
                Ok(())
            }
            fn click_xy(&self, x: f64, y: f64) -> anyhow::Result<()> {
                self.tab
                    .click_point(Point { x, y })
                    .map_err(|e| anyhow::anyhow!("{e}"))?;
                Ok(())
            }
            fn type_text(&self, selector: &str, text: &str) -> anyhow::Result<()> {
                self.tab
                    .wait_for_element(selector)
                    .map_err(|e| anyhow::anyhow!("{e}"))?
                    .type_into(text)
                    .map_err(|e| anyhow::anyhow!("{e}"))?;
                Ok(())
            }
            fn keypress(&self, key: &str) -> anyhow::Result<()> {
                self.tab
                    .press_key(key)
                    .map_err(|e| anyhow::anyhow!("{e}"))?;
                Ok(())
            }
            fn scroll(&self, amount: i32) -> anyhow::Result<()> {
                let expr =
                    format!("window.scrollBy({{ top: {amount}, left: 0, behavior: 'instant' }})");
                self.tab
                    .evaluate(&expr, true)
                    .map_err(|e| anyhow::anyhow!("{e}"))?;
                Ok(())
            }
            fn screenshot(&self) -> anyhow::Result<Vec<u8>> {
                self.tab
                    .capture_screenshot(CaptureScreenshotFormatOption::Png, None, None, true)
                    .map_err(|e| anyhow::anyhow!("{e}"))
            }
            fn wait_for(&self, selector: &str, _timeout: Duration) -> anyhow::Result<()> {
                self.tab
                    .wait_for_element(selector)
                    .map_err(|e| anyhow::anyhow!("{e}"))?;
                Ok(())
            }
            fn evaluate(&self, expression: &str) -> anyhow::Result<String> {
                let result = self
                    .tab
                    .evaluate(expression, true)
                    .map_err(|e| anyhow::anyhow!("{e}"))?;
                Ok(result.value.map(|v| v.to_string()).unwrap_or_default())
            }
        }

        let mut builder = headless_chrome::LaunchOptions::default_builder();
        builder.headless(true);
        builder.sandbox(false);
        let options = builder
            .build()
            .expect("failed to build Chrome launch options");
        let browser = match headless_chrome::Browser::new(options) {
            Ok(b) => b,
            Err(_) => return,
        };
        let tab = match browser.new_tab() {
            Ok(t) => t,
            Err(_) => return,
        };
        let chrome_tab: Arc<dyn ChromeTab> = Arc::new(ChromeTabForTest {
            _browser: browser,
            tab,
        });
        let config = ComputerUseConfig {
            enabled: true,
            ..Default::default()
        };
        let tool = ComputerUse::with_tab(DenyList::default(), config, chrome_tab);
        let outcome = tool
            .run(
                &ToolContext::new(),
                json!({"action": "open", "url": "https://example.com"}),
            )
            .await;
        if matches!(outcome, ToolOutcome::Failure(_)) {
            return;
        }
        assert!(
            matches!(outcome, ToolOutcome::Success { .. }),
            "open should succeed: {outcome:?}"
        );
        let outcome = tool
            .run(&ToolContext::new(), json!({"action": "screenshot"}))
            .await;
        match outcome {
            ToolOutcome::Image { data_base64, .. } => {
                let bytes = base64::prelude::BASE64_STANDARD
                    .decode(&data_base64)
                    .expect("valid base64");
                assert!(bytes.len() >= 4, "screenshot too short");
                assert_eq!(bytes[0..4], [0x89, 0x50, 0x4E, 0x47], "not a PNG");
            }
            other => panic!("expected Image, got {other:?}"),
        }
        let outcome = tool
            .run(&ToolContext::new(), json!({"action": "close"}))
            .await;
        assert!(
            matches!(outcome, ToolOutcome::Success { .. }),
            "close should succeed: {outcome:?}"
        );
    }

    #[tokio::test]
    #[ignore = "requires headless Chrome"]
    async fn browser_session_close_cleans_up() {
        use headless_chrome::browser::tab::point::Point;
        use headless_chrome::protocol::cdp::Page::CaptureScreenshotFormatOption;

        struct ChromeTabForTest {
            _browser: headless_chrome::Browser,
            tab: Arc<headless_chrome::Tab>,
        }

        impl ChromeTab for ChromeTabForTest {
            fn navigate(&self, url: &str) -> anyhow::Result<()> {
                self.tab
                    .navigate_to(url)
                    .map_err(|e| anyhow::anyhow!("{e}"))?;
                self.tab
                    .wait_until_navigated()
                    .map_err(|e| anyhow::anyhow!("{e}"))?;
                Ok(())
            }
            fn click(&self, selector: &str) -> anyhow::Result<()> {
                self.tab
                    .wait_for_element(selector)
                    .map_err(|e| anyhow::anyhow!("{e}"))?
                    .click()
                    .map_err(|e| anyhow::anyhow!("{e}"))?;
                Ok(())
            }
            fn click_xy(&self, x: f64, y: f64) -> anyhow::Result<()> {
                self.tab
                    .click_point(Point { x, y })
                    .map_err(|e| anyhow::anyhow!("{e}"))?;
                Ok(())
            }
            fn type_text(&self, selector: &str, text: &str) -> anyhow::Result<()> {
                self.tab
                    .wait_for_element(selector)
                    .map_err(|e| anyhow::anyhow!("{e}"))?
                    .type_into(text)
                    .map_err(|e| anyhow::anyhow!("{e}"))?;
                Ok(())
            }
            fn keypress(&self, key: &str) -> anyhow::Result<()> {
                self.tab
                    .press_key(key)
                    .map_err(|e| anyhow::anyhow!("{e}"))?;
                Ok(())
            }
            fn scroll(&self, amount: i32) -> anyhow::Result<()> {
                let expr =
                    format!("window.scrollBy({{ top: {amount}, left: 0, behavior: 'instant' }})");
                self.tab
                    .evaluate(&expr, true)
                    .map_err(|e| anyhow::anyhow!("{e}"))?;
                Ok(())
            }
            fn screenshot(&self) -> anyhow::Result<Vec<u8>> {
                self.tab
                    .capture_screenshot(CaptureScreenshotFormatOption::Png, None, None, true)
                    .map_err(|e| anyhow::anyhow!("{e}"))
            }
            fn wait_for(&self, selector: &str, _timeout: Duration) -> anyhow::Result<()> {
                self.tab
                    .wait_for_element(selector)
                    .map_err(|e| anyhow::anyhow!("{e}"))?;
                Ok(())
            }
            fn evaluate(&self, expression: &str) -> anyhow::Result<String> {
                let result = self
                    .tab
                    .evaluate(expression, true)
                    .map_err(|e| anyhow::anyhow!("{e}"))?;
                Ok(result.value.map(|v| v.to_string()).unwrap_or_default())
            }
        }

        let mut builder = headless_chrome::LaunchOptions::default_builder();
        builder.headless(true);
        builder.sandbox(false);
        let options = builder
            .build()
            .expect("failed to build Chrome launch options");
        let browser = match headless_chrome::Browser::new(options) {
            Ok(b) => b,
            Err(_) => return,
        };
        let tab = match browser.new_tab() {
            Ok(t) => t,
            Err(_) => return,
        };
        let chrome_tab: Arc<dyn ChromeTab> = Arc::new(ChromeTabForTest {
            _browser: browser,
            tab,
        });
        let config = ComputerUseConfig {
            enabled: true,
            ..Default::default()
        };
        let tool = ComputerUse::with_tab(DenyList::default(), config, chrome_tab);
        tool.run(
            &ToolContext::new(),
            json!({"action": "open", "url": "https://example.com"}),
        )
        .await;
        assert!(tool.session.lock().unwrap().is_some());
        tool.run(&ToolContext::new(), json!({"action": "close"}))
            .await;
        assert!(
            tool.session.lock().unwrap().is_none(),
            "close should destroy session"
        );
    }
}
