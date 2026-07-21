# Plugin3 — Output-side token budget + slicing

Output-side sibling of the Stratum input-compression plugin. Slices oversized
tool results, enforces a per-conversation token budget, and tracks cost per turn.

## State

| Metric | Value |
|--------|-------|
| Tests | 1478 passing |
| Crates | `plugin3-core`, `plugin3-hosts`, `plugin3-cli` |
| ADRs | 0017 (build features), 0016 (test strategy), 0015 (CLI design) |
