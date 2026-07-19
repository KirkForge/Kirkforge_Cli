# ADR 0021: `computer_use` tool via headless Chrome CDP

- **Status:** Accepted
- **Date:** 2026-07-19

## Context

The model sometimes needs to interact with web pages: fill forms, click through dashboards, or take screenshots of the current state. KirkForge has had `web_fetch` for static HTML and `read_image` for local images, but no way to drive a browser. Anthropic's `computer_use` capability demonstrates that vision models can control a desktop session when given screenshot feedback. We want a similar capability for KirkForge that works with any vision-capable model and does not require a third-party API.

## Decision

Add a `computer_use` tool backed by a local headless Chrome instance controlled through the Chrome DevTools Protocol (CDP) via the `headless_chrome` crate.

### Actions supported

- `navigate`: load a public http(s) URL.
- `click`: click an element by CSS selector.
- `click_xy`: click at viewport coordinates.
- `type`: type text into an element by CSS selector.
- `keypress`: press a named key (e.g. `Enter`, `Tab`).
- `scroll`: scroll the page by a pixel amount.
- `screenshot`: capture a PNG screenshot of the current page.
- `wait_for`: wait until an element is present.
- `evaluate`: run a JavaScript expression and return the stringified result.

### Security constraints

- Only `http://` and `https://` URLs are accepted; the same deny-list and literal-internal-IP checks used by `web_fetch` are reused.
- The tool is registered only when `Config::computer_use::enabled` is `true` **and** the active model adapter reports `supports_images: true`.
- Chrome is launched with `--no-sandbox` to avoid failures in container/cloud environments; operators who want full sandboxing can configure `chrome_path` to a system Chrome binary and we will extend the flag set in a future iteration.

### Architecture

- `src/tools/computer_use.rs` defines the `Tool` impl and a `ChromeTab` trait. The trait abstracts the real CDP tab so tests can inject a fake and so the real launcher can live next to `headless_chrome` imports.
- `src/main/chrome_launcher.rs` constructs `headless_chrome::Browser`, launches a fresh tab, and wraps it in a `ChromeTab` implementation on a tokio blocking thread.
- Screenshots are returned as `ToolOutcome::Image` so the existing executor path (`handle_tool_outcome`) splices them back into the conversation as a vision input.
- If Chrome is unavailable at startup, the tool is still registered but uses a `PlaceholderTab` that fails gracefully at runtime rather than crashing the session.

### Configuration

```toml
[computer_use]
enabled = false
chrome_path = ""          # optional explicit binary
headful = false          # set true for visible debugging
width = 1280
height = 800
startup_timeout_secs = 30
wait_timeout_secs = 10
```

All fields also have `KIRKFORGE_COMPUTER_USE_*` environment-variable overrides.

## Consequences

- Vision models can now inspect and manipulate live web pages without leaving the CLI.
- The dependency tree grows by `headless_chrome`, but the tool is opt-in via config so users without Chrome pay only the compile-time cost.
- Tests use a fake `ChromeTab` because the CI environment may not have Chrome installed.

## Future work

- Add per-page cookie/authorization support so the model can interact with authenticated apps.
- Gate `headless_chrome` behind a Cargo feature for users who want a smaller default binary.
