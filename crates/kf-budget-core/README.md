# Plugin3 — Output-side token budget + slicing

Output-side sibling of the Stratum input-compression plugin. Slices oversized
tool results, enforces a per-conversation token budget, and tracks cost per turn.

## State

| Metric | Value |
|--------|-------|
| Tests | 635 passing |
| Crates | `kf-budget-core` |
| ADRs | 0017 (build features), 0016 (test strategy), 0015 (CLI design) |
