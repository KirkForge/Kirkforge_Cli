# ADR-070: Adapter-Gap Decisions and Deferrals

- **Status:** Accepted
- **Date:** 2026-08-08

## Context

WO 21.4 identified adapter gaps for non-Anthropic/Ollama providers and
WO 22.9 reviewed every item. Several were silently deferred — violating
AGENTS.md #11 ("no silent deferral"). This ADR documents two decisions:

1. **R7 — native provider adapters**: which providers get native adapters and
   which go through the OpenAI-compatible adapter.
2. **R8 — deferral ledger**: every deferred item, the reason, remaining work,
   and tracking ID.

## R7 — Native Provider Decision

### Decision

Use the OpenAI-compatible adapter as the default for all non-Anthropic/Ollama
models. Native adapters are added only when a provider-specific feature is
needed that the OpenAI-compatible adapter cannot express.

### Provider Support Matrix

| Provider | OpenAI-compat quality | Native adapter? | Notes |
|----------|----------------------|-----------------|-------|
| Anthropic | N/A | Yes | Native adapter; chat, vision, tool use |
| Ollama | N/A | Yes | Native adapter; local inference |
| Gemini | Partial | No | Missing grounding, caching controls |
| Azure | Good | No | OpenAI-compat surface covers most needs |
| Mistral | Good | No | Works via OpenAI-compat endpoint |
| Groq | Good | No | Works via OpenAI-compat endpoint |
| xAI | Partial | No | Missing some parameter mappings |
| Bedrock | N/A | Routed via Anthropic adapter | SigV4 auth, region config |
| Vertex | N/A | Routed via Anthropic adapter | GCP auth, project config |

### Consequences

- All providers except Anthropic and Ollama go through the OpenAI-compatible
  adapter by default.
- Provider-specific features (Gemini grounding, xAI extended parameters) are
  unavailable until a native adapter is written.
- Model name heuristics (`model_name.to_lowercase().contains("gemini")`, etc.)
  determine routing. Unknown models fall back to OpenAI-compatible.
- Adding a native adapter is triggered only by a concrete feature need, not
  by provider identity alone.

## R8 — Deferral Ledger

### R2: JSON-schema structured output — DEFER-229-R2

**What:** `response_format` parameter for structured JSON output from models.

**Why:** Feature addition, not a bug. Adapters work without it; tool call
parsing handles raw text. Needed for future reliability improvements.

**Remaining:**
- Add `response_format` to adapter request bodies (OpenAI-compatible:
  `{"type": "json_object", "schema": {...}}`, Anthropic:
  `{"type": "json", "json_schema": {...}}`).
- Thread `response_format` through the executor.
- Add `structured_output: Option<serde_json::Value>` to `ToolConfig` or
  `RequestConfig`.
- ADR documenting structured output decision and provider support matrix.

**Tracking:** PONYTAIL-DEBT.md.

### R4: Bedrock/Vertex test hardening — DEFER-229-R4

**What:** Integration tests for Bedrock SigV4 and Vertex GCP auth flows.

**Why:** Test hardening, not a production bug. Adapter selection tests cover
routing. Signing correctness is validated in production.

**Remaining:**
- Add `#[ignore]` integration tests for Bedrock request signing with mock AWS
  credentials.
- Add `#[ignore]` integration tests for Vertex request signing with mock GCP
  credentials.

**Tracking:** PONYTAIL-DEBT.md.

### R6: Keychain stub assessment — DEFER-229-R6

**What:** Assess orphaned `keyring` dependency after `kf-budget-hosts` deletion.

**Why:** Depends on WO 22.7-R1 completion (kf-budget-hosts deletion).

**Remaining:**
- After `kf-budget-hosts` deletion, verify no `keyring` dependency remains
  without a consumer.
- Remove `keyring` from `Cargo.toml` if unused.

**Tracking:** PONYTAIL-DEBT.md.

### R7: Native provider decision — this ADR

Documented above. The decision is recorded; no further action needed.

## Consequences

- Every adapter-gap deferral from WO 21.4/22.9 is now explicitly tracked.
- No silent deferrals remain.
- ADR-070 is the single source of truth for the OpenAI-compat default decision
  and all deferred items.