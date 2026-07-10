# ADR-0013: Output shim — per-host payload translation

- **Status:** Accepted
- **Date:** 2026-06-24

## Context

Plugin3 speaks the canonical payload schema defined in
ADR-0009 (`PostToolUsePayload`, `UserPromptSubmitPayload`,
`PreCompactPayload`, and their responses). The host agent
speaks its own schema: Claude Code uses one JSON envelope,
Cursor uses another, Aider uses environment variables.

The shim is the boundary. Each host gets one module in
`plugin3-hosts/` that:

1. Parses the host's payload format into the canonical
   payload.
2. Calls the canonical handler.
3. Translates the canonical response back to the host's
   format.

The MVP does **not** wire this boundary. The CLI's hook
handlers consume the canonical payload types directly and
run the canonical logic themselves. The shim layer is
reserved for a future ADR when a second host is supported.

## Decision

### Host enum

```rust
// crates/plugin3-hosts/src/lib.rs

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Host {
    ClaudeCode,
    Cursor,
    Aider,
    KirkForge,
}

pub trait EnvSource {
    fn is_set(&self, key: &str) -> bool;
}

struct OsEnv;
impl EnvSource for OsEnv {
    fn is_set(&self, key: &str) -> bool { std::env::var(key).is_ok() }
}

pub fn detect_host() -> Host {
    // ponytail: production entry point — wraps the pure
    // function below so the host shim layer reads
    // `std::env::var` exactly once per call. The trait-
    // parameterised `detect_host_with` is the seam used by
    // drift tests (ADR-0013 § drift tests) so they don't
    // race with parallel tests that mutate the process env.
    detect_host_with(&OsEnv)
}

pub fn detect_host_with(env: &dyn EnvSource) -> Host {
    // ponytail: only Claude Code has real CLI hook handlers.
    // The env-var check exists so future Cursor/Aider/KirkForge
    // detection slots are obvious. Precedence: CLAUDE_CODE >
    // CURSOR_TRACE_ID > AIDER > KIRKFORGE_PLUGIN3 > ClaudeCode.
    if env.is_set("CLAUDE_CODE") {
        Host::ClaudeCode
    } else if env.is_set("CURSOR_TRACE_ID") {
        Host::Cursor
    } else if env.is_set("AIDER") {
        Host::Aider
    } else if env.is_set("KIRKFORGE_PLUGIN3") {
        Host::KirkForge
    } else {
        Host::ClaudeCode // ponytail: default to the only host
                         // with wired hook handlers today.
    }
}
```

### Canonical payloads

```rust
// crates/plugin3-hosts/src/canonical.rs

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct PostToolUsePayload {
    pub tool_name: String,
    #[serde(default)]
    pub tool_result_key: String,
    pub content: String,
    // ponytail: session_id is load-bearing for ADR-0010's
    // usage.jsonl grouping. Hosts that don't tag sessions
    // emit default-empty rather than breaking the cost reporter.
    #[serde(default)]
    pub session_id: String,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct PostToolUseResponse {
    /// Modified tool result content. The host replaces its
    /// in-memory tool result with this string.
    pub content: String,
    /// Optional human-readable note for the user.
    pub note: Option<String>,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct UserPromptSubmitPayload {
    pub prompt: String,
    #[serde(default)]
    pub session_id: String,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum UserPromptSubmitResponse {
    Allow,
    Warn { remaining: usize },
    Slice { target_key: String, slice_to: usize },
    Compact { reason: String },
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct PreCompactPayload {
    pub history_turns: Vec<Turn>,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct Turn {
    pub index: usize,
    pub role: String,            // "user" | "assistant" | "tool"
    pub content_preview: String, // first 200 chars
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct PreCompactResponse {
    // ponytail: hint is `serde_json::Value` rather than the
    // typed `CompactHint` so a future host shim can emit any
    // shape the host consumes (turns count, summary text,
    // CompactHint). The CLI side builds a typed CompactHint
    // (ADR-0008) and serialises a thin `{ "turns": N }`
    // envelope that the host can interpret without depending
    // on plugin3-core types.
    pub hint: serde_json::Value,
}
```

### Shim entry point

ponytail: the earlier draft prescribed a single dispatch entry
`emit_to(host, event, payload)` mirroring Stratum ADR-0009.
That entry point was removed (B3) because it had become a
dead passthrough: the CLI hook handlers consume the canonical
payloads directly and run the canonical logic themselves. A
future per-host translation layer will be re-introduced when a
second host is supported, with the dispatch wired from
`plugin3-cli::hooks` rather than as a library-only stub.

### Claude Code shim

ponytail: the Claude Code shim is a stub today. The real
Claude Code payload handling lives in
`crates/plugin3-cli/src/hooks/mod.rs`, which:

- parses the host envelope into `PostToolUsePayload` /
  `UserPromptSubmitPayload` via `read_stdin_json`,
- runs the canonical logic (`SlicingOrchestrator`,
  `TokenBudget::decide`, `compaction::build_hint`),
- serialises the canonical response back to the host wire
  shape.

When a future ADR extracts per-host translation into this
crate, the Claude Code module will grow `handle_post_tool_use`,
`handle_user_prompt_submit`, and `handle_pre_compact` functions
that wrap the canonical types and forward to the core logic.

### Cursor shim

ponytail: the Cursor shim is a stub today — the file
`crates/plugin3-hosts/src/cursor.rs` exists with a
`stub_present` test but no real handler. The MVP does not
route Cursor anywhere because the dispatch entry point was
removed (B3); a future ADR will wire the stub.

When a user reports a need, the stub graduates to a real
shim using the translation sketched below:

```rust
// crates/plugin3-hosts/src/cursor.rs (future)

pub fn handle_post_tool_use(payload: Value) -> Value {
    // Cursor's PostToolUse payload has the tool result under
    // a different field name. Translate.
    let tool_name = payload["tool_name"].as_str().unwrap_or("unknown").to_string();
    let content = payload["result"]["content"].as_str().unwrap_or("").to_string();
    let tool_result_key = payload["result"]["id"].as_str().unwrap_or("").to_string();
    let canonical = PostToolUsePayload {
        tool_name,
        tool_result_key,
        content,
        session_id: String::new(),
    };
    let response = crate::canonical::PostToolUseResponse {
        content: canonical.content,
        note: None,
    };
    // Cursor expects the response in a `patch` field.
    serde_json::json!({
        "patch": {
            "content": response.content,
        },
        "note": response.note,
    })
}
```

### Aider shim

ponytail: the Aider shim is a stub today for the same
reason as Cursor — the file
`crates/plugin3-hosts/src/aider.rs` exists with a
`stub_present` test but no real handler. Aider uses
environment variables, not JSON envelopes, so the shim
will be different from Claude Code's. The MVP leaves it
unwired; a future ADR will route here.

### KirkForge shim

ponytail: the KirkForge shim is a stub today for the same
reason as Cursor and Aider — the file
`crates/plugin3-hosts/src/kirkforge.rs` exists with a
`stub_present` test but no real handler. KirkForge-Cli is the
sibling host in the same plugin ecosystem; its hook model is
assumed to emit the same canonical events as Claude Code,
but the exact envelope shape and env-var detection are not yet
specified. The MVP leaves it unwired; a future ADR will route
here once the KirkForge hook contract is written.

The sketched future shape:

```rust
// crates/plugin3-hosts/src/kirkforge.rs (future)

pub fn handle_post_tool_use(payload: Value) -> Value {
    // KirkForge-Cli is expected to pipe the same canonical
    // payloads as Claude Code, but may wrap them under a
    // different top-level key. Translate.
    let canonical: PostToolUsePayload = serde_json::from_value(payload)
        .expect("kirkforge PostToolUse payload");
    let response = crate::canonical::PostToolUseResponse {
        content: canonical.content,
        note: None,
    };
    serde_json::json!({
        "content": response.content,
        "note": response.note,
    })
}
```

```rust
// crates/plugin3-hosts/src/aider.rs (future)

pub fn handle_post_tool_use(payload: Value) -> Value {
    // Aider pipes tool results via stdin; the shim reads
    // from stdin directly. The `payload` is the parsed
    // JSON; the response is written to stdout as a JSON
    // patch.
    let canonical: PostToolUsePayload = serde_json::from_value(payload)
        .expect("aider PostToolUse payload");
    let response = crate::canonical::PostToolUseResponse {
        content: canonical.content,
        note: None,
    };
    serde_json::json!({
        "content": response.content,
        "note": response.note,
    })
}
```

### Drift tests

Drift tests pin the host enum and canonical payload shapes.
The Host enum tests live in
`crates/plugin3-hosts/src/lib.rs::tests` and assert:

- the variants serialize to kebab-case,
- `UserPromptSubmitResponse` keeps its four tagged-enum
  variants with the load-bearing field names
  (`remaining`, `target_key`, `slice_to`, `reason`).

The `detect_host_with` precedence chain and canonical env-var
names are pinned in the same module using an `EnvSource`
trait seam so tests do not race on `std::env::var` mutation.

The Cursor, Aider, KirkForge, and Claude Code shim files each
contain a `stub_present` test asserting the module exists and is wired.
When a stub graduates to a real shim, its drift test moves
into a `drift_tests` module alongside the canonical wire-shape
tests.

## Consequences

Negative first:

- Three shim modules is more than one. The trade is per-host
  payload differences will be isolated to the shim layer once
  it is wired; today the CLI handlers carry that responsibility.
- A new host is a non-trivial addition: enum variant,
  detector function, shim module, hook-handler wiring, drift
  tests. The README documents the steps.

Positive:

- The canonical payload schema is documented in code. A
  contributor adding a new shim has a clear contract.
- Drift tests catch canonical-shape regressions: a contributor
  who changes a payload field name fails CI.
- The canonical types live in `plugin3-hosts` so the CLI and
  any future shim share the same definitions without
  `plugin3-core` knowing about host envelopes.

## Implementation notes

The canonical payload definitions live at
`crates/plugin3-hosts/src/canonical.rs` and are re-exported
from the crate root. Host detection lives in
`crates/plugin3-hosts/src/lib.rs`.

The host detection is a one-time cost at CLI startup. The
detected host is cached in the plugin's state file
(ADR-0014) so subsequent hook invocations skip detection.

The shim module files (`claude_code.rs`, `cursor.rs`,
`aider.rs`, `kirkforge.rs`) are stubs today. The real per-host
translation happens in `plugin3-cli::hooks` until a future ADR
extracts it. Removing the dead `emit_to` library path (B3)
prevents a stub from silently diverging from the actual hook
handler behaviour.
