# ADR-0027: Context management depth — cache-stem reuse, microcompaction, and tool-result truncation

- **Status:** Accepted
- **Date:** 2026-07-20

## Context

KirkForge-Cli already had basic context management: per-tool caps in `PromptBuilder`, naive `/compact` (ADR-0008), and LLM summarization behind a config flag. The A− → A gap is *depth*, not presence:

1. **Prompt-cache-stem reuse.** `PromptBuilder::build_stem` existed but was not wired into the adapter request path. `cache_control` markers were placed heuristically on the last two prefix messages, not on a stable, content-identical system/tool stem.
2. **Microcompaction.** `/compact` rewrites the whole conversation log. There was no per-turn, automatic middle compression that fires when `build_messages` detects an over-budget request.
3. **Tool-result truncation policy.** `max_tool_result_chars` was applied only to `bash`. Other tools could persist huge results, and there was no policy for repeated identical results inside a single turn.

This ADR records the design for closing those three gaps.

## Decision

### 1. Prompt-cache-stem reuse

`PromptBuilder` now memoises the system-prompt `Message` it produces. The memo key covers every input that can change the system prompt text:

- model name and `supports_thinking`
- tool names list
- carryover block
- memory context and memory knobs (`enabled`, `max_tokens`, `top_n`)
- `--system` override hash

When `build()` is called with the same key, the *same* `Message` value is returned. Because the bytes going to the provider are byte-for-byte identical across turns, the provider's KV-cache can hit for the stable prefix. The stem size (system prompt + tool JSON) is estimated and emitted as `TurnEvent::CacheStats` alongside the adapter's reported `cached_tokens`, so the TUI can verify that cache reuse is actually happening.

The adapter's existing `cache_control` marker logic remains in place; the cache-stem memoisation is complementary — it guarantees content stability for the part the adapter already tries to cache.

### 2. Microcompaction

A new `session/prompt/microcompaction.rs` module runs inside `PromptBuilder::build_messages` *after* per-tool caps and dedup but *before* minification/stubbing/truncation. If the estimated token count exceeds the 90%-of-max budget, it:

- preserves the leading system anchor
- preserves the last 5 messages verbatim
- replaces the middle messages with a single deterministic system summary that captures tool names, file paths, and error markers

This is distinct from `/compact` because it happens at request-build time and does not mutate the conversation log. It is a lower-latency, lower-quality complement to the LLM-based `/compact` summarizer.

### 3. Tool-result truncation policy

`max_tool_result_chars` now applies to all tools, not just `bash`. The condition `tc.name == "bash" || max_tool_result_chars > 0` keeps the existing bash behaviour and extends it to every tool when the cap is configured (default 4000).

Adjacent identical tool results are still deduplicated to `[duplicate tool result omitted — see previous identical result]`. Additionally, a third (or later) identical result in a row is collapsed to `[unchanged from previous identical tool result]`. This implements the workorder's "repeated tool results" truncation policy without requiring semantic comparison.

## Consequences

- The adapter body builders receive identical system messages across turns, improving KV-cache hit probability on Anthropic and OpenAI-compatible endpoints.
- The TUI can display a cache-hit ratio because `TurnEvent::CacheStats` exposes `cached_tokens`, `prompt_tokens`, and `stem_tokens`.
- Over-budget requests get a lightweight automatic compression before the heavier minification/stubbing/truncation pipeline runs.
- Non-bash tools no longer risk flooding the conversation log with huge results, and repeated identical tool results cost only a short marker after the second occurrence.

## ponytail

- The heuristic microcompaction summary is not LLM-quality; semantic summarization remains behind `/compact`.
- Cache-hit verification depends on the provider actually reporting `cache_read_input_tokens` / `cached_tokens`; not all adapters do.
- The unchanged-marker policy only catches *adjacent, identical* results; non-adjacent duplicates or near-duplicates are handled by the existing per-tool caps and stubbing.

## ceiling

- Microcompaction could drop nuance from user/assistant prose in the middle. Upgrade path: wire the LLM summarizer behind a config flag for automatic semantic microcompaction.
- Applying `max_tool_result_chars` to all tools changes default behaviour for read_file, glob, grep, etc. Operators who relied on the old unlimited defaults may need to raise the cap.
