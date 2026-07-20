# ADR-0028: Unify the Rust and TypeScript verifier buses

- **Status:** Accepted
- **Date:** 2026-07-20

## Context

KirkForge has two verifier systems that overlap rather than cooperate:

1. **Rust runtime verifier bus** — `src/session/verifier/` with priority slots, a correction loop, and built-in security / lint / git / rustfmt / plugin verifiers. It is event-driven via `EventBus` and produces `Verdict::{Clean,Fixable,Unfixable,Skipped}`.
2. **TypeScript plugin orchestrator verifier bus** — `npm/kirkforge-plugin/packages/orchestrator/src/` with emitters (`SecurityEmitter`, `GraphEmitter`, language-specific lint/type/import engines) that write KirkForge events to a shared event bus, a `truth-model.ts` final verdict, and an LLM-prompt-based correction loop.

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

- **Rust bridge** — a new `kirkforge-plugin-host` verifier adapter (or a small crate) that, when a TS orchestrator is configured as an MCP server, forwards Rust `BusEvent`s as KVB events over stdio and receives KVB events back.
- **TS bridge** — a new package `@kirkforge/verifier-bridge` that receives KVB events from the Rust runtime (when invoked as a subprocess/MCP server) and emits them into the orchestrator's event bus.

The wire format is NDJSON lines of KVB events. Both sides must ignore unknown event kinds (forward compatibility).

## Consequences

- Rust sessions can request graph/import analysis from the TS orchestrator without duplicating the implementation.
- TS plugin sessions can reuse Rust in-process verifiers (clippy, rustfmt, secret scanning) without spawning equivalent tooling.
- A single truth model and correction contract reduces divergence between hosts.
- The shared schema becomes a public compatibility surface: changes need ADR amendments and version bumps.

## ponytail

- This ADR is design-only. No bridge code is implemented yet; the contract must be reviewed and agreed before any bridge work starts.
- The Rust side currently lacks `types`, `graph`, and `imports` verifiers. The TS side currently lacks an in-process rustfmt/clippy verifier. The unified registry acknowledges these gaps rather than hiding them.

## ceiling

- The bridge adds a serialization hop. In-process Rust verifiers will remain faster than TS-originated verifiers for local Rust projects. Upgrade path: keep Rust built-ins as defaults and invoke TS verifiers only when the slot has no local implementation.
- The shared event schema is a breaking change for both event buses. Migration must be staged: first add KVB event kinds alongside existing kinds, then deprecate old kinds once both sides consume KVB.
