# ADR-044: Computer-use depth (multi-step browser flows)

- **Status:** Accepted (partially implemented)

## Context
The computer_use tool existed at 383L but was single-shot — one screenshot, one action, done. Real computer-use needs multi-step browser flows with a persistent session, vision-grounded UI reasoning, and session management.

## Decision
Add BrowserSession with open/click/type/scroll/screenshot/evaluate/close. Session persists across tool calls within the same conversation. max_steps prevents infinite loops (default 20). Updated JSON schema to support open and close actions.

Session lifecycle: the `open` action creates a fresh Chrome instance per session via `SessionLauncher` (keeps `headless_chrome::Browser` alive for the session's lifetime). The `close` action drops the session, which drops the Browser process. This ensures Chrome stays alive during multi-step flows and shuts down cleanly on close.

## What shipped
- `BrowserSession` struct with step counter and `max_steps` limit (default 20)
- `open`/`close` actions that create/destroy browser sessions
- `SessionLauncher` async factory for per-session Chrome instances
- `BrowserSessionOwner` in `chrome_launcher.rs` that owns both `Browser` and `Tab`
- `step_count()` getter for test assertions
- All existing single-shot actions work within a session (step counter increments)
- 16 unit tests (2 ignored Chrome integration tests), all passing
- URL validation (http/https only, SSRF deny list) on both `open` and `navigate`

## What is still needed for full "depth"
- The "vision-grounded UI reasoning loop" (model sees screenshot → decides next action → repeat) is not a tool-level feature — it's an executor-level orchestration that relies on the model's tool-calling loop. The tool already supports this pattern: the model calls `screenshot`, sees the result, decides the next action. The loop is implicit in the tool-calling protocol, not explicit in the tool.
- `#[ignore = "requires headless Chrome"]` integration tests exist but need a local Chrome installation to run.

## Consequences
Positive: multi-step browser automation with per-session Chrome lifecycle, step limit prevents infinite loops, session state persists across tool calls.
Negative: headless Chrome dependency, session state management complexity, binary size increase from `headless_chrome` crate.