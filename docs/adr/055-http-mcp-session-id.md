# ADR-055: HTTP MCP session-id tracking + resumable streams

- **Status:** Accepted
- **Date:** 2026-07-27

## Context

The HTTP/SSE MCP transport (`src/session/mcp_client/http.rs`) shipped in
WO 6.x as a real streamable-HTTP client: GET `/sse` opens the event
stream, POST `/messages` sends JSON-RPC, SSE `message` events are routed
back by id. But it had a documented gap at `http.rs:395`:

> We do not yet track session ids in this minimal implementation; the
> server is expected to route by the open SSE connection.

The MCP streamable-HTTP spec (2025-06-18, §Session Management)
recommends sending an `Mcp-Session-Id` header on all subsequent HTTP
requests once the server provides it. The SSE spec supports
`Last-Event-ID` on reconnect to resume from the last received event id.
The client omitted both, which means:

1. No session resumption — a dropped SSE connection starts a fresh
   session, losing server-side state.
2. No server-side routing for stateful MCP servers (e.g. the
   Sourcegraph one) that use the session id to route to the right
   backend.
3. No `Last-Event-ID` resumption — events sent during the disconnect
   are lost.

## Decision

Add session-id tracking, `Last-Event-ID` resumption, and a
reconnect-with-backoff loop to the HTTP MCP transport.

### Session-id sources

The transport supports both the old HTTP+SSE transport (2024-11-05) and
the new streamable-HTTP transport (2025-06-18):

1. **Old transport**: the `endpoint` SSE event carries the POST URL,
   which may include a `session_id` (or `sessionId`) query param. The
   SSE reader parses this from the `endpoint` event's data payload and
   stores it.
2. **New transport**: the server includes an `Mcp-Session-Id` header on
   the initial GET response. `open_sse_stream` captures this header and
   returns it to the reader.

When a session id is known, the poster task adds
`Mcp-Session-Id: <id>` to every POST request. When no session id is
known (the server never sends one), the header is omitted — some
servers reject unknown headers, so this is backward-compatible.

### Last-Event-ID

The SSE reader now parses `id:` lines (SSE spec) and stores the last
seen event id. On reconnect, `open_sse_stream` sends
`Last-Event-ID: <id>` as a request header so the server can replay
missed events. On the first connect (no prior events), the header is
omitted.

### Reconnect with backoff

When the SSE stream drops (network blip, server restart), the reader
reconnects with the session id + last event id. The backoff schedule is
`[1s, 2s, 5s, 10s, 30s]` with a max of 5 retries. The backoff resets to
0 after a successful connect, so a long-lived session that occasionally
drops reconnects quickly. If all retries are exhausted, the reader
fails all pending requests and marks the transport as dead.

## Consequences

Positive:

- Stateful MCP servers that require a session id now work correctly.
- Dropped SSE connections resume from the last event id instead of
  losing in-flight messages.
- Backward-compatible: servers that do not send a session id or event
  ids are unaffected (the headers are omitted).

Negative:

- The reconnect loop adds latency to a dropped connection (up to 30s
  on the final retry). This is the right trade-off for reliability over
  a fast-fail, but an operator who wants fast-fail can tune the
  backoff schedule in a future WO.
- The SSE parser was rewritten from a `data:`-only parser to a
  full `field: value` parser (handling `event:`, `data:`, `id:`, and
  comments). This is a larger change than the old parser but is
  necessary to capture `id:` and `event:` fields.

## Tests

- `parse_session_id_from_url_extracts_query_param` — extracts
  `session_id` from a URL query string.
- `parse_session_id_from_url_extracts_camel_case_param` — extracts
  `sessionId` (camelCase variant).
- `parse_session_id_from_url_returns_none_without_param` — returns
  `None` when the URL has no session_id param.
- `post_request_sends_session_id_header_when_provided` — a mock HTTP
  server captures the `Mcp-Session-Id` header on POST.
- `post_request_omits_session_id_header_when_none` — the header is
  omitted when no session id is known.
- `open_sse_stream_sends_last_event_id_header_on_reconnect` — a mock
  SSE server captures the `Last-Event-ID` header on GET.
- `open_sse_stream_omits_last_event_id_header_on_first_connect` — the
  header is omitted on the first connect.
- `open_sse_stream_captures_session_id_from_response_header` — the
  `Mcp-Session-Id` header is captured from the GET response.
- `open_sse_stream_returns_none_session_id_when_absent` — no session
  id when the header is absent.

## Future work

- Make the backoff schedule and max-retry count configurable via
  `McpServerConfig` (currently hardcoded).
- Add a `DELETE` request on graceful disconnect (the spec says clients
  SHOULD send `DELETE` with the session id to terminate the session).
- Migrate from the old HTTP+SSE transport (separate `/sse` and
  `/messages` endpoints) to the new single-endpoint streamable-HTTP
  transport. The current code supports both session-id sources, so the
  migration is a transport-layer change, not a session-id change.