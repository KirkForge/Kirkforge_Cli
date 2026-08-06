# Cost Tracking & Pricing Tables

**Sources:** claude-code-rust (`total_cost` field on `Conversation`), vix (`internal/protocol/cost.go`)
**Goal:** Track per-turn and cumulative cost with model-aware pricing tables, display in the TUI status bar, cap spending in non-interactive mode.

## Why

- Claude Code bleeds tokens through tool call round-trips — users don't see the cost
- Ollama is free locally but users may proxy through paid providers (OpenAI, OpenRouter)
- Cost awareness changes behavior: users approve fewer expensive tool calls when they see the running total
- Budget caps prevent runaway sessions in CI/headless mode

## Pricing Table

```rust
struct Pricing {
    model_prefix: &'static str,  // longest-prefix match wins
    input_per_mtok: f64,
    output_per_mtok: f64,
    cache_write_per_mtok: f64,
    cache_read_per_mtok: f64,
}

const PRICING_TABLE: &[Pricing] = &[
    // Anthropic (via proxy)
    Pricing { model_prefix: "claude-opus-4", input: 15.00, output: 75.00, cache_write: 18.75, cache_read: 1.50 },
    Pricing { model_prefix: "claude-sonnet-4", input: 3.00, output: 15.00, cache_write: 3.75, cache_read: 0.30 },
    Pricing { model_prefix: "claude-haiku", input: 0.25, output: 1.25, cache_write: 0.30, cache_read: 0.05 },
    // OpenAI (via proxy)
    Pricing { model_prefix: "gpt-4", input: 10.00, output: 30.00, cache_write: 0., cache_read: 0. },
    // Free models (Ollama local)
    Pricing { model_prefix: "", input: 0.0, output: 0.0, cache_write: 0.0, cache_read: 0.0 }, // catch-all
];

fn calculate_cost(model: &str, input_tokens: usize, output_tokens: usize) -> f64 {
    let pricing = PRICING_TABLE.iter()
        .find(|p| model.starts_with(p.model_prefix))
        .unwrap_or_else(|| PRICING_TABLE.last().unwrap());
    let input_cost = (input_tokens as f64 / 1_000_000.0) * pricing.input_per_mtok;
    let output_cost = (output_tokens as f64 / 1_000_000.0) * pricing.output_per_mtok;
    input_cost + output_cost
}
```

## Data Flow

```
Adapter emits StreamEvent::Done { usage }
    → Executor records completion_tokens + prompt_tokens
    → CostTracking struct computes per-turn cost
    → EventBus fires CostEvent { turn_cost, cumulative_cost }
    → TUI status bar updates (↓ $0.042)
    → Non-interactive mode: check budget cap, abort if exceeded
```

## Integration Points

| File | Change |
|------|--------|
| `src/shared/mod.rs` | Add `CostTracking` struct, `Pricing` table, `calculate_cost()` |
| `src/session/executor.rs` | In `Done` handler: compute cost, emit `TurnEvent::CostStats` |
| `src/tui/app.rs` | Add `cumulative_cost: f64` to `AppState` |
| `src/tui/widgets/status.rs` | Render per-turn cost next to token counts |
| `src/main.rs` | Add `--max-cost` flag to Cli |

## UI Display

```
 ◆ qwen2.5:0.5b  │  ↑1.2K ↓4.5K │ $0.00 │ 2m34s
```

For Ollama local models, cost stays $0.00. For proxied models (OpenAI, Anthropic), it shows the running total.

## Budget Cap (Non-interactive)

```rust
if let Some(max_cost) = cli.max_cost {
    if cumulative_cost > max_cost {
        eprintln!("[cost] Budget of ${:.2} exceeded (${:.2})", max_cost, cumulative_cost);
        break;
    }
}
```