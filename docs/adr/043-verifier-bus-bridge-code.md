# ADR-043: Verifier-bus bridge code

## Status

Accepted

## Context

ADR-0028 designed a unified verifier bus (KVB) that the plugin system, LSP diagnostics, and CI gates all feed into. Currently the existing verifiers (security, lint, build, git, rustfmt, test, plugin) are separate systems — each implements the `Verifier` trait and operates on `BusEvent`s via the event bus and `VerifierHandler`. The executor can't query "what did all verifiers say about this edit?" in a structured way — it gets individual `Verdict` enum values through the correction loop, but these are the first-definitive-result-wins (truth model) rather than a complete collection of findings.

## Decision

Build `src/session/verifier/bus.rs` with:

1. **`VerifierBus`** — a struct that holds registered `BusVerifier` instances and collects all `VerdictEntry`s from every verifier (not just the first definitive one). The bus runs after file-modifying tool calls (`write_file`, `edit_file`, `apply_patch`).

2. **`BusVerifier` trait** — a sync interface (`verify(&VerifyContext) -> Vec<VerdictEntry>`) distinct from the async `Verifier` trait. `BusVerifier` receives a `VerifyContext` with the sandbox dir and changed files, not a `BusEvent`.

3. **`VerdictEntry` struct** — a structured finding with `source` (which verifier), `severity` (Info/Warning/Error), `message`, `file`, and `line`. This is distinct from the existing `Verdict` enum (`Clean`/`Fixable`/`Unfixable`/`Skipped`), which represents an aggregate result.

4. **`VerifyContext` struct** — carries `sandbox_dir` and `changed_files` for the verification run.

5. **Built-in adapters** — `SecurityBusVerifier` and `GitBusVerifier` implement `BusVerifier` by wrapping the existing async verifier functions via `tokio::task::block_in_place`. The `default_verifier_bus()` constructor registers all built-ins.

6. **Executor wiring** — the `Executor` gains a `verifier_bus: Option<Mutex<VerifierBus>>` field. After file-modifying tool calls, `emit_tool_event_and_correct` runs the bus, collects error verdicts, and injects them into the correction results so the model sees verifier feedback immediately.

## Consequences

- **Positive:** Unified verifier interface — the executor queries all verifiers in one call. Easy to add new `BusVerifier` implementations (LSP, custom) without touching the event bus. Structured `VerdictEntry` type enables richer feedback to the model.
- **Negative:** The sync `BusVerifier::verify()` wraps async verifier functions via `block_in_place`, which requires a multi-threaded runtime. The bus runs after every file-modifying tool call, adding latency. The existing event-driven `Verifier` system and the new `BusVerifier` coexist — they're not unified yet (that's a future migration step).