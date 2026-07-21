# ADR-030: `--seed` deterministic mode

**Status:** Accepted (2026-07-21)

## Context

ChatGPT's cross-review named "deterministic execution mode" as one of the
four "things most open-source agents lack." `grep -rn 'seed.*42|deterministic.*mode|reproducible' src/` → only test-level determinism. No `--seed` flag for reproducible planning.

Without deterministic mode, regression testing is anecdotal: the same prompt
on the same repo produces different tool-call sequences on every run because
model temperature and `tokio::spawn` scheduling both introduce nondeterminism.

## Decision

Add a `--seed <u64>` CLI flag that:

1. **Pins model temperature to 0** for all providers (Anthropic, OpenAI-compat,
   Ollama). Anthropic does not accept a `seed` field, so temperature=0 is the
   closest approximation. OpenAI-compat servers get `temperature: 0` + `seed: <N>`.
   Ollama gets `options: { temperature: 0, seed: <N> }`.

2. **Forces sequential tool dispatch** — when `--seed` is set, the parallel
   batch in `dispatch_tool_call_batch` skips `tokio::spawn` and runs all
   non-file tools sequentially. This eliminates nondeterminism from task
   scheduling while preserving the prepare/run/record split.

3. **Is best-effort** — model providers do not guarantee identical outputs
   even with seed=42. The tool-call *sequence* is reproducible enough for
   regression testing; the model's *content* may still vary.

## Implementation

- `src/cli.rs`: `#[arg(long)] seed: Option<u64>` on the `Run` variant.
- `src/shared/mod.rs`: `pub seed: Option<u64>` on `Config`, default `None`.
- `src/adapters/mod.rs`: `fn set_seed(&mut self, Option<u64>)` on `ModelAdapter`
  trait (default no-op).
- Each adapter stores `seed: Option<u64>` and passes it to the body builder.
- `src/session/executor/mod.rs`: calls `adapter.set_seed(cfg.seed)` at
  construction, next to the existing `set_json_mode` call.
- `src/session/executor/turn.rs`: `is_deterministic()` helper checks
  `config.seed.is_some()`; when true, Phase 2 of `dispatch_tool_call_batch`
  runs sequentially instead of spawning.

## Consequences

**Positive:**
- Same prompt + same seed + same repo → same tool-call sequence.
- Enables the task-benchmark harness (P1-long-2) to use `--seed 42` for
  reproducible measurements.
- Regression tests can assert tool-call sequences, not just outcomes.

**Negative:**
- Sequential dispatch is slower for multi-tool turns (no parallelism).
- Model providers don't guarantee identical outputs — the seed is a hint,
  not a contract.
- Anthropic's API doesn't accept a `seed` field; temperature=0 is the only
  lever.

**Neutral:**
- The flag is a runtime override, not persisted to config.toml.
- When unset (the default), behavior is unchanged — parallel dispatch,
  provider-default temperature.
