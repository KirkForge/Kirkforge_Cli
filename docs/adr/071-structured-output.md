# ADR-071: JSON-Schema Structured Output

**Status:** Accepted
**Date:** 2026-08-08
**Replaces:** WO 22.9-R2 (deferred), WO 20.0.8-G4 (structured output half)

## Context

No `response_format: { type: "json_object", schema: ... }` support existed in any
adapter. The `json_mode: bool` flag only toggled unstructured JSON mode. This is
a competitive gap vs Claude Code, Codex CLI, and opencode, all of which can
request structured JSON responses conforming to a schema.

## Decision

Add `ResponseFormat` enum (`Text`, `JsonObject`, `JsonSchema { name, schema }`)
to `src/shared/mod.rs`. Extend `ModelAdapter` trait with
`set_response_format(ResponseFormat)`. Each body builder emits the
provider-correct structured output field.

## Provider support

| Provider | JsonObject | JsonSchema |
|----------|-----------|------------|
| OpenAI-compat | `response_format: {type: "json_object"}` | `response_format: {type: "json_schema", json_schema: {name, schema}}` |
| Anthropic | System-prefill instruction | Tool-use trick: synthetic `respond_with_{name}` tool + forced `tool_choice` |
| Ollama | `"format": "json"` | `"format": {type: "object", properties: ...}` |

## Fallback

Providers that don't support `JsonSchema` silently degrade to `JsonObject` (or
`Text` with a system-prompt instruction for providers that don't support any
JSON mode).

## Backward compatibility

Config `json_mode = true` maps to `ResponseFormat::JsonObject` via
`ModelConfig::effective_response_format()`. `json_mode = false` (default) maps
to `ResponseFormat::Text`. Old configs continue to work without changes.
