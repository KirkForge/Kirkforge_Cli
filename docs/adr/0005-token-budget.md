# ADR-0005: Three-state token budget guard

- **Status:** Accepted
- **Date:** 2026-06-24

## Context

A user wants a ceiling on how many tokens a session burns.
Plugin3's load-bearing feature is the budget guard: every
turn, before the user's prompt is sent to the model, the guard
checks the running token total and decides whether the turn is
allowed.

Three states cover the surface:

1. **Under** — well within budget. The turn is allowed
   unconditionally.
2. **Approaching** — within N% of the ceiling. The turn is
   allowed, but the PostToolUse hook proactively slices the
   next tool output to slow the burn.
3. **Over** — at or above the ceiling. The turn is *not*
   sent as-is. Plugin3 auto-intervenes by slicing the largest
   recent tool output (via ADR-0003's `SlicingTransform`) and
   re-checking.

A boolean "under/over" guard would be too coarse: a user who
hovers at 95% of budget wants a slow-down signal, not a
hard-stop. A multi-tier guard (Under / Approaching / Over /
Way Over) would over-engineer; three states is enough.

## Decision

### TokenBudget struct

```rust
// crates/plugin3-core/src/budget.rs

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BudgetState { Under, Approaching, Over }

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct TokenBudget {
    /// Hard ceiling in tokens per session.
    pub ceiling: usize,
    /// Approaching band starts at ceiling * approaching_ratio.
    pub approaching_ratio: f64,         // default: 0.8
    /// Current accumulated tokens (sum of inputs + outputs).
    pub used: usize,
}

impl Default for TokenBudget {
    fn default() -> Self {
        Self { ceiling: 200_000, approaching_ratio: 0.8, used: 0 }
    }
}

/// User-editable subset of `TokenBudget` (the runtime `used` is
/// session-local and intentionally absent here). This is the shape
/// that `~/.config/plugin3/config.toml`'s `[budget]` section holds.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct BudgetConfig {
    pub ceiling: usize,
    pub approaching_ratio: f64,
}

impl Default for BudgetConfig {
    fn default() -> Self { Self { ceiling: 200_000, approaching_ratio: 0.8 } }
}

/// Wrapper that emits the `[budget]` section header (and a
/// sibling `[usage]` section per ADR-0010 § Privacy). Bare-
/// serialising `BudgetConfig` produces flat key=value pairs with
/// no section header — the documented shape diverges.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ConfigFile {
    #[serde(default)]
    pub budget: BudgetConfig,
    #[serde(default)]
    pub usage: UsageConfig,
}

impl TokenBudget {
    pub fn state(&self) -> BudgetState {
        // ponytail: ceiling == 0 → Over (never allow). The
        // division-by-zero guard returns Over unconditionally.
        if self.ceiling == 0 { return BudgetState::Over; }
        let ratio = self.used as f64 / self.ceiling as f64;
        if ratio >= 1.0 { BudgetState::Over }
        else if ratio >= self.approaching_ratio { BudgetState::Approaching }
        else { BudgetState::Under }
    }

    pub fn can_send(&self, incoming: usize) -> bool {
        self.used.saturating_add(incoming) <= self.ceiling
    }

    pub fn record(&mut self, n: usize) {
        self.used = self.used.saturating_add(n);
    }

    pub fn remaining(&self) -> usize {
        self.ceiling.saturating_sub(self.used)
    }
}
```

ponytail: the earlier draft declared only `BudgetState` and
`TokenBudget`. The MVP also exports `BudgetConfig`,
`ConfigFile`, and `UsageConfig` (the latter in `budget.rs`
so `ConfigFile` can name it without crossing module
boundaries — see ADR-0005 § Defaults). The runtime
`TokenBudget::used` field is **not** in `BudgetConfig`:
runtime counters must not bleed into `config.toml` via
`budget set --default` (the set command persists
`BudgetConfig`, not `TokenBudget`). The drift test
`config_file_emits_budget_section_header` pins the
`[budget]` section header; `budget_config_defaults_match_token_budget`
pins the default-value agreement.

### Default ceiling

```toml
# ~/.config/plugin3/config.toml
[budget]
ceiling = 200_000
approaching_ratio = 0.8
```

The 200 000 default is a starting point — roughly the
context window of Claude Sonnet 4.6 plus room for system
prompts and tool definitions. A user with a smaller model
sets a smaller ceiling; a user with Opus and a long task sets
a larger one.

### Auto-intervention

When `can_send(incoming) == false`:

```rust
pub enum Intervention {
    /// Send the turn as-is.
    Allow,
    /// Send the turn but warn the user.
    Warn { remaining: usize },
    /// Slice the largest recent tool output to fit the
    /// turn into budget.
    Slice { target_key: String, slice_to: usize },
    /// Refuse the turn — too big even after slicing. Emit a
    /// compaction hint.
    Compact { reason: String },
}

pub fn decide(
    budget: &TokenBudget,
    incoming: usize,
    recent_tool_outputs: &[(String, usize)],  // (key, bytes)
) -> Intervention {
    if budget.can_send(incoming) {
        return match budget.state() {
            BudgetState::Under => Intervention::Allow,
            BudgetState::Approaching => Intervention::Warn {
                remaining: budget.remaining(),
            },
            BudgetState::Over => Intervention::Warn {
                remaining: 0,
            },
        };
    }
    // We are over budget. Try to slice the largest recent
    // tool output to free enough room.
    let needed = incoming - budget.remaining();
    if let Some((key, size)) = recent_tool_outputs
        .iter()
        .max_by_key(|(_, s)| *s)
    {
        if *size > needed + SLICE_OVERHEAD {
            let slice_to = size.saturating_sub(needed);
            return Intervention::Slice {
                target_key: key.clone(),
                slice_to,
            };
        }
    }
    Intervention::Compact {
        reason: format!(
            "session at {}/{} tokens; cannot fit {} more",
            budget.used, budget.ceiling, incoming
        ),
    }
}
```

The `SLICE_OVERHEAD` constant is 256 bytes — the typical
slice marker plus the head/tail sections.

### UserPromptSubmit hook flow

```rust
// crates/plugin3-cli/src/hooks/mod.rs

pub(crate) fn user_prompt_submit() {
    let Some(payload) = read_stdin_json::<UserPromptSubmitPayload>() else {
        // ADR-0009: default to Allow on parse failure — the host's
        // own validation catches garbage; we should not block.
        // ponytail: serialise `Intervention::Allow` directly. The
        // wire shape matches `UserPromptSubmitResponse::Allow`
        // (same `#[serde(tag = "kind", rename_all = "snake_case")]`
        // rule on both enums); using the core type here avoids a
        // second hand-written reference that would have to track
        // variant renames.
        println!("{}", serde_json::to_string(&Intervention::Allow).unwrap());
        return;
    };
    let mut b = super::load_budget();
    let mut recent = super::load_recent_outputs();
    let incoming = estimate_tokens(&payload.prompt);
    b.record(incoming);
    // ponytail: `VecDeque::make_contiguous()` is the canonical
    // stdlib way to borrow the deque's ring buffer as a `&[T]`.
    // It is O(1) when the deque is already contiguous (the
    // common case after a series of `push_back`s from a fresh
    // deque) and O(n) when the ring buffer wraps — bounded at
    // 32 entries on the UserPromptSubmit path. The `mut` on
    // the binding is forced by the method signature, not by
    // intent at the call site; the call site only reads.
    let intervention = decide(&b, incoming, recent.make_contiguous());
    // ponytail: classify_kind returns None for Intervention::Allow —
    // a healthy turn is not a "significant event" per ADR-0010 and
    // must not inflate the warnings count in `plugin3 report
    // --summary`. The Option forces the skip to be explicit at the
    // call site rather than smuggled through the kind enum.
    if let Some(kind) = classify_kind(&intervention) {
        emit_usage(&UsageRecord {
            kind,
            session_id: payload.session_id.clone(),
            tokens_used: Some(b.used),
            tokens_ceiling: Some(b.ceiling),
            ..empty_record()
        });
    }
    super::save_budget(&b);
    // ponytail: `Intervention` (plugin3-core) and
    // `UserPromptSubmitResponse` (plugin3-hosts) are byte-equivalent
    // tagged enums on the wire — both `#[serde(tag = "kind",
    // rename_all = "snake_case")]` over the same four-variant shape.
    // Serialising the core enum directly produces the exact JSON
    // shape the canonical host expects; the previous hand-written
    // 4-arm `Intervention → UserPromptSubmitResponse` match
    // duplicated the variant list (adding a 5th variant required
    // updating both enums and the match arms — easy to forget one).
    // serde derives make the rename + tag work in both enums; the
    // conversion goes away.
    println!("{}", serde_json::to_string(&intervention).unwrap());
}
```

ponytail: the earlier draft specified a `HookResponse` enum
local to the hook module plus `tracing::warn!` /
`tracing::info!` events on every intervention arm. The MVP
does **not** depend on `tracing` (ADR-0017 § Workspace
Cargo.toml) and does **not** declare a local `HookResponse`
enum — the response is `Intervention` from `plugin3-core`
(serialised directly), and the host shim already maps
`UserPromptSubmitResponse` (the host-side tagged enum) to
its own payload format. The two enums are wire-equivalent
so a single serde-tagged `Intervention` covers both the
canonical host-shim surface and the CLI's internal
decision type. The drift test
`hooks_mod_drift::adr_0009_*` pins the no-`tracing` shape
for the hooks module; this ADR pins it for the budget
flow's no-`tracing` shape.

`Intervention::Warn { remaining }` is a non-blocking
advisory; the host may show it as a statusline hint.
`Intervention::Slice { target_key, slice_to }`
*modifies* the recent tool output via the OffloadStore.
The host sees the modified tool output on the next read.

### Token estimation

The MVP uses a cheap, deterministic estimator:

```rust
pub fn estimate_tokens(s: &str) -> usize {
    // Heuristic: ~4 chars per token for English text.
    // JSON / source code: ~3 chars per token.
    let bytes = s.len();
    if s.starts_with('{') || s.starts_with('[') || s.starts_with("fn ") {
        bytes / 3
    } else {
        bytes / 4
    }
}
```

The estimator is conservative. A future ADR swaps in a real
tokeniser (tiktoken or a model-specific counter) when the
error margin becomes load-bearing.

### Persistence

The budget state lives in a flag file (ADR-0014). Every
`record` writes the new state atomically. A crash mid-write
loses at most one turn's worth of increment — acceptable
because the guard is *advisory*; the worst case is "the next
turn is allowed when it should have warned."

## Consequences

Negative first:

- Three states is one more than two. The trade is the
  Approaching state gives the user a heads-up before the
  hard-stop kicks in.
- The auto-slicing is "best-effort" — it slices the *largest*
  recent output, not the optimal one. A future ADR adds
  multi-output slicing.
- The token estimator is a heuristic. Off-by-30% is possible
  for code-heavy content. The drift test pins the estimator's
  output for a known corpus.

Positive:

- The guard is honest about its three states. A user reading
  the budget state knows exactly where they are.
- Auto-slicing is reversible: the user can `cat` the slice
  marker to retrieve the full content (ADR-0010).
- The flag file is a single small file; O(1) writes are cheap.

## Implementation notes

The `recent_tool_outputs` list is maintained by the
PostToolUse hook (ADR-0009). Every successful tool result is
appended; every compaction (manual or automatic) evicts the
oldest entries. The list is bounded at 32 entries (per
ADR-0014 § Recent outputs file); older entries are dropped.

The response is `UserPromptSubmitResponse` from
`plugin3-hosts` (ADR-0013):

```rust
// crates/plugin3-hosts/src/lib.rs

pub enum UserPromptSubmitResponse {
    Allow,
    Warn { remaining: usize },
    Slice { target_key: String, slice_to: usize },
    Compact { reason: String },
}
```

ponytail: the earlier draft specified a local `HookResponse`
enum with `Ok` and `OkWithWarning(String)` variants. The MVP
uses the host-shared `UserPromptSubmitResponse` enum —
Claude Code's hook envelope does not have a separate "ok"
vs "ok-with-warning" path (the warning is the `Warn`
variant). The drift test
`classify_kind_allow_returns_none` (in `cost.rs`) pins the
`Allow → None` mapping so a healthy turn doesn't emit a
`budget_warn` usage record.

The host shim (ADR-0013) translates `UserPromptSubmitResponse`
into the host's payload format. Claude Code has a JSON
envelope; Cursor has a different envelope; the shim
normalises.