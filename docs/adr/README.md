# Plugin3 Architecture Decision Records

This package documents the design of Plugin3 — the output-side
sibling of the Stratum input-compression plugin.

## Scope

Plugin1 (KirkForge-Plugin) routes and verifies. Plugin2 (Stratum)
compresses input. Plugin3 slices output and enforces a token
budget. The three cover input-routing, input-compression, and
output-control respectively.

## Index

| ADR | Title | Status |
|-----|-------|--------|
| [0001](./0001-purpose.md) | Purpose — output-side sibling of Stratum | Accepted |
| [0002](./0002-workspace.md) | Workspace layout | Accepted |
| [0003](./0003-output-split.md) | SlicingTransform + CompactionTransform | Accepted |
| [0004](./0004-offload-store.md) | OffloadStore reuse from Stratum | Accepted |
| [0005](./0005-token-budget.md) | Three-state token budget guard | Accepted |
| [0006](./0006-tool-output-detector.md) | Tool output detection | Accepted |
| [0007](./0007-slicing-orchestrator.md) | Parallel slicing orchestrator | Accepted |
| [0008](./0008-compaction-strategy.md) | Conversation-length compaction | Accepted |
| [0009](./0009-hooks-model.md) | Hook surface — PostToolUse, UserPromptSubmit, PreCompact | Accepted |
| [0010](./0010-cost-reporting.md) | Cost reporting — usage.jsonl + report subcommand | Accepted |
| [0011](./0011-persistent-knowledge.md) | Persistent knowledge (deferred) | Deferred |
| [0012](./0012-speculative-priming.md) | Speculative priming (deferred) | Deferred |
| [0013](./0013-output-shim.md) | Per-host output shim | Accepted |
| [0014](./0014-state-management.md) | State management — XDG dirs, flag file | Accepted |
| [0015](./0015-cli-design.md) | CLI design | Accepted |
| [0016](./0016-test-strategy.md) | Test strategy | Accepted |
| [0017](./0017-build-features.md) | Build profile and feature gating | Accepted |

## Cross-references

ADRs that reuse a Stratum design cite the Stratum ADR by number
rather than re-deriving it. The shared trait shapes (OffloadStore,
compression pipeline, layered content detection) are documented
once in the Stratum ADRs and inherited here.

## Reading order

Newcomers should read 0001 → 0002 → 0003 → 0005 → 0006 → 0009 →
0013 → 0015 in that order. The other ADRs are reference material.