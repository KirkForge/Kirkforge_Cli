# ADR 003: Model Abstraction — Single Stream, Per-Model Adapters

## Status

Accepted

## Date

2026-06-03

## Context

We run three models with different response formats through Ollama:

- **GLM-5.1:Cloud** — emits a `thinking` field alongside `content` in `/api/chat` responses. Thinking tokens arrive in `delta.thinking` or `message.thinking` depending on Ollama version. They are not instructions — they are the model's internal chain-of-thought, useful to show but never fed back as input.
- **DeepSeek-v4-Pro** — structured tool calls arrive as a complete block rather than streaming tokens. The model can issue multiple tool calls in one response. Tool call format through Ollama's OpenAI-compat layer differs slightly from its native `/api/chat` representation.
- **Gemini 3.0 Flash 1M** — streams token by token with no thinking field, no native tool calls (uses OpenAI-compatible function calling through Ollama's translation layer). Different chunk boundaries than the other two.

ADR 001 decided to speak Ollama's APIs directly. But "Ollama's APIs" is two endpoints (`/api/chat` and `/v1/chat/completions`) that each return model-specific payloads. Writing one parser that handles all three is a race condition waiting to happen.

## Decision

A two-layer architecture: a **stream abstraction** that all model adapters implement, and one **adapter per model** that translates model-specific wire formats into the common stream type.

```
┌─────────────────────────────────────────────────────┐
│                    Chat Session                      │
│  (owns conversation state, tool dispatch, context)   │
└──────────┬──────────────────────────────────────────┘
           │ sends/receives StreamEvent
┌──────────▼──────────────────────────────────────────┐
│              StreamEvent enum                        │
│  ┌─────────────┐ ┌──────────┐ ┌──────────────────┐  │
│  │ Text(token)  │ │ ToolCall │ │ Thinking(token)   │  │
│  └─────────────┘ └──────────┘ └──────────────────┘  │
│  ┌──────────────┐ ┌────────────┐ ┌───────────────┐  │
│  │ ToolResult   │ │ Error      │ │ Done(reason)   │  │
│  └──────────────┘ └────────────┘ └───────────────┘  │
└──────────┬──────────────────────────────────────────┘
           │ implemented by
┌──────────▼──────────────────────────────────────────┐
│              ModelAdapter trait                       │
│  fn stream(&self, messages) -> Stream<StreamEvent>   │
│  fn model_info(&self) -> ModelInfo                   │
└──────────┬──────┬──────────┬────────────────────────┘
           │      │          │
┌──────────▼┐ ┌───▼────┐ ┌──▼──────────┐
│ GLMAdapter │ │DeepSeek│ │GeminiAdapter │
│            │ │Adapter │ │              │
│ +thinking  │ │+tool   │ │+openai-compat│
│  field     │ │ blocks │ │  streaming   │
└────────────┘ └────────┘ └──────────────┘
```

### StreamEvent enum

```rust
#[derive(Debug, Clone)]
pub enum StreamEvent {
    /// A token of visible response text
    Text(String),
    /// A token of internal thinking (CoT) — GLM only, others return empty
    Thinking(String),
    /// A complete tool call (name + args)
    ToolCall {
        id: String,
        name: String,
        arguments: serde_json::Value,
    },
    /// A tool result being sent back
    ToolResult {
        id: String,
        content: String,
    },
    /// A non-recoverable streaming error
    Error(String),
    /// Stream completed
    Done {
        finish_reason: FinishReason,
        usage: Option<TokenUsage>,
    },
}
```

The session layer never sees raw JSON. It reads `StreamEvent` values from a channel. The adapter handles wire format, thinking token extraction, tool call assembly, and retry/backoff.

### ModelInfo

```rust
pub struct ModelInfo {
    pub supports_thinking: bool,
    pub tool_call_format: ToolCallStyle,  // Native | OpenAiCompat | None
    pub max_context_tokens: usize,
    pub recommended_temperature: f64,
}
```

The UI uses `ModelInfo` to decide: show a thinking panel, render tool calls as JSON vs. formatted, warn about context limits.

### Adapter selection

Automatic at connect time. Ollama's `/api/tags` returns model names. We pattern-match:

- `"glm*"` → `GLMAdapter`
- `"deepseek*"` → `DeepSeekAdapter`
- `"gemini*"` → `GeminiAdapter`
- Everything else → `OpenAiCompatAdapter` (falls back to `/v1/chat/completions`)

Override with `--model-type glm|deepseek|gemini|openai` flag.

## Consequences

**Positive:**
- Adding a new model means writing one file that implements `ModelAdapter`. No changes to session, UI, or tool dispatch.
- Thinking tokens are a first-class `StreamEvent` variant. The UI can show them in a collapsible panel, the session never feeds them back into the prompt, and if the model starts emitting them differently, only the adapter changes.
- Tool call normalization: DeepSeek's batch tool calls, Gemini's single-shot function calls, and any future format all converge to the same `ToolCall(vec)` at the session layer.
- Testable in isolation — each adapter can be unit tested against recorded Ollama responses. No real model needed.

**Negative:**
- The adapter layer is one extra allocation hop. On the C-50, that's measurable. Mitigation: each adapter processes chunks on a single thread and sends `StreamEvent` over a bounded channel — no heap cloning of large strings, just index ranges where possible.
- Pattern-matching model names is fragile. GLM might change its model ID format. Mitigation: the `--model-type` override exists for exactly this case, and the OpenAiCompatAdapter catches anything unrecognized.

## Open Questions

- Should tool calls from DeepSeek be streamed token-by-token or assembled as a block? The adapter can do either — start with block assembly (simpler, matches how DeepSeek actually sends them), migrate to streaming if the UI needs progressive tool call rendering.
- Token usage reporting: Ollama's `/api/chat` doesn't always return usage stats. The adapter should fill what it can and leave `None` when unavailable, rather than computing estimates.