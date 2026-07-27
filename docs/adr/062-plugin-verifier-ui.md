# ADR-062: Plugin verifier results in `/verify` panel + cost report

- **Status:** Accepted
- **Date:** 2026-07-27

## Context

Plugin-declared verifiers register into the Rust `VerifierBus` (WO 7.7/9.6)
and flow into the correction loop. But the verifier results were not
surfaced in the UI or the cost report. A user running `kirkforge run`
saw the correction loop fix errors, but there was no `/verify` panel
showing "here are the verifier verdicts from the last turn," and the
`MetricEvent::Verifier` did not include the source (built-in vs
plugin:name), so the cost report couldn't attribute.

## Decision

1. **`MetricEvent::Verifier` gains a `source: String` field.** The
   value is `"built-in"` for built-in verifiers or `"plugin:<name>"` for
   plugin-declared ones. Additive — old NDJSON logs without the field
   default to `"built-in"` via `#[serde(default)]`. The OTel
   attributes now include `verifier.source`.

2. **`/verify` TUI slash command.** Reads the metrics NDJSON log for
   the last 20 `MetricEvent::Verifier` entries and renders them as a
   table: `Verifier | Source | Verdict`. No executor plumbing needed —
   the metrics log already records every verdict; the command just
   queries it. This works for both TUI and headless modes.

3. **`kirkforge verify` CLI subcommand.** Prints the same table to
   stdout for headless / CI users.

4. **`format_verdict_report` on `VerifierBus`.** A free function that
   formats the bus's in-memory verdicts (from the last `run()`) as a
   table with `Verifier | Source | Verdict | File:Line | Message`. This
   is the future per-turn report; the current `/verify` command uses
   the metrics-log-based `format_verifier_report` for simplicity (no
   executor plumbing needed), but the bus-side formatter is available
   for when the executor exposes the bus to the TUI.

## Consequences

- Users can now ask "what did the verifiers say?" via `/verify` (TUI)
  or `kirkforge verify` (CLI).
- The cost report (metrics NDJSON) distinguishes built-in vs plugin
  verifiers via the `source` field.
- Old metrics logs still parse (the `source` field defaults to
  `"built-in"`).
- The `/verify` command reads the metrics log, not the live bus — so
  it shows the *history* of verdicts, not just the current turn. This
  is the headless-friendly path (no executor handle needed). A future
  per-turn panel can use `format_verdict_report(bus.verdicts())`.

## Notes

- The `ponytail:` annotations in `src/session/verifier/bus.rs` (the
  `VerifierSource::Display` impl) are preserved.
- The metric `source` field is additive (serde `#[serde(default)]`).