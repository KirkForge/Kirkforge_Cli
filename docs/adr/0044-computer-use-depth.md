# ADR-044: Computer-use depth (multi-step browser flows)

- **Status:** Accepted

## Context
The computer_use tool existed at 383L but was single-shot — one screenshot, one action, done. Real computer-use needs multi-step browser flows with a persistent session, vision-grounded UI reasoning, and session management.

## Decision
Add BrowserSession with open/click/type/scroll/screenshot/evaluate/close. Session persists across tool calls within the same conversation. max_steps prevents infinite loops (default 20). Updated JSON schema to support open and close actions.

## Consequences
Positive: multi-step browser automation, vision-grounded reasoning loop.
Negative: headless Chrome dependency, session state management complexity, binary size increase.
