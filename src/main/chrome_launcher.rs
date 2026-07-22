//! Chrome launcher for the `computer_use` tool.
//!
//! This module is intentionally separate from the tool itself so that the tool
//! trait (`ChromeTab`) and the production launcher can live next to different
//! sets of `headless_chrome` imports. The launcher runs on the tokio blocking
//! pool because `headless_chrome::Browser::new` is synchronous and can block for
//! several seconds while Chrome starts.

use headless_chrome::browser::tab::point::Point;
use headless_chrome::protocol::cdp::Page::CaptureScreenshotFormatOption;
use std::sync::Arc;
use std::time::Duration;

/// Concrete Chrome tab that implements the `computer_use::ChromeTab` trait.
/// Owns the `Browser` process handle to keep Chrome alive for the tab's lifetime.
struct RealChromeTab {
    _browser: headless_chrome::Browser,
    tab: Arc<headless_chrome::Tab>,
}

impl crate::tools::computer_use::ChromeTab for RealChromeTab {
    fn navigate(&self, url: &str) -> anyhow::Result<()> {
        self.tab
            .navigate_to(url)
            .map_err(|e| anyhow::anyhow!("navigation failed: {e}"))?;
        self.tab
            .wait_until_navigated()
            .map_err(|e| anyhow::anyhow!("navigation did not complete: {e}"))?;
        Ok(())
    }

    fn click(&self, selector: &str) -> anyhow::Result<()> {
        let element = self
            .tab
            .wait_for_element(selector)
            .map_err(|e| anyhow::anyhow!("element not found: {e}"))?;
        element
            .click()
            .map_err(|e| anyhow::anyhow!("click failed: {e}"))?;
        Ok(())
    }

    fn click_xy(&self, x: f64, y: f64) -> anyhow::Result<()> {
        let point = Point { x, y };
        self.tab
            .click_point(point)
            .map_err(|e| anyhow::anyhow!("click failed: {e}"))?;
        Ok(())
    }

    fn type_text(&self, selector: &str, text: &str) -> anyhow::Result<()> {
        let element = self
            .tab
            .wait_for_element(selector)
            .map_err(|e| anyhow::anyhow!("element not found: {e}"))?;
        element
            .type_into(text)
            .map_err(|e| anyhow::anyhow!("type failed: {e}"))?;
        Ok(())
    }

    fn keypress(&self, key: &str) -> anyhow::Result<()> {
        self.tab
            .press_key(key)
            .map_err(|e| anyhow::anyhow!("keypress failed: {e}"))?;
        Ok(())
    }

    fn scroll(&self, amount: i32) -> anyhow::Result<()> {
        let expr = format!("window.scrollBy({{ top: {amount}, left: 0, behavior: 'instant' }})");
        self.tab
            .evaluate(&expr, true)
            .map_err(|e| anyhow::anyhow!("scroll failed: {e}"))?;
        Ok(())
    }

    fn screenshot(&self) -> anyhow::Result<Vec<u8>> {
        self.tab
            .capture_screenshot(CaptureScreenshotFormatOption::Png, None, None, true)
            .map_err(|e| anyhow::anyhow!("screenshot failed: {e}"))
    }

    fn wait_for(&self, selector: &str, _timeout: Duration) -> anyhow::Result<()> {
        self.tab
            .wait_for_element(selector)
            .map_err(|e| anyhow::anyhow!("wait failed: {e}"))?;
        Ok(())
    }

    fn evaluate(&self, expression: &str) -> anyhow::Result<String> {
        let result = self
            .tab
            .evaluate(expression, true)
            .map_err(|e| anyhow::anyhow!("evaluate failed: {e}"))?;
        Ok(result.value.map(|v| v.to_string()).unwrap_or_default())
    }
}

/// Launch a fresh Chrome tab according to `config`.
///
/// Returns an `Arc<dyn ChromeTab>` suitable for passing to `ComputerUse::new`.
/// This function is async only because it runs the synchronous launch on a
/// blocking thread.
pub async fn launch_chrome_tab(
    config: &crate::shared::ComputerUseConfig,
) -> anyhow::Result<Arc<dyn crate::tools::computer_use::ChromeTab>> {
    let config = config.clone();
    tokio::task::spawn_blocking(move || launch_sync(&config))
        .await
        .map_err(|e| anyhow::anyhow!("Chrome launch task panicked: {e}"))?
}

fn launch_sync(
    config: &crate::shared::ComputerUseConfig,
) -> anyhow::Result<Arc<dyn crate::tools::computer_use::ChromeTab>> {
    let (browser, tab) = launch_browser_and_tab(config)?;
    Ok(Arc::new(RealChromeTab {
        _browser: browser,
        tab,
    }))
}

/// Launch Chrome and return both the Browser process handle and the active Tab.
/// The caller must keep the `Browser` alive for the `Tab` to remain functional.
fn launch_browser_and_tab(
    config: &crate::shared::ComputerUseConfig,
) -> anyhow::Result<(headless_chrome::Browser, Arc<headless_chrome::Tab>)> {
    let mut builder = headless_chrome::LaunchOptions::default_builder();
    builder.headless(!config.headful);
    builder.sandbox(false);
    if let Some(path) = &config.chrome_path {
        builder.path(Some(path.clone()));
    }
    builder.window_size(Some((config.width, config.height)));
    let options = builder
        .build()
        .map_err(|e| anyhow::anyhow!("failed to build Chrome launch options: {e}"))?;
    let browser = headless_chrome::Browser::new(options)
        .map_err(|e| anyhow::anyhow!("failed to launch Chrome: {e}"))?;
    let tab = browser
        .new_tab()
        .map_err(|e| anyhow::anyhow!("failed to create Chrome tab: {e}"))?;
    Ok((browser, tab))
}

/// Owns the Browser process for a multi-step browser session.
/// Keeps Chrome alive for the session's lifetime. Implements `ChromeTab`
/// by delegating to the inner `Tab`.
pub struct BrowserSessionOwner {
    _browser: headless_chrome::Browser,
    tab: Arc<headless_chrome::Tab>,
    _step: u32,
    _max_steps: u32,
}

impl crate::tools::computer_use::ChromeTab for BrowserSessionOwner {
    fn navigate(&self, url: &str) -> anyhow::Result<()> {
        self.tab
            .navigate_to(url)
            .map_err(|e| anyhow::anyhow!("navigation failed: {e}"))?;
        self.tab
            .wait_until_navigated()
            .map_err(|e| anyhow::anyhow!("navigation did not complete: {e}"))?;
        Ok(())
    }

    fn click(&self, selector: &str) -> anyhow::Result<()> {
        let element = self
            .tab
            .wait_for_element(selector)
            .map_err(|e| anyhow::anyhow!("element not found: {e}"))?;
        element
            .click()
            .map_err(|e| anyhow::anyhow!("click failed: {e}"))?;
        Ok(())
    }

    fn click_xy(&self, x: f64, y: f64) -> anyhow::Result<()> {
        let point = Point { x, y };
        self.tab
            .click_point(point)
            .map_err(|e| anyhow::anyhow!("click failed: {e}"))?;
        Ok(())
    }

    fn type_text(&self, selector: &str, text: &str) -> anyhow::Result<()> {
        let element = self
            .tab
            .wait_for_element(selector)
            .map_err(|e| anyhow::anyhow!("element not found: {e}"))?;
        element
            .type_into(text)
            .map_err(|e| anyhow::anyhow!("type failed: {e}"))?;
        Ok(())
    }

    fn keypress(&self, key: &str) -> anyhow::Result<()> {
        self.tab
            .press_key(key)
            .map_err(|e| anyhow::anyhow!("keypress failed: {e}"))?;
        Ok(())
    }

    fn scroll(&self, amount: i32) -> anyhow::Result<()> {
        let expr = format!("window.scrollBy({{ top: {amount}, left: 0, behavior: 'instant' }})");
        self.tab
            .evaluate(&expr, true)
            .map_err(|e| anyhow::anyhow!("scroll failed: {e}"))?;
        Ok(())
    }

    fn screenshot(&self) -> anyhow::Result<Vec<u8>> {
        self.tab
            .capture_screenshot(CaptureScreenshotFormatOption::Png, None, None, true)
            .map_err(|e| anyhow::anyhow!("screenshot failed: {e}"))
    }

    fn wait_for(&self, selector: &str, _timeout: Duration) -> anyhow::Result<()> {
        self.tab
            .wait_for_element(selector)
            .map_err(|e| anyhow::anyhow!("wait failed: {e}"))?;
        Ok(())
    }

    fn evaluate(&self, expression: &str) -> anyhow::Result<String> {
        let result = self
            .tab
            .evaluate(expression, true)
            .map_err(|e| anyhow::anyhow!("evaluate failed: {e}"))?;
        Ok(result.value.map(|v| v.to_string()).unwrap_or_default())
    }
}

/// Launch a fresh browser session that owns its Chrome process.
/// Returns an `Arc<dyn ChromeTab>` that keeps Chrome alive until dropped.
/// Use this for multi-step sessions where the `open` action needs a
/// dedicated browser instance that won't be killed mid-session.
pub async fn open_browser_session(
    config: &crate::shared::ComputerUseConfig,
) -> anyhow::Result<Arc<dyn crate::tools::computer_use::ChromeTab>> {
    let config = config.clone();
    tokio::task::spawn_blocking(move || {
        let (browser, tab) = launch_browser_and_tab(&config)?;
        Ok(Arc::new(BrowserSessionOwner {
            _browser: browser,
            tab,
            _step: 0,
            _max_steps: config.max_steps,
        }) as Arc<dyn crate::tools::computer_use::ChromeTab>)
    })
    .await
    .map_err(|e| anyhow::anyhow!("Chrome session launch task panicked: {e}"))?
}
