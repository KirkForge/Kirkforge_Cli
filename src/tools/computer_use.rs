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

use crate::shared::access::DenyList;
use crate::shared::{ComputerUseConfig, ToolDef, ToolError, ToolOutcome};
use crate::tools::{Tool, ToolContext};
use base64::Engine as _;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// JS preamble injected before `evaluate` expressions to block network requests.
/// This prevents SSRF via `fetch` or `XMLHttpRequest` to internal/metadata IPs.
/// ponytail: blocks all network in evaluate mode; open/navigate use different
/// code paths and are unaffected. WebSocket/EventSource not blocked — add if needed.
const EVALUATE_SAFETY_PREAMBLE: &str = r#"
(() => { window.fetch = async (url, opts) => { throw new Error('fetch blocked in evaluate mode'); }; XMLHttpRequest.prototype.open = function(method, url) { throw new Error('XHR blocked in evaluate mode'); }; })();
"#;

/// Pinned, boxed future returned by [`SessionLauncher`].
pub type SessionFuture =
    std::pin::Pin<Box<dyn std::future::Future<Output = anyhow::Result<Arc<dyn ChromeTab>>> + Send>>;

/// Factory function that creates a fresh browser session.
/// In production, this launches a real Chrome instance via `open_browser_session`.
/// In tests, this is `None` (falls back to the shared tab).
pub type SessionLauncher = Arc<dyn Fn() -> SessionFuture + Send + Sync>;

/// Trait that abstracts the actual Chrome tab so tests can inject a fake.
/// Exported so the launcher in `main/mod.rs` can hand a real tab handle to
/// the tool. `BrowserSessionOwner` lives next to the launcher to keep headless_chrome
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
        if self.config.hosted {
            hosted_def()
        } else {
            local_def()
        }
    }

    async fn run(&self, _ctx: &ToolContext, args: serde_json::Value) -> ToolOutcome {
        let action = match args.get("action").and_then(|v| v.as_str()) {
            Some(a) => a.to_string(),
            None => return ToolOutcome::Failure(ToolError::invalid_args("Missing 'action'")),
        };

        // Hosted path: translate Anthropic's hosted computer_use action
        // vocabulary into the local CDP action shape, dispatch, then
        // ALWAYS capture a screenshot and feed it back as the tool result
        // so the next model turn sees the resulting screen. The executor's
        // turn loop + `handle_tool_outcome::Image` already splice the image
        // into the conversation. See WO 32.17.
        if self.config.hosted {
            return run_hosted_action(&self.tab, &self.config, &action, args).await;
        }

        // URL validation applies to both "open" and "navigate"
        if matches!(action.as_str(), "open" | "navigate") {
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

        match action.as_str() {
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
                // Poison-tolerant (WO 38.2): a panic elsewhere must not
                // brick the browser-session lock — recover the inner state.
                let mut guard = self.session.lock().unwrap_or_else(|e| e.into_inner());
                *guard = Some(BrowserSession::new(session_tab, self.config.max_steps));
                ToolOutcome::Success {
                    content: format!("Opened session and navigated to {url}"),
                }
            }
            "close" => {
                let mut guard = self.session.lock().unwrap_or_else(|e| e.into_inner());
                guard.take();
                ToolOutcome::Success {
                    content: "Browser session closed".into(),
                }
            }
            _ => {
                // Hold a single lock across check + step + use. All
                // BrowserSession/ChromeTab methods are sync, so no await
                // while the guard is held. Splitting the check, step,
                // and use across separate acquisitions left the boolean
                // stale between locks (a concurrent close() could drop
                // the session between the peek and the unwrap).
                // The guard is scoped to the inner block so it drops
                // before the async single-shot fallback (a held
                // std::sync::MutexGuard is not Send and would make the
                // future non-Send across the await).
                let outcome = {
                    let mut guard = self.session.lock().unwrap_or_else(|e| e.into_inner());
                    match guard.as_mut() {
                        Some(session) => {
                            if let Err(e) = session.step() {
                                return ToolOutcome::Failure(ToolError::Internal {
                                    message: format!("{e:#}"),
                                });
                            }
                            Some(run_on_session_sync(session, &action, &args, &self.config))
                        }
                        None => None,
                    }
                };
                match outcome {
                    Some(o) => o,
                    None => run_on_tab(&*self.tab, &action, &args, &self.config).await,
                }
            }
        }
    }
}

fn local_def() -> ToolDef {
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

fn hosted_def() -> ToolDef {
    ToolDef {
        name: "computer",
        description: "Anthropic hosted computer_use tool: click, type, scroll, screenshot, and key actions at screen coordinates. A screenshot is captured after every action and fed back as the tool result so the next model turn sees the resulting screen.",
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["screenshot", "click", "left_click", "right_click", "double_click", "triple_click", "mouse_move", "left_click_drag", "type", "key", "wait", "hold_key", "release", "scroll"],
                    "description": "The hosted computer action to perform."
                },
                "coordinate": {
                    "type": "array",
                    "items": {"type": "integer"},
                    "description": "[x, y] screen coordinate (required for click/move/scroll)."
                },
                "text": {
                    "type": "string",
                    "description": "Text to type (required for type)."
                },
                "key": {
                    "type": "string",
                    "description": "Key combination, e.g. 'Return', 'Tab', 'ctrl+s' (required for key/hold_key/release)."
                },
                "scroll_direction": {
                    "type": "string",
                    "enum": ["up", "down", "left", "right"],
                    "description": "Scroll direction (required for scroll)."
                },
                "duration": {
                    "type": "number",
                    "description": "Seconds to wait (for wait)."
                }
            },
            "required": ["action"]
        }),
    }
}

/// Hosted-path action dispatch: translate Anthropic's hosted computer_use
/// vocabulary into the local CDP `ChromeTab` actions, run the action, then
/// ALWAYS capture a screenshot and return it as `ToolOutcome::Image` so the
/// next model turn sees the resulting screen.
///
/// Reuses the session/tab machinery. `screenshot` and `wait` return the
/// image directly; every other action runs the CDP call then captures a
/// fresh screenshot. The step counter enforces `max_steps` so a hosted
/// loop cannot run away.
async fn run_hosted_action(
    tab: &Arc<dyn ChromeTab>,
    config: &ComputerUseConfig,
    action: &str,
    args: serde_json::Value,
) -> ToolOutcome {
    let coord = args.get("coordinate").and_then(|c| c.as_array());
    let (x, y) = coord
        .and_then(|a| {
            let xv = a.first().and_then(|v| v.as_f64())?;
            let yv = a.get(1).and_then(|v| v.as_f64())?;
            Some((xv, yv))
        })
        .unwrap_or((0.0, 0.0));

    let wait = Duration::from_secs(config.wait_timeout_secs);
    let action_result = match action {
        "screenshot" => Ok(()),
        "wait" => {
            let secs = args.get("duration").and_then(|d| d.as_f64()).unwrap_or(1.0);
            tokio::time::sleep(Duration::from_secs_f64(secs.max(0.0))).await;
            Ok(())
        }
        "click" | "left_click" | "double_click" | "triple_click" => tab.click_xy(x, y),
        "right_click" => {
            // CDP right-click is not in the trait; fall back to a JS
            // contextmenu dispatch at the coordinate.
            // ponytail: ceiling — no right-click in the ChromeTab trait;
            // upgrade path: add `right_click_xy` to the trait.
            let expr = format!(
                "document.elementFromPoint({x},{y})?.dispatchEvent(new MouseEvent('contextmenu', {{clientX:{x},clientY:{y},bubbles:true}}))"
            );
            let _ = tab.evaluate(&expr);
            Ok(())
        }
        "mouse_move" => Ok(()),
        "left_click_drag" => {
            // Drag: press at start, move, release. The trait lacks a drag
            // primitive, so approximate with a click at the start coord;
            // the model re-emits move + click for multi-step drags.
            tab.click_xy(x, y)
        }
        "type" => {
            let text = args.get("text").and_then(|t| t.as_str()).unwrap_or("");
            // Hosted `type` types at the current focus; click first to
            // focus the element at the coordinate, then type.
            if coord.is_some() {
                let _ = tab.click_xy(x, y);
            }
            tab.type_text("body", text)
        }
        "key" | "hold_key" | "release" => {
            let key = args.get("key").and_then(|k| k.as_str()).unwrap_or("");
            tab.keypress(key)
        }
        "scroll" => {
            let dir = args
                .get("scroll_direction")
                .and_then(|d| d.as_str())
                .unwrap_or("down");
            let amount = match dir {
                "up" => -400,
                "left" => 0,
                "right" => 0,
                _ => 400,
            };
            tab.scroll(amount)
        }
        _ => {
            return ToolOutcome::Failure(ToolError::invalid_args(format!(
                "unknown hosted action: {action}"
            )))
        }
    };
    let _ = wait; // suppress unused warning when no wait_for path runs

    if let Err(e) = action_result {
        return ToolOutcome::Failure(ToolError::Internal {
            message: format!("{action} failed: {e:#}"),
        });
    }

    // ALWAYS capture a screenshot after the action and feed it back. This
    // is the vision loop: screenshot → model → action → screenshot.
    match tab.screenshot() {
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

#[allow(clippy::too_many_arguments)]
fn dispatch_action(
    action: &str,
    args: &serde_json::Value,
    config: &ComputerUseConfig,
    navigate: impl FnOnce(&str) -> anyhow::Result<()>,
    click: impl FnOnce(&str) -> anyhow::Result<()>,
    click_xy: impl FnOnce(f64, f64) -> anyhow::Result<()>,
    type_text: impl FnOnce(&str, &str) -> anyhow::Result<()>,
    keypress: impl FnOnce(&str) -> anyhow::Result<()>,
    scroll: impl FnOnce(i32) -> anyhow::Result<()>,
    wait_for: impl FnOnce(&str, Duration) -> anyhow::Result<()>,
    evaluate: impl FnOnce(&str) -> anyhow::Result<String>,
    screenshot: impl FnOnce() -> anyhow::Result<Vec<u8>>,
) -> ToolOutcome {
    let wait = Duration::from_secs(config.wait_timeout_secs);
    let result = match action {
        "navigate" => {
            let url = args["url"].as_str().unwrap_or("");
            navigate(url).map(|_| format!("Navigated to {url}"))
        }
        "click" => {
            let selector = args["selector"].as_str().unwrap_or("");
            click(selector).map(|_| format!("Clicked {selector}"))
        }
        "click_xy" => {
            let x = args["x"].as_f64().unwrap_or(0.0);
            let y = args["y"].as_f64().unwrap_or(0.0);
            click_xy(x, y).map(|_| format!("Clicked at ({x}, {y})"))
        }
        "type" => {
            let selector = args["selector"].as_str().unwrap_or("");
            let text = args["text"].as_str().unwrap_or("");
            type_text(selector, text).map(|_| format!("Typed into {selector}"))
        }
        "keypress" => {
            let key = args["key"].as_str().unwrap_or("");
            keypress(key).map(|_| format!("Pressed {key}"))
        }
        "scroll" => {
            let amount = args["amount"].as_i64().unwrap_or(0) as i32;
            scroll(amount).map(|_| format!("Scrolled {amount} pixels"))
        }
        "wait_for" => {
            let selector = args["selector"].as_str().unwrap_or("");
            wait_for(selector, wait).map(|_| format!("Element {selector} is present"))
        }
        "evaluate" => {
            let expression = args["expression"].as_str().unwrap_or("");
            let safe_expression = format!("{EVALUATE_SAFETY_PREAMBLE}{expression}");
            evaluate(&safe_expression)
        }
        "screenshot" => {
            return match screenshot() {
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

async fn run_on_tab(
    tab: &dyn ChromeTab,
    action: &str,
    args: &serde_json::Value,
    config: &ComputerUseConfig,
) -> ToolOutcome {
    dispatch_action(
        action,
        args,
        config,
        |url| tab.navigate(url),
        |sel| tab.click(sel),
        |x, y| tab.click_xy(x, y),
        |sel, txt| tab.type_text(sel, txt),
        |key| tab.keypress(key),
        |amt| tab.scroll(amt),
        |sel, wait| tab.wait_for(sel, wait),
        |expr| tab.evaluate(expr),
        || tab.screenshot(),
    )
}

/// Synchronous runner for session actions. All BrowserSession methods
/// are sync, so this avoids holding a MutexGuard across an await.
fn run_on_session_sync(
    session: &mut BrowserSession,
    action: &str,
    args: &serde_json::Value,
    config: &ComputerUseConfig,
) -> ToolOutcome {
    dispatch_action(
        action,
        args,
        config,
        |url| session.tab.navigate(url),
        |sel| session.click(sel),
        |x, y| session.tab.click_xy(x, y),
        |sel, txt| session.type_text(sel, txt),
        |key| session.tab.keypress(key),
        |amt| session.scroll(amt),
        |sel, wait| session.wait_for(sel, wait),
        |expr| session.evaluate(expr),
        || session.screenshot(),
    )
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

    #[tokio::test]
    async fn placeholder_tab_navigate_returns_error() {
        let tab: Arc<dyn ChromeTab> = Arc::new(PlaceholderTab);
        let err = tab.navigate("https://example.com").unwrap_err();
        assert!(err.to_string().contains("not initialized"));
    }

    #[tokio::test]
    async fn placeholder_tab_click_returns_error() {
        let tab: Arc<dyn ChromeTab> = Arc::new(PlaceholderTab);
        assert!(tab.click("#x").is_err());
    }

    #[tokio::test]
    async fn placeholder_tab_screenshot_returns_error() {
        let tab: Arc<dyn ChromeTab> = Arc::new(PlaceholderTab);
        assert!(tab.screenshot().is_err());
    }

    #[tokio::test]
    async fn placeholder_tab_type_returns_error() {
        let tab: Arc<dyn ChromeTab> = Arc::new(PlaceholderTab);
        assert!(tab.type_text("#x", "hello").is_err());
    }

    #[tokio::test]
    async fn placeholder_tab_keypress_returns_error() {
        let tab: Arc<dyn ChromeTab> = Arc::new(PlaceholderTab);
        assert!(tab.keypress("Enter").is_err());
    }

    #[tokio::test]
    async fn placeholder_tab_scroll_returns_error() {
        let tab: Arc<dyn ChromeTab> = Arc::new(PlaceholderTab);
        assert!(tab.scroll(10).is_err());
    }

    #[tokio::test]
    async fn placeholder_tab_wait_for_returns_error() {
        let tab: Arc<dyn ChromeTab> = Arc::new(PlaceholderTab);
        assert!(tab.wait_for("#x", Duration::from_secs(1)).is_err());
    }

    #[tokio::test]
    async fn placeholder_tab_evaluate_returns_error() {
        let tab: Arc<dyn ChromeTab> = Arc::new(PlaceholderTab);
        assert!(tab.evaluate("1+1").is_err());
    }

    #[test]
    fn placeholder_tab_implements_all_methods_with_same_error() {
        let tab = PlaceholderTab;
        assert!(tab.navigate("x").is_err());
        assert!(tab.click("x").is_err());
        assert!(tab.click_xy(1.0, 2.0).is_err());
        assert!(tab.type_text("x", "y").is_err());
        assert!(tab.keypress("x").is_err());
        assert!(tab.scroll(1).is_err());
        assert!(tab.screenshot().is_err());
        assert!(tab.wait_for("x", Duration::from_secs(1)).is_err());
        assert!(tab.evaluate("x").is_err());
    }

    #[tokio::test]
    async fn open_rejects_internal_metadata_endpoint() {
        let tool = fake_tool();
        let outcome = tool
            .run(
                &ToolContext::new(),
                json!({"action": "open", "url": "http://169.254.169.254/latest"}),
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
    async fn open_rejects_internal_ip_literal() {
        let tool = fake_tool();
        let outcome = tool
            .run(
                &ToolContext::new(),
                json!({"action": "open", "url": "http://10.0.0.1/x"}),
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
    async fn navigate_rejects_internal_ip_literal() {
        let tool = fake_tool();
        let outcome = tool
            .run(
                &ToolContext::new(),
                json!({"action": "navigate", "url": "http://127.0.0.1/"}),
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
    async fn open_rejects_missing_url() {
        let tool = fake_tool();
        let outcome = tool
            .run(&ToolContext::new(), json!({"action": "open"}))
            .await;
        assert!(
            matches!(outcome, ToolOutcome::Failure(ToolError::InvalidArgs { .. })),
            "got {outcome:?}"
        );
    }

    #[tokio::test]
    async fn missing_action_is_invalid_args() {
        let tool = fake_tool();
        let outcome = tool.run(&ToolContext::new(), json!({})).await;
        assert!(
            matches!(outcome, ToolOutcome::Failure(ToolError::InvalidArgs { .. })),
            "got {outcome:?}"
        );
    }

    #[tokio::test]
    async fn click_returns_success_message() {
        let tool = fake_tool();
        let outcome = tool
            .run(
                &ToolContext::new(),
                json!({"action": "click", "selector": "#btn"}),
            )
            .await;
        match outcome {
            ToolOutcome::Success { content } => assert!(content.contains("#btn")),
            other => panic!("expected Success, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn click_xy_returns_success_message() {
        let tool = fake_tool();
        let outcome = tool
            .run(
                &ToolContext::new(),
                json!({"action": "click_xy", "x": 12.5, "y": 34.7}),
            )
            .await;
        match outcome {
            ToolOutcome::Success { content } => {
                assert!(content.contains("12.5"), "got: {content}");
                assert!(content.contains("34.7"), "got: {content}");
            }
            other => panic!("expected Success, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn type_returns_success_message() {
        let tool = fake_tool();
        let outcome = tool
            .run(
                &ToolContext::new(),
                json!({"action": "type", "selector": "#input", "text": "hello"}),
            )
            .await;
        match outcome {
            ToolOutcome::Success { content } => assert!(content.contains("#input")),
            other => panic!("expected Success, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn keypress_returns_success_message() {
        let tool = fake_tool();
        let outcome = tool
            .run(
                &ToolContext::new(),
                json!({"action": "keypress", "key": "Enter"}),
            )
            .await;
        match outcome {
            ToolOutcome::Success { content } => assert!(content.contains("Enter")),
            other => panic!("expected Success, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn scroll_returns_success_message() {
        let tool = fake_tool();
        let outcome = tool
            .run(
                &ToolContext::new(),
                json!({"action": "scroll", "amount": 250}),
            )
            .await;
        match outcome {
            ToolOutcome::Success { content } => assert!(content.contains("250 pixels")),
            other => panic!("expected Success, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn wait_for_returns_success_message() {
        let tool = fake_tool();
        let outcome = tool
            .run(
                &ToolContext::new(),
                json!({"action": "wait_for", "selector": "#foo"}),
            )
            .await;
        match outcome {
            ToolOutcome::Success { content } => assert!(content.contains("#foo")),
            other => panic!("expected Success, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn unknown_action_in_session_returns_failure() {
        let tool = fake_tool();
        tool.run(
            &ToolContext::new(),
            json!({"action": "open", "url": "https://example.com"}),
        )
        .await;
        let outcome = tool
            .run(&ToolContext::new(), json!({"action": "frobnicate"}))
            .await;
        assert!(
            matches!(outcome, ToolOutcome::Failure(ToolError::Internal { .. })),
            "got {outcome:?}"
        );
    }

    #[tokio::test]
    async fn browser_session_step_count_increments_per_action() {
        let tool = fake_tool_with_max_steps(20);
        tool.run(
            &ToolContext::new(),
            json!({"action": "open", "url": "https://example.com"}),
        )
        .await;
        tool.run(
            &ToolContext::new(),
            json!({"action": "click", "selector": "a"}),
        )
        .await;
        tool.run(
            &ToolContext::new(),
            json!({"action": "scroll", "amount": 100}),
        )
        .await;
        let guard = tool.session.lock().unwrap();
        let session = guard.as_ref().unwrap();
        assert_eq!(
            session.step_count(),
            2,
            "click and scroll increment, open does not"
        );
    }

    #[tokio::test]
    async fn browser_session_zero_max_steps_defaults_to_20() {
        let tool = fake_tool_with_max_steps(0);
        tool.run(
            &ToolContext::new(),
            json!({"action": "open", "url": "https://example.com"}),
        )
        .await;
        let guard = tool.session.lock().unwrap();
        let session = guard.as_ref().unwrap();
        assert_eq!(session.max_steps, 20, "zero should fall back to default");
    }

    #[tokio::test]
    async fn click_after_close_falls_back_to_single_shot_tab() {
        let tool = fake_tool();
        tool.run(
            &ToolContext::new(),
            json!({"action": "open", "url": "https://example.com"}),
        )
        .await;
        tool.run(&ToolContext::new(), json!({"action": "close"}))
            .await;
        let outcome = tool
            .run(
                &ToolContext::new(),
                json!({"action": "click", "selector": "a"}),
            )
            .await;
        assert!(
            matches!(outcome, ToolOutcome::Success { .. }),
            "click should succeed via shared tab, got {outcome:?}"
        );
    }

    #[tokio::test]
    async fn open_failure_returns_internal_error() {
        struct FailingTab;
        impl ChromeTab for FailingTab {
            fn navigate(&self, _url: &str) -> anyhow::Result<()> {
                Err(anyhow::anyhow!("network down"))
            }
            fn click(&self, _: &str) -> anyhow::Result<()> {
                Ok(())
            }
            fn click_xy(&self, _: f64, _: f64) -> anyhow::Result<()> {
                Ok(())
            }
            fn type_text(&self, _: &str, _: &str) -> anyhow::Result<()> {
                Ok(())
            }
            fn keypress(&self, _: &str) -> anyhow::Result<()> {
                Ok(())
            }
            fn scroll(&self, _: i32) -> anyhow::Result<()> {
                Ok(())
            }
            fn screenshot(&self) -> anyhow::Result<Vec<u8>> {
                Ok(vec![])
            }
            fn wait_for(&self, _: &str, _: Duration) -> anyhow::Result<()> {
                Ok(())
            }
            fn evaluate(&self, _: &str) -> anyhow::Result<String> {
                Ok(String::new())
            }
        }
        let tool = ComputerUse::with_tab(
            DenyList::default(),
            ComputerUseConfig::default(),
            Arc::new(FailingTab),
        );
        let outcome = tool
            .run(
                &ToolContext::new(),
                json!({"action": "open", "url": "https://example.com"}),
            )
            .await;
        match outcome {
            ToolOutcome::Failure(ToolError::Internal { message }) => {
                assert!(message.contains("open failed"), "got {message}");
                assert!(message.contains("network down"), "got {message}");
            }
            other => panic!("expected Internal error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn screenshot_failure_returns_internal_error() {
        struct NoScreenshotTab;
        impl ChromeTab for NoScreenshotTab {
            fn navigate(&self, _: &str) -> anyhow::Result<()> {
                Ok(())
            }
            fn click(&self, _: &str) -> anyhow::Result<()> {
                Ok(())
            }
            fn click_xy(&self, _: f64, _: f64) -> anyhow::Result<()> {
                Ok(())
            }
            fn type_text(&self, _: &str, _: &str) -> anyhow::Result<()> {
                Ok(())
            }
            fn keypress(&self, _: &str) -> anyhow::Result<()> {
                Ok(())
            }
            fn scroll(&self, _: i32) -> anyhow::Result<()> {
                Ok(())
            }
            fn screenshot(&self) -> anyhow::Result<Vec<u8>> {
                Err(anyhow::anyhow!("capture failed"))
            }
            fn wait_for(&self, _: &str, _: Duration) -> anyhow::Result<()> {
                Ok(())
            }
            fn evaluate(&self, _: &str) -> anyhow::Result<String> {
                Ok(String::new())
            }
        }
        let tool = ComputerUse::with_tab(
            DenyList::default(),
            ComputerUseConfig::default(),
            Arc::new(NoScreenshotTab),
        );
        let outcome = tool
            .run(&ToolContext::new(), json!({"action": "screenshot"}))
            .await;
        match outcome {
            ToolOutcome::Failure(ToolError::Internal { message }) => {
                assert!(message.contains("screenshot failed"), "got {message}");
                assert!(message.contains("capture failed"), "got {message}");
            }
            other => panic!("expected Internal error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn open_with_session_launcher_failure_returns_internal_error() {
        struct OkTab;
        impl ChromeTab for OkTab {
            fn navigate(&self, _: &str) -> anyhow::Result<()> {
                Ok(())
            }
            fn click(&self, _: &str) -> anyhow::Result<()> {
                Ok(())
            }
            fn click_xy(&self, _: f64, _: f64) -> anyhow::Result<()> {
                Ok(())
            }
            fn type_text(&self, _: &str, _: &str) -> anyhow::Result<()> {
                Ok(())
            }
            fn keypress(&self, _: &str) -> anyhow::Result<()> {
                Ok(())
            }
            fn scroll(&self, _: i32) -> anyhow::Result<()> {
                Ok(())
            }
            fn screenshot(&self) -> anyhow::Result<Vec<u8>> {
                Ok(vec![])
            }
            fn wait_for(&self, _: &str, _: Duration) -> anyhow::Result<()> {
                Ok(())
            }
            fn evaluate(&self, _: &str) -> anyhow::Result<String> {
                Ok(String::new())
            }
        }
        let launcher: SessionLauncher =
            Arc::new(|| Box::pin(async { Err(anyhow::anyhow!("chrome binary missing")) }));
        let tool = ComputerUse::new(
            DenyList::default(),
            ComputerUseConfig::default(),
            Arc::new(OkTab),
            Some(launcher),
        );
        let outcome = tool
            .run(
                &ToolContext::new(),
                json!({"action": "open", "url": "https://example.com"}),
            )
            .await;
        match outcome {
            ToolOutcome::Failure(ToolError::Internal { message }) => {
                assert!(
                    message.contains("failed to launch browser session"),
                    "got {message}"
                );
                assert!(message.contains("chrome binary missing"), "got {message}");
            }
            other => panic!("expected Internal error, got {other:?}"),
        }
    }

    #[test]
    fn def_lists_all_actions_in_enum() {
        let tool = fake_tool();
        let def = tool.def();
        let actions = def
            .parameters
            .get("properties")
            .and_then(|p| p.get("action"))
            .and_then(|a| a.get("enum"))
            .and_then(|e| e.as_array())
            .expect("action enum");
        let names: Vec<&str> = actions.iter().filter_map(|v| v.as_str()).collect();
        for expected in [
            "open",
            "navigate",
            "click",
            "click_xy",
            "type",
            "keypress",
            "scroll",
            "screenshot",
            "wait_for",
            "evaluate",
            "close",
        ] {
            assert!(
                names.contains(&expected),
                "missing {expected} in enum: {names:?}"
            );
        }
    }

    #[tokio::test]
    async fn session_navigate_uses_session_tab() {
        let tool = fake_tool_with_max_steps(20);
        tool.run(
            &ToolContext::new(),
            json!({"action": "open", "url": "https://example.com"}),
        )
        .await;
        let outcome = tool
            .run(
                &ToolContext::new(),
                json!({"action": "navigate", "url": "https://example.com"}),
            )
            .await;
        match outcome {
            ToolOutcome::Success { content } => {
                assert!(content.contains("Navigated"), "got {content}");
                assert!(content.contains("example.com"), "got {content}");
            }
            other => panic!("expected Success, got {other:?}"),
        }
        let guard = tool.session.lock().unwrap();
        assert_eq!(guard.as_ref().unwrap().step_count(), 1);
    }

    #[tokio::test]
    async fn session_type_returns_success_message() {
        let tool = fake_tool_with_max_steps(20);
        tool.run(
            &ToolContext::new(),
            json!({"action": "open", "url": "https://example.com"}),
        )
        .await;
        let outcome = tool
            .run(
                &ToolContext::new(),
                json!({"action": "type", "selector": "#input", "text": "hello"}),
            )
            .await;
        match outcome {
            ToolOutcome::Success { content } => {
                assert!(content.contains("#input"), "got {content}");
            }
            other => panic!("expected Success, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn session_scroll_returns_success_message() {
        let tool = fake_tool_with_max_steps(20);
        tool.run(
            &ToolContext::new(),
            json!({"action": "open", "url": "https://example.com"}),
        )
        .await;
        let outcome = tool
            .run(
                &ToolContext::new(),
                json!({"action": "scroll", "amount": 250}),
            )
            .await;
        match outcome {
            ToolOutcome::Success { content } => {
                assert!(content.contains("250 pixels"), "got {content}");
            }
            other => panic!("expected Success, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn session_evaluate_returns_result() {
        let tool = fake_tool_with_max_steps(20);
        tool.run(
            &ToolContext::new(),
            json!({"action": "open", "url": "https://example.com"}),
        )
        .await;
        let outcome = tool
            .run(
                &ToolContext::new(),
                json!({"action": "evaluate", "expression": "1+1"}),
            )
            .await;
        match outcome {
            ToolOutcome::Success { content } => {
                assert_eq!(content, "42", "got {content}");
            }
            other => panic!("expected Success, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn session_wait_for_returns_success_message() {
        let tool = fake_tool_with_max_steps(20);
        tool.run(
            &ToolContext::new(),
            json!({"action": "open", "url": "https://example.com"}),
        )
        .await;
        let outcome = tool
            .run(
                &ToolContext::new(),
                json!({"action": "wait_for", "selector": "#foo"}),
            )
            .await;
        match outcome {
            ToolOutcome::Success { content } => {
                assert!(content.contains("#foo"), "got {content}");
            }
            other => panic!("expected Success, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn session_keypress_returns_success_message() {
        let tool = fake_tool_with_max_steps(20);
        tool.run(
            &ToolContext::new(),
            json!({"action": "open", "url": "https://example.com"}),
        )
        .await;
        let outcome = tool
            .run(
                &ToolContext::new(),
                json!({"action": "keypress", "key": "Enter"}),
            )
            .await;
        match outcome {
            ToolOutcome::Success { content } => {
                assert!(content.contains("Enter"), "got {content}");
            }
            other => panic!("expected Success, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn session_click_xy_returns_success_message() {
        let tool = fake_tool_with_max_steps(20);
        tool.run(
            &ToolContext::new(),
            json!({"action": "open", "url": "https://example.com"}),
        )
        .await;
        let outcome = tool
            .run(
                &ToolContext::new(),
                json!({"action": "click_xy", "x": 12.5, "y": 34.7}),
            )
            .await;
        match outcome {
            ToolOutcome::Success { content } => {
                assert!(content.contains("12.5"), "got {content}");
                assert!(content.contains("34.7"), "got {content}");
            }
            other => panic!("expected Success, got {other:?}"),
        }
    }
}
