# ADR-0028: Unify the Rust and TypeScript verifier buses

<!-- adr-predicates
status: accepted
implemented: true
supersedes: []
affects-crates: []
-->

- **Status:** Accepted
- **Date:** 2026-07-20

## Amendment (2026-07-31)

The Status was promoted from `Accepted (partially implemented)` to
`Accepted` in WO 9.6 after confirming the plugin-verifier bridge is
code-complete: `register_plugin_verifiers_into_bus` wires any
`Capability::Verifier` into the unified `VerifierBus`, and the
executor's `emit_tool_event_and_correct` converts each
`Severity::Error` `VerdictEntry` into a `CorrectionResult` — the same
struct the correction loop emits — so a single correction path handles
built-in and plugin verdicts. The end-to-end path is proven by the
`plugin_verifier_triggers_correction_result` integration test. The
cross-language NDJSON wire bridge (Rust ↔ TS orchestrator over stdio)
shipped subsequently in WO 10.8 (`TsOrchestratorBridgeVerifier` in
`bus.rs`). See the `## ponytail` section below for the full
implementation record.

## Context

KirkForge has two verifier systems that overlap rather than cooperate:

1. **Rust runtime verifier bus** — `src/session/verifier/` with priority slots, a correction loop, and built-in security / lint / git / rustfmt / plugin verifiers. It is event-driven via `EventBus` and produces `Verdict::{Clean,Fixable,Unfixable,Skipped}`.
2. **TypeScript plugin orchestrator verifier bus** — `npm/kf-plugin/packages/orchestrator/src/` with emitters (`SecurityEmitter`, `GraphEmitter`, language-specific lint/type/import engines) that write KirkForge events to a shared event bus, a `truth-model.ts` final verdict, and an LLM-prompt-based correction loop.

Both detect security issues, lint problems, and structural graph changes. Neither can see the other. A Rust-only session cannot benefit from the TS graph/import analysis; a TS-only plugin session cannot benefit from the Rust in-process clippy/rustfmt/security verifiers. This is the "merge" seam identified in ADR-007 and the workorder.

This ADR records the shared contract and migration path. It is intentionally design-first: no bridge code ships until the contract is stable.

## Decision

Introduce a single **KirkForge Verification Bus (KVB)** contract that both the Rust runtime and the TS orchestrator implement. The contract has three layers:

1. **Shared event schema** — canonical event kinds and payloads.
2. **Shared verifier slot registry** — slot names, priorities, required/advisory policy.
3. **Shared truth model** — final verdict computation and correction decision.

### 1. Shared event schema

The KVB schema is a superset of both current vocabularies. Events are JSON objects with `kind`, `task_id`, `timestamp`, and a typed `payload`.

```jsonc
{
  "kind": "verify.security",
  "task_id": "t-uuid",
  "timestamp": "2026-07-20T00:00:00Z",
  "payload": {
    "status": "fail",
    "findings": [
      {
        "file": "src/main.rs",
        "line": 42,
        "rule": "dangerous-shell-pattern",
        "severity": "critical",
        "message": "unchecked user input passed to sh -c"
      }
    ],
    "duration_ms": 12
  }
}
```

Canonical event kinds:

| Kind | Emitter | Purpose |
|------|---------|---------|
| `tool.file_read` | Rust | A file was read. |
| `tool.file_write` | Rust/TS | A file was written or overwritten. |
| `tool.edit` | Rust/TS | An edit_file result. |
| `tool.bash_exec` | Rust | Bash command executed. |
| `tool.git_op` | Rust | Git operation executed. |
| `verify.security` | Rust/TS | Security scan result. |
| `verify.lint` | Rust/TS | Lint scan result. |
| `verify.types` | TS | Type-check result. |
| `verify.imports` | TS | Import hygiene result. |
| `state.graph` | TS | Import graph / broken edges / cycles. |
| `state.changes` | TS/Rust | Git diff summary of written files. |
| `artifact.emitted` | TS | File emitted by an agent with hash/size metadata. |
| `artifact.blocked` | TS/Rust | Protocol-integrity block (e.g., unterminated artifact). |

The existing Rust `EventKind` and TS `KirkForgeEvent` kinds are mapped to these canonical kinds at the bridge boundary. New kinds require an ADR amendment.

### 2. Shared verifier slot registry

Both sides expose the same five verifier slots with the same priority and default policy:

| Slot | Priority | Policy | Rust impl | TS impl |
|------|----------|--------|-----------|---------|
| `security` | 1 | required | `security.rs` | `SecurityEmitter` |
| `lint` | 2 | required | `lint.rs` (Rust) | language lint engines |
| `types` | 3 | required-advisory* | none yet | `TscEmitter` / `PyrightEmitter` |
| `graph` | 4 | required | none yet | `GraphEmitter` |
| `imports` | 5 | advisory | none yet | import lint engine |

*Type checks are required when a language-specific `checkCommand` is configured, otherwise advisory.

The registry is described by a JSON/TOML manifest:

```toml
[[verifier_slot]]
name = "security"
priority = 1
policy = "required"
rust = "builtin"
ts = "SecurityEmitter"
```

### 3. Shared truth model

Both sides implement the same precedence table from TS `truth-model.ts`, generalized:

1. Protocol-integrity break (`artifact.blocked`) → `fail`
2. External task validator result (pass/fail) → overrides everything
3. Required verifier slot fail → `fail`
4. Advisory verifier slot fail → `warn` (does not block)
5. All slots pass or skipped → `pass`
6. No signal → `unknown`

Final verdict shape:

```jsonc
{
  "final_verdict": "pass" | "fail" | "error" | "unknown",
  "source_of_truth": "task-validator" | "verifier" | "protocol",
  "reason": "string",
  "slot_verdicts": { "security": "fail", "lint": "pass", ... }
}
```

### 4. Shared correction contract

A `FixSuggestion` is the common fix representation:

```jsonc
{
  "description": "remove unused import",
  "file": "src/lib.rs",
  "line": 5,
  "original": "use std::collections::HashMap;\n",
  "replacement": "",
  "severity": "warning",
  "command": null
}
```

- If `command` is set and `original`/`replacement` are empty, the consumer runs the command in-place.
- If `original`/`replacement` are set, the consumer applies a text patch.
- If neither is set, the fix is model-facing only; both correction loops may append it to the next prompt.

### 5. Bridge architecture

The bridge is a thin adapter in each host:

- **Rust bridge** — a new `kf-plugin-host` verifier adapter (or a small crate) that, when a TS orchestrator is configured as an MCP server, forwards Rust `BusEvent`s as KVB events over stdio and receives KVB events back.
- **TS bridge** — a new package `@kirkforge/verifier-bridge` that receives KVB events from the Rust runtime (when invoked as a subprocess/MCP server) and emits them into the orchestrator's event bus.

The wire format is NDJSON lines of KVB events. Both sides must ignore unknown event kinds (forward compatibility).

## Consequences

- Rust sessions can request graph/import analysis from the TS orchestrator without duplicating the implementation.
- TS plugin sessions can reuse Rust in-process verifiers (clippy, rustfmt, secret scanning) without spawning equivalent tooling.
- A single truth model and correction contract reduces divergence between hosts.
- The shared schema becomes a public compatibility surface: changes need ADR amendments and version bumps.

## ponytail

- Implemented. The Rust-side plugin verifier bridge shipped
  (Workorder 7.7, completed by Workorder 9.6): plugin-declared
  `Capability::Verifier` entries register into the unified
  `VerifierBus` (ADR-043) via `VerifierBus::add_plugin_verifier` and
  `register_plugin_verifiers_into_bus`. The bus runs each plugin verifier
  through the host crate's env-cleared `PluginVerifier` subprocess (exit 0
  = pass, non-zero = fail with stderr as the message) and tags results
  `VerifierSource::Plugin(name)`. Error verdicts are injected into the
  conversation as tool results by the executor's
  `emit_tool_event_and_correct`, which converts each `Severity::Error`
  `VerdictEntry` into a `CorrectionResult` — the same struct the
  correction loop emits — so a single correction path handles built-in
  and plugin verdicts. Live plugin reload rebuilds the plugin-verifier
  set on the bus while keeping built-in verifiers. The integration test
  `plugin_verifier_triggers_correction_result` in
  `src/session/executor/tests/mod.rs` proves the end-to-end path: a mock
  plugin declaring a `security` verifier → `VerifierBus` →
  `CorrectionResult`.
- The cross-language NDJSON bridge (Rust ↔ TS orchestrator over stdio)
  shipped in WO 10.8. The Rust `TsOrchestratorBridgeVerifier`
  (`src/session/verifier/bus.rs`) implements `BusVerifier` by shelling
  out to the TS orchestrator's bridge emitter
   (`npm/kf-plugin/packages/orchestrator/src/bridge-emitter.ts`)
  and parsing NDJSON verdicts from stdout. The wire format is one JSON
  object per line:
  `{"verifier":"security","severity":"error","file":"src/foo.ts","line":42,"message":"...","rule":"no-eval"}`.
  Malformed lines become `Severity::Warning` verdicts (never silently
  dropped). The bridge is registered on the `VerifierBus` when the TS
  orchestrator plugin is loaded; error verdicts flow through the same
  `emit_tool_event_and_correct` → `CorrectionResult` path as built-in
  and plugin verifiers. The integration test
  `ts_orchestrator_bridge_verifier` proves the end-to-end path: a mock
  bridge script emits one `security` error NDJSON line →
  `VerdictEntry { Severity::Error }` → `has_errors()`. The TS-side
  bridge emitter test (`bridge-emitter.test.ts`) verifies the
  event-to-NDJSON translation. The Node SDK plugin
    (`kf-plugin`) still runs its TS-based verifiers through the
  legacy event-driven `Verifier` trait path
  (`PluginVerifierAdapter`), which is retained for backward
  compatibility.
- The Rust side currently lacks `types`, `graph`, and `imports` verifiers. The TS side currently lacks an in-process rustfmt/clippy verifier. The unified registry acknowledges these gaps rather than hiding them.

## ceiling

- The bridge adds a serialization hop. In-process Rust verifiers will remain faster than TS-originated verifiers for local Rust projects. Upgrade path: keep Rust built-ins as defaults and invoke TS verifiers only when the slot has no local implementation.
- The shared event schema is a breaking change for both event buses. Migration must be staged: first add KVB event kinds alongside existing kinds, then deprecate old kinds once both sides consume KVB.

## Amendment (2026-08-22, WO 41.3) — cross-language NDJSON bridge retired, TS tree deleted

The "ponytail" section above records that the cross-language NDJSON wire
bridge (Rust ↔ TS orchestrator over stdio) shipped in WO 10.8 via
`TsOrchestratorBridgeVerifier` shelling out to
`npm/kf-plugin/packages/orchestrator/src/bridge-emitter.ts`. That bridge is
**retired as of WO 29.2**: the 14 regex security rules now live in Rust
(`src/session/verifier/security_emitter.rs`), and
`TsOrchestratorBridgeVerifier` is a thin `BusVerifier` wrapper that calls
`security_emitter::emit_security_findings(&changed_files)` directly — no
subprocess, no NDJSON round-trip. This was the last Rust→TS call path. The
entire `npm/kf-plugin/` tree was deleted in WO 29.9, so the
`bridge-emitter.ts` file no longer exists. The Rust `VerifierBus` is
authoritative: built-in verifiers register directly, plugin verifiers
register via `register_plugin_verifiers_into_bus`, and the security scan
registers via the `TsOrchestratorBridgeVerifier` wrapper (now a misnomer —
it no longer bridges to TS).

## Amendment (2026-08-29, WO 47.14) — plugin verifiers bus-only, adapter deleted

The ponytail section's final claim ("The Node SDK plugin (`kf-plugin`)
still runs its TS-based verifiers through the legacy event-driven
`Verifier` trait path (`PluginVerifierAdapter`), which is retained for
backward compatibility") is retired: `PluginVerifierAdapter` is
**deleted** (WO 47.14). Plugin-declared verifiers register exclusively
into the `VerifierBus` via `register_plugin_verifiers_into_bus` —
`BusVerifier` is the surviving trait of the unification, and until the
deletion plugin verifiers were dual-registered and ran twice per
file-modifying tool call. The subprocess env contract changed with it:
verifier scripts now receive `KF_VERIFIER_NAME` +
`KF_CHANGED_FILES` (newline-separated); the deleted adapter's
`KF_EVENT_KIND`/`KF_EVENT_JSON` vars are gone (restoring event
visibility requires extending `VerifyContext`, tracked in WO 47.14
remaining work). Live reload re-runs the registration while keeping the
rest of the bus intact.
