# ADR-042: OpenCode Zen provider

## Status

Accepted

## Context

OpenCode Zen is an AI gateway at `https://opencode.ai/zen/v1/chat/completions` offering free models (big-pickle, deepseek-v4-flash-free, mimo-v2.5-free) and paid models (GLM 5.1/5.2, Claude, GPT). KirkForge already has an `openai_compat` adapter that can talk to any OpenAI-compatible endpoint. Supporting Zen means adding the endpoint, routing, and optional API key authentication.

## Decision

1. Add `AdapterKind::OpenCodeZen` variant to the adapter enum.
2. Route `opencode/*` model name prefixes to the Zen endpoint via `openai_compat`.
3. Add `opencode_zen_api_key: Option<String>` and `opencode_zen_endpoint: String` to `Config`. The default endpoint is `https://opencode.ai/zen/v1/chat/completions`.
4. Extend `OpenAiCompatAdapter` to carry an optional `api_key` field. When present, `stream()` adds `Authorization: Bearer <key>` to requests.
5. Extend `adapter_for_with_provider()` with two new parameters (`opencode_zen_endpoint`, `opencode_zen_api_key`) so the Zen adapter can be constructed with the configured endpoint and key.
6. Add `AdapterKind::OpenCodeZen` to the `/model` command match arm so users can switch to Zen models interactively.

## Consequences

- **Positive:** Free subagent model via big-pickle. All Zen models are accessible. The implementation reuses `openai_compat` — no new adapter code.
- **Negative:** External dependency on opencode.ai availability. API key management adds config surface. Rate limits on free models may require retry logic in a future iteration.