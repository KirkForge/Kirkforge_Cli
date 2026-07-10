# ADR-0008: Conversation-length compaction strategy

- **Status:** Accepted
- **Date:** 2026-06-24

## Context

The budget guard (ADR-0005) refuses a turn when slicing cannot
free enough room. The next step is a *compaction* hint: tell
the host that the session history should be compacted.

Two flavours of compaction exist:

1. **Local compaction** — heuristic, runs in the plugin, no
   LLM call. Summarises old turns by extracting titles,
   first lines, and key tokens.
2. **LLM compaction** — calls a model to summarise old turns.
   Better quality, higher latency, costs tokens to save tokens.

Plugin3 ships only local compaction in the MVP. LLM compaction
is a future ADR.

## Decision

### When to suggest compaction

The orchestrator suggests compaction in two cases:

1. The budget is `Over` and the largest recent tool output is
   smaller than the incoming prompt (no slice can save us).
2. The user has explicitly asked for compaction
   (`plugin3 compact` subcommand).

### CompactHint payload

```rust
// crates/plugin3-core/src/compaction.rs

pub struct CompactHint {
    pub reason: String,
    pub tokens_used: usize,
    pub tokens_ceiling: usize,
    pub oldest_turn: Option<usize>,  // index into the conversation
    pub newest_turn: Option<usize>,
}

/// Turn index into the conversation history (host-side).
/// Used by `build_hint` to populate the `oldest_turn` /
/// `newest_turn` fields.
pub struct Turn {
    pub index: usize,
    pub role: String,
    pub content_preview: String,
}

pub fn build_hint(budget: &TokenBudget, history: &[Turn]) -> CompactHint {
    CompactHint {
        reason: format!(
            "session at {}/{} tokens; compaction suggested",
            budget.used, budget.ceiling
        ),
        tokens_used: budget.used,
        tokens_ceiling: budget.ceiling,
        oldest_turn: history.first().map(|t| t.index),
        newest_turn: history.last().map(|t| t.index),
    }
}

/// Output of a compaction transform — the summary plus the
/// bookkeeping the cost reporter reads.
pub struct CompactedOutput {
    pub summary: String,
    pub bytes_saved: usize,
    pub lossy: bool,
}

pub trait CompactionTransform: Send + Sync {
    fn name(&self) -> &'static str;
    fn apply(&self, input: &str) -> Result<CompactedOutput, TransformError>;
}

/// Heuristic line filter — keeps the first non-empty short
/// line of each "paragraph", drops noisy long lines.
pub struct LocalSummaryCompactor {
    pub max_output_bytes: usize,  // default: 8192
}
```

ponytail: the earlier draft omitted the `LocalSummaryCompactor`
struct, the `CompactionTransform` trait, and the `Turn` /
`CompactedOutput` supporting types. The MVP ships all of
them — the `PreCompact` hook handler in
`crates/plugin3-cli/src/hooks/mod.rs` runs the compactor
over the per-turn previews before emitting the hint, so the
host's compactor has a head-start. The default
`max_output_bytes: 8192` is pinned by the in-file test
`local_summary_compactor_default_matches_adr`; the
`name() == "local_summary"` contract is pinned by
`local_summary_compactor_name_is_pinned`. A future ADR
adds an `LlmCompactor` that implements the same trait.

### Local summary

The local summary uses `LocalSummaryCompactor` (ADR-0003). It
extracts:

- The first line of each turn.
- The first sentence of each user prompt.
- The first line of each tool result (often the most
  diagnostic).
- The headers / section titles from any structured output.

```rust
pub fn local_summarise(input: &str, max_bytes: usize) -> String {
    let mut out = String::new();
    for line in input.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        // Heuristic: keep the first line of each paragraph,
        // skip empty, skip overly long lines (likely noisy).
        if line.len() > 500 {
            continue;
        }
        // Pre-check the bound so a single line longer than
        // `max_bytes` doesn't blow past the cap. The +1 is
        // the trailing `\n` we are about to push.
        if out.len() + line.len() + 1 > max_bytes {
            break;
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}
```

The heuristic is intentionally crude. The MVP goal is "give
the user a sense of what the conversation was about" — not
"produce a publishable summary". A future LLM-based compactor
improves quality when needed.

### Host integration

The host shim (ADR-0013) translates `CompactHint` into the
host's payload:

- **Claude Code** — emit a `PreCompact` hook payload with the
  `reason` and `tokens_used` fields. The host's own compactor
  consumes the hint.
- **Cursor** — same shape, different envelope.
- **Aider** — set a `COMPACT=1` environment variable on the
  next invocation; Aider's own compaction logic kicks in.

Plugin3 does not implement the compaction itself; it *suggests*
compaction and the host does the work. This is the right
boundary because the host has the conversation state — Plugin3
only knows token counts.

### Compact subcommand

```rust
// crates/plugin3-cli/src/main.rs

#[derive(Parser, Debug)]
#[command(about = "Inspect or set the token budget.")]
struct BudgetCmd {
    #[command(subcommand)]
    sub: BudgetSub,
}

#[derive(Subcommand, Debug)]
enum BudgetSub {
    /// Print the current budget state (used, ceiling, state).
    Status,
    /// Set the budget ceiling for this session.
    Set {
        ceiling: usize,
        /// Persist as the default in config.toml (ADR-0015).
        #[arg(long)]
        default: bool,
    },
    /// Emit a CompactHint for the host's compactor (ADR-0008).
    Compact {
        /// Print the hint as JSON (default: human-readable).
        #[arg(long)]
        json: bool,
    },
}
```

ponytail: the earlier draft collapsed the clap structure
into a single `pub enum BudgetCmd` with three variants. The
MVP uses a `BudgetCmd` *struct* that wraps a `BudgetSub`
*enum* — clap's subcommand pattern. The `Set` arm carries a
`--default: bool` flag (ADR-0015: `plugin3 budget set --default`
persists the ceiling to `config.toml`); the earlier draft's
`Set { ceiling: usize }` lacked the flag and would have
forced a future ADR to retrofit persistence.

`plugin3 budget compact` emits the `CompactHint` via the
configured host shim. With `--json`, it prints the hint to
stdout for debugging.

### Conversation history

The plugin does not maintain a full conversation log; the
host does. Plugin3 reads token counts via the budget state
file (ADR-0014) and the `usage.jsonl` stream (ADR-0010). The
`oldest_turn` / `newest_turn` fields are hints to the host —
the host knows the actual conversation.

## Consequences

Negative first:

- The local summary is crude. A user who wants a high-quality
  summary must enable LLM compaction (future ADR) or rely on
  the host's native compactor.
- The plugin does not implement the compaction itself. A user
  who runs `plugin3 budget compact` on a host that does not
  support the hint sees a no-op.

Positive:

- The MVP ships with no LLM call. Local summarisation is
  deterministic, fast, and free.
- The `CompactHint` is small and structured; the host shim
  translates it.
- The `usage.jsonl` stream records every compaction event so
  the user can audit how often the budget triggered one.

## Implementation notes

The compaction module lives at
`crates/plugin3-core/src/compaction.rs`. It depends on the
`budget` module (ADR-0005) and `serde` for serialisation.

Tests:

1. `local_summarise_empty_input` — empty input returns empty
   output.
2. `local_summarise_short_input` — short input returns the
   input truncated at `max_bytes`.
3. `local_summarise_skips_long_lines` — lines over 500 chars
   are dropped.
4. `build_hint_includes_turn_range` — when history is
   non-empty, the hint includes the first/last turn indices.
5. `build_hint_no_history` — when history is empty, the
   hint has `None` for both fields.

The drift test for `local_summarise` pins the output for a
known corpus so a contributor who tweaks the heuristic
surfaces the change for review.