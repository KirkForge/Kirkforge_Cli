# ADR-067: TUI and daemon coverage exclusion rationale

- **Status:** Accepted
- **Date:** 2026-08-06

## Context

The CI coverage gate (ADR-065) enforces line-coverage thresholds on
`src/session`, `src/tools`, and `src/adapters`. Two major subsystems
are explicitly excluded from the gate: `src/tui` (terminal rendering)
and `src/daemon` (socket IPC). This ADR documents the exclusion and
the manual test plan that replaces automated coverage for those paths.

## Decision

### Exclusion rationale

1. **`src/tui`**: Terminal rendering code (`ratatui` widgets, crossterm
   event handling, layout arithmetic) requires a real terminal or a
   pty-emulating harness to exercise. Unit tests for rendering code
   assert pixel-level layout that is tightly coupled to the framework's
   internal state — such tests are brittle and low-value. The TUI is
   integration-tested via `kf-code` in a terminal (or `TmuxDriver` harness
   planned in WO 19.6).

2. **`src/daemon`**: The daemon is a long-running process with Unix
   socket IPC, lock-file management, and process lifecycle. Testing it
   requires subprocess management, socket juggling, and PID-file races.
   Integration tests in WO 19.5 already cover daemon auth token
   enforcement and job lifecycle timeout. The remaining daemon code
   (socket bind, lock-file, graceful shutdown) is verified by the
   integration test suite, not unit tests.

Both subsystems are excluded from the `--lib` tarpaulin run, which
is the CI gate. Integration tests live in `tests/` and are run by the
`quality` job separately.

### Manual test plan

Every PR that touches `src/tui/` or `src/daemon/` must include at
least one of:

- An integration test in `tests/` that exercises the changed path.
- A reproduction step in the PR description run against a live
  `kf-code` binary in a terminal.
- An update to the TmuxDriver scenarios (WO 19.6) when those land.

## Consequences

- `src/tui` and `src/daemon` will not have automated line-coverage
  gates. This is intentional: a low threshold would be meaningless
  (too easy to game), and a high threshold would be unreachable
  without a terminal harness.
- The exclusion does not mean untested — it means tested via
  integration tests, not unit tests.
- If a tarpaulin-compatible terminal harness becomes available, this
  ADR should be revisited.
