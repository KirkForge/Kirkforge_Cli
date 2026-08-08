# ADR-061: Plugin hook fail-open audit log

- **Status:** Accepted
- **Date:** 2026-07-27

## Context

Plugin hooks (`src/session/hooks.rs`) follow a fail-open convention
(documented at `crates/kf-plugin-host/src/hook.rs`): exit 0 →
allow, exit 2 → deny, any other non-zero / timeout / crash → allow with
a warning. The warning was `tracing::warn!(...)` — it goes to the
tracing log, not the audit log.

The audit log (`src/shared/audit.rs` `AuditLog`) records tool calls to
an NDJSON file. It's the tamper-evident record of what the agent did.
But hook denials and hook failures (timeouts, crashes) were not in it.
A hook that denies a tool call (exit 2) was logged to `tracing::warn!`
and lost; a hook that crashes (non-zero exit) was fail-opened (the tool
runs anyway) and the warning was lost.

This is a security-observability gap: a malicious or buggy hook that
denies legitimate tool calls (or fails open on dangerous ones) is not
in the audit trail.

## Decision

1. **Add `AuditEntry::Hook` variant.** The `AuditEntry` enum (formerly
   a struct) gains a `Hook { timestamp, event, plugin, verdict, reason,
   session_id }` variant. The `verdict` is `"deny"`, `"allow_fail_open"`,
   or `"allow"`. The `plugin` is `None` for built-in hooks, `Some(name)`
   for plugin hooks. Serialized as NDJSON with a `"kind"` tag
   (`"tool"` / `"hook"`).

2. **Pass the audit log to the hook runner.** `HookRunner` gains an
   optional `audit_log: Option<Arc<AuditLog>>` field. The executor
   sets it via `hook_runner.set_audit_log(audit_log)` after
   constructing both. When `None` (tests), the existing `tracing::warn!`
   path is preserved and no audit entry is written.

3. **Log denials and failures.** In `run_decision` and
   `run_decision_with_context`:
   - A `Deny` verdict (exit 2) → audit `verdict = "deny"` + the reason.
   - A failure (non-zero exit, timeout, spawn error) → audit
     `verdict = "allow_fail_open"` + the error. The tool runs anyway
     (fail-open semantics unchanged).
   - The `tracing::warn!` is kept (live operator signal); the audit log
     is the persistent record. Both are written.

4. **`run_hook_script` returns `Err` for non-zero exits** (not 0 or 2)
   so the `run_decision` Err arm fires the audit + fail-open path. The
   caller converts `Err` → `Allow` (fail-open semantics preserved).

## Consequences

- The audit trail now shows hook denials and fail-open failures
  alongside tool calls. A security review can grep the audit log for
  `"kind":"hook"` + `"verdict":"deny"` or `"verdict":"allow_fail_open"`.
- The fail-open convention is unchanged — a broken hook still cannot
  block the user. The audit log makes the fail-open visible.
- The `AuditEntry` type changed from a struct to a tagged enum. Old
  NDJSON log entries (struct form, no `"kind"` tag) do NOT deserialize
  with the new enum — this is a backward-incompatible log format
  change. New entries are tagged. Existing audit logs are append-only
  NDJSON; a reader that needs to parse both formats can fall back to
  raw JSON. This is acceptable: the audit log is a tamper-evident
  record, not a queryable database, and the old entries are still
  human-readable NDJSON.
- The `plugin_hooks` field in `HookRunner` gained a third element
  (the plugin name) so denials/failures are attributed to the right
  plugin in the audit log.

## Notes

- The `tracing::warn!` path is kept (live operator signal); the audit
  log is the persistent record. Both are written.
- The fail-open convention (ADR-009-documented) is unchanged. This
  WO only adds audit logging; the semantics must not change.
- The `ponytail:` annotations in `src/session/hooks.rs` (the fail-open
  convention comment) are preserved.