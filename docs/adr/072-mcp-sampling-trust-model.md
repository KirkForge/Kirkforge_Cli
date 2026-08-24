# ADR-072: MCP Sampling Trust Model and Approval Flow

<!-- adr-predicates
status: accepted
implemented: true
supersedes: []
affects-crates: []
-->

**Status:** Accepted
**Date:** 2026-08-10
**Replaces:** none (new decision)

## Context

MCP (Model Context Protocol) servers can request model completions from the
client via `sampling/createMessage`. This is the reverse of a tool call: the
server initiates a model invocation, and the client decides whether to comply.
Sampling has a real security surface — a server can drive model inference and
see the result, and an unvetted server could use it to exfiltrate context or
burn tokens. kf-code previously had no handler for `sampling/createMessage`;
the request fell through to the "unhandled server request" branch and was
silently ignored, with `sampling` still listed in `UNSUPPORTED_CAPABILITIES`.

## Decision

Add an approval-gated handler for `sampling/createMessage` that routes through
the **same approval bus** that gates tool calls — the `ApprovalRequest` /
`ApprovalResponder` channel created by the session driver. The handler MUST
NOT run a model completion without going through that bus.

The flow is:

1. A server sends `sampling/createMessage`.
2. The client sends an `ApprovalRequest` (tool name
   `mcp/sampling/createMessage/<server>`, args carrying the server + params)
   on the shared approval channel. The TUI / line-mode handler presents it to
   the user exactly like a tool-approval prompt.
3. On approval, the client builds a one-off completion adapter from the
   session `Config` (`adapters::sampling_adapter`) and streams the completion,
   returning the text as an MCP `content` block. The executor's live adapter
   (and its swap/caching state) is untouched.
4. On denial, the client returns an MCP JSON-RPC error (`code: -32000`) so the
   server knows the completion was refused.

### Headless policy

In headless / non-interactive mode there is no human to prompt, so the
existing non-interactive approval handler denies every request by default —
matching current tool-approval behavior. An operator may opt in for trusted
servers via the config flag `tools.allow_sampling_unattended` (default
`false`). When `true`, sampling requests are auto-approved without the bus
(this is the sole bypass, and it is explicit).

## Options considered

- **Ignore sampling entirely** (status quo): keeps `sampling` unsupported but
  leaves a security-relevant MCP capability unimplemented.
- **Auto-run sampling without approval**: simplest, but a silent security hole
  and a violation of the tool-approval trust model.
- **Approval-gated via the existing bus (chosen)**: reuses the established,
  audited approval mechanism; denial is the default in headless mode; the
  single opt-out flag is explicit.

## Consequences

- Sampling requests now surface in the TUI as approval prompts, giving the
  user control over server-initiated model calls.
- Headless runs deny sampling by default; enabling
  `allow_sampling_unattended` is the explicit trust decision.
- The one-off sampling adapter duplicates the `run_session` adapter-construction
  arguments once. If those arguments ever drift, sampling may use a stale
  provider config until updated. `ponytail: sampling adapter mirrors
  run_session; if provider args change, update both call sites.`
- A client that reconnects after `set_sampling` loses the sampling context
  until the session reinstalls it. `ceiling: per-session sampling wiring is
  best-effort across reconnects.` Upgrade path: reinstall the context on
  reconnect inside the manager.
