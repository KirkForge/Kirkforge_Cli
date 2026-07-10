# ADR-0009: Hook surface — PostToolUse, UserPromptSubmit, PreCompact

- **Status:** Accepted
- **Date:** 2026-06-24

## Context

Plugin3 fires on host-side hooks, not on user-pasted content
(that's Stratum). The hook surface is the boundary between
Plugin3 and the host agent.

Three hooks carry Plugin3's logic:

1. **PostToolUse** — fires after every tool result. Plugin3
   slices the result via the orchestrator (ADR-0007) and
   emits the modified result back to the host.
2. **UserPromptSubmit** — fires before the user's prompt is
   sent to the model. Plugin3 runs the budget guard
   (ADR-0005) and either allows, warns, slices, or suggests
   compaction.
3. **PreCompact** — fires before the host compacts the
   conversation. Plugin3 emits a `CompactHint` (ADR-0008) so
   the host's own compactor has context.

Plugin3 does *not* register a `SessionStart` or `Subagent`
hook — those are Plugin1's territory. The three plugins are
composable: a user with all three installed sees one hook
table in `~/.claude/settings.json` (or equivalent).

## Decision

### Hook registry

```rust
// crates/plugin3-cli/src/hooks/mod.rs

pub fn register_hooks(host: Host) -> HookConfig {
    match host {
        Host::ClaudeCode => HookConfig {
            post_tool_use: Some(vec![CommandHook {
                kind: "command",
                command: "plugin3 hook post-tool-use".into(),
                timeout: 5,
            }]),
            user_prompt_submit: Some(vec![CommandHook {
                kind: "command",
                command: "plugin3 hook user-prompt-submit".into(),
                timeout: 2,
            }]),
            pre_compact: Some(vec![CommandHook {
                kind: "command",
                command: "plugin3 hook pre-compact".into(),
                timeout: 10,
            }]),
        },
        // ponytail: Cursor/Aider slots stay empty (HookConfig::default())
        // — their host settings formats are not yet wired. A future ADR
        // adds one arm at a time. Drift test
        // `register_hooks_cursor_and_aider_return_empty_config` pins
        // the empty-config behaviour so a cross-host leak surfaces
        // here.
        _ => HookConfig::default(),
    }
}

pub struct HookConfig {
    pub post_tool_use: Option<Vec<CommandHook>>,
    pub user_prompt_submit: Option<Vec<CommandHook>>,
    pub pre_compact: Option<Vec<CommandHook>>,
}

pub struct CommandHook {
    /// Discriminator for the JSON wire shape (Claude Code expects
    /// `"type": "command"`). serde-renamed so the Rust field stays
    /// `kind` while the wire field reads `type`.
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub command: String,
    pub timeout: u64,
}
```

ponytail: the earlier draft used `Option<CommandHook>`
(singular) and named the timeout field `timeout_seconds`.
The MVP uses `Option<Vec<CommandHook>>` to match Claude
Code's actual schema (which is an array of hook entries —
some hosts run multiple commands per hook) and uses
`timeout` because Claude Code's settings.json reads
`"timeout": 5` (no `_seconds` suffix). The drift test
`register_hooks_claude_code_matches_adr_shape` pins the
JSON field names so a contributor who renames `command` to
`cmd` or `timeout` to `timeout_seconds` fails CI before
a user copies the JSON into `~/.claude/settings.json` and
wonders why Claude Code ignores the hook.

The registry is built once at install time
(`plugin3 init`); the resulting JSON is written to the host's
settings file.

### PostToolUse flow

```rust
// crates/plugin3-cli/src/hooks/mod.rs

pub(crate) fn post_tool_use() {
    let _host = current_host();
    let Some(payload) = read_stdin_json::<PostToolUsePayload>() else {
        let resp = PostToolUseResponse {
            content: String::new(),
            note: Some("plugin3: stdin parse failed; passing through".into()),
        };
        println!("{}", serde_json::to_string(&resp).unwrap());
        return;
    };
    let bytes_in = payload.content.len();
    let store = open_store();
    let slicer = HeadTailSlicer::default();
    let orch = SlicingOrchestrator {
        store: store.as_ref(),
        slicer: &slicer,
        detector: DetectorCache::new(),
    };
    let result = run_orchestrator(&orch, &[(
        payload.tool_result_key.clone(),
        payload.content.clone(),
        Some(payload.tool_name.clone()),
    )]);
    let (_key, decision) = result.decisions.into_iter().next()
        .expect("orchestrator returns one decision per input");
    // ponytail: the orchestrator's DetectorCache already detected
    // the kind for the Slice/Keep decision and now surfaces it on
    // the decision itself (ADR-0007 § Orchestrator API). Reading
    // it here avoids a second `detector::detect(...)` call on the
    // PostToolUse hot path.
    let (content, note, bytes_out, recent_key, sliced) = match decision {
        SliceDecision::Keep { bytes, .. } => (
            payload.content, None, bytes,
            if payload.tool_result_key.is_empty() {
                "passthrough".to_string()
            } else { payload.tool_result_key },
            false,
        ),
        SliceDecision::Sliced { kind, marker, head, tail, bytes_kept, .. } => {
            let note = Some(format!("sliced {kind:?} ({bytes_kept} bytes kept)"));
            let content = format!("{head}{marker}{tail}");
            (content, note, bytes_kept, marker, true)
        }
    };
    append_recent(&recent_key, bytes_in);
    // ponytail: emit a Slice record only when an actual slice
    // happened. Keep decisions have `bytes_in == bytes_out`, so
    // the aggregator's `saturating_sub` already contributed 0 to
    // `bytes_saved` — but emitting a record unconditionally would
    // inflate `records` and the `plugin3 report --kind slice` count
    // (every PostToolUse would count as a slice event). The
    // orchestrator invariant
    // (`total_bytes_saved_sums_only_sliced_offloaded`) treats Keep
    // rows as no-ops; the CLI matches by gating the record itself.
    if sliced {
        emit_usage(&UsageRecord {
            kind: UsageKind::Slice,
            session_id: payload.session_id.clone(),
            bytes_in: Some(bytes_in),
            bytes_out: Some(bytes_out),
            tool: Some(payload.tool_name),
            ..empty_record()
        });
    }
    let resp = PostToolUseResponse { content, note };
    println!("{}", serde_json::to_string(&resp).unwrap());
}
```

### UserPromptSubmit flow

```rust
// crates/plugin3-cli/src/hooks/mod.rs

pub(crate) fn user_prompt_submit() {
    let Some(payload) = read_stdin_json::<UserPromptSubmitPayload>() else {
        println!("{}", serde_json::to_string(&UserPromptSubmitResponse::Allow).unwrap());
        return;
    };
    let mut b = super::load_budget();
    let recent = super::load_recent_outputs();
    let incoming = estimate_tokens(&payload.prompt);
    b.record(incoming);
    let intervention = decide(&b, incoming, &recent);
    if let Some(kind) = classify_kind(&intervention) {
        emit_usage(&UsageRecord {
            kind,
            session_id: payload.session_id.clone(),
            tokens_used: Some(b.used),
            tokens_ceiling: Some(b.ceiling),
            ..empty_record()
        });
    }
    super::save_budget(&b);
    let resp = match intervention {
        Intervention::Allow => UserPromptSubmitResponse::Allow,
        Intervention::Warn { remaining } => UserPromptSubmitResponse::Warn { remaining },
        Intervention::Slice { target_key, slice_to } =>
            UserPromptSubmitResponse::Slice { target_key, slice_to },
        Intervention::Compact { reason } => UserPromptSubmitResponse::Compact { reason },
    };
    println!("{}", serde_json::to_string(&resp).unwrap());
}
```

### PreCompact flow

```rust
// crates/plugin3-cli/src/hooks/mod.rs

pub(crate) fn pre_compact() {
    let Some(payload) = read_stdin_json::<PreCompactPayload>() else {
        let resp = json!({ "hint": null, "summary": "" });
        println!("{}", serde_json::to_string(&resp).unwrap());
        return;
    };
    let b = super::load_budget();
    let turns: Vec<Turn> = payload.history_turns.into_iter().map(|t| Turn {
        index: t.index, role: t.role, content_preview: t.content_preview,
    }).collect();
    let hint = compaction::build_hint(&b, &turns);
    let compactor = LocalSummaryCompactor::default();
    let summary_text = {
        let joined = turns.iter()
            .map(|t| format!("[{}] {}: {}", t.index, t.role, t.content_preview))
            .collect::<Vec<_>>().join("\n");
        compactor.apply(&joined).map(|o| o.summary).unwrap_or_default()
    };
    let resp = json!({
        "hint": hint,
        "summary": summary_text,
    });
    emit_compact_hint(&b);
    println!("{}", serde_json::to_string(&resp).unwrap());
}
```

### Timeout discipline

Each hook has a hard timeout (default 5 s for PostToolUse,
2 s for UserPromptSubmit, 10 s for PreCompact). The host
itself enforces the timeout via the `timeout` field in the
settings.json entry — Plugin3 does not run a synchronous
wall-clock guard inside the hook handler. A hook that
exceeds the host's timeout is killed by the host; the next
plugin invocation starts fresh.

ponytail: the earlier draft specified a synchronous
`std::time::Instant` + thread guard with a `tracing::warn!`
event on timeout. The MVP does **not** depend on `tracing`
(ADR-0017 § Workspace Cargo.toml), and the host's settings
schema already carries the timeout per hook entry — a
plugin-internal wall-clock guard would only fire if the
plugin itself hung, which a healthy hook implementation
does not do (the cost reporter is `O(1)`, the orchestrator
is `O(n_outputs)`, the budget guard is `O(recent)`). The
drift test
`hooks_mod_drift::adr_0009_timeout_section_omits_tracing_warn`
pins the absence of the `tracing::warn!` event so a
contributor who re-pastes the older timeout-guard example
documents a design the impl does not ship.

The plugin never blocks the host for more than the host's
configured timeout.

### Concurrency

The PostToolUse hook runs serially per tool result. The
orchestrator (ADR-0007) parallelises within a single tool
output's slice operations, but the hook itself does not
spawn concurrent hook invocations — the host serialises them.

### Error contract

A hook handler that returns an error (panic, unhandled
exception) does *not* crash the host. The hook returns a
passthrough response and emits one `eprintln!` line tagged
`plugin3:` to the host's stderr:

```rust
match read_stdin_json::<Payload>() {
    Some(payload) => handler(payload),
    None => {
        eprintln!("plugin3: {} stdin parse failed; passing through", hook_name);
        passthrough_response()
    }
}
```

ponytail: the earlier draft specified a `tracing::error!`
event for handler failures. The MVP does **not** depend on
`tracing` (ADR-0017 § Workspace Cargo.toml). The hook
handlers today are infallible — `read_stdin_json` returns
`None` on parse failure and the handler short-circuits to a
passthrough. A user inspecting a misbehaving hook runs
`plugin3 hook <kind>` directly (the CLI entry point) and
sees the `eprintln!` on stderr; the cost reporter does not
record hook-failure events (ADR-0010 § Significant events
pins the kinds the reporter emits).

The `TransformError` enum (Stratum ADR-0011) applies — three
variants, no panics.

## Consequences

Negative first:

- Three hooks is more than Stratum's two. A user installing
  Plugin3 alone sees three new entries in their `settings.json`.
- The 5 s timeout is tight. A slow store backend (network
  filesystem, slow disk) could trip it. The FileOffloadStore
  is the fastest backend; SQLite is slower but bounded.
- Plugin3 reads token counts from the budget state file; the
  host does not push them. A bug in the budget state file
  could cause the guard to misjudge. The drift test pins
  the format.

Positive:

- The three hooks cover the load-bearing paths. PostToolUse
  slices noisy output; UserPromptSubmit guards the budget;
  PreCompact suggests compaction when the budget is breached.
- Hook handlers are pure functions of their payload + state;
  easy to test.
- The hook registry is host-agnostic; adding a new host is
  one new module in `plugin3-hosts/`.

## Implementation notes

The hook handlers live at `crates/plugin3-cli/src/hooks/`.
Each handler is a function that takes a typed payload and
returns a typed response. The host shim (ADR-0013) translates
the host's payload format to the typed payload.

The `register_hooks` function emits JSON compatible with the
host's settings schema. For Claude Code:

```json
{
  "hooks": {
    "PostToolUse": [
      {
        "hooks": [
          { "type": "command", "command": "plugin3 hook post-tool-use", "timeout": 5 }
        ]
      }
    ],
    "UserPromptSubmit": [
      {
        "hooks": [
          { "type": "command", "command": "plugin3 hook user-prompt-submit", "timeout": 2 }
        ]
      }
    ],
    "PreCompact": [
      {
        "hooks": [
          { "type": "command", "command": "plugin3 hook pre-compact", "timeout": 10 }
        ]
      }
    ]
  }
}
```

ponytail: the earlier draft showed `"matcher": "*"` on
the `PostToolUse` entry. The MVP's serde-emitted JSON
omits the `matcher` field — Claude Code treats its
absence as "match all", which is the behaviour the MVP
needs (no host-side filtering of tool names). Adding
`matcher` is a future ADR if the host ever needs to
filter on tool name (e.g. only slice `Bash` outputs);
that change is a one-line addition to the Claude Code
arm of `register_hooks`. The drift test
`register_hooks_claude_code_matches_adr_shape` pins the
absence of the `matcher` key.

The `plugin3 init` subcommand writes this to the host's
settings file (Claude Code: `~/.claude/settings.json`). Cursor
and Aider slots return `HookConfig::default()` today — their
host settings formats are not yet wired (see § Hook registry).
Adding one is a future ADR per-host with the exact settings
file path and JSON shape.