# Plugin3 Architecture Decision Records

This package documents the design of Plugin3 — the output-side
sibling of the Stratum input-compression plugin.

## Scope

Plugin1 (KirkForge-Plugin) routes and verifies. Plugin2 (Stratum)
compresses input. Plugin3 slices output and enforces a token
budget. The three cover input-routing, input-compression, and
output-control respectively.

**Series note:** files `001`–`017` are the native KirkForge-Cli ADRs;
files `0001`–`0018` and `0021`–`0022` (indexed below) are the vendored Plugin3 ADRs.

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
| [0011](./0011-persistent-knowledge.md) | Persistent knowledge (rejected — no implementation) | Rejected (2026-07-17) |
| [0012](./0012-speculative-priming.md) | Speculative priming (rejected — no implementation) | Rejected (2026-07-17) |
| [0013](./0013-output-shim.md) | Per-host output shim | Accepted |
| [0014](./0014-state-management.md) | State management — XDG dirs, flag file | Accepted |
| [0015](./0015-cli-design.md) | CLI design | Accepted |
| [0016](./0016-test-strategy.md) | Test strategy | Accepted |
| [0017](./0017-build-features.md) | Build profile and feature gating | Accepted |
| [0020](./0020-parallel-tool-dispatch.md) | Parallel tool dispatch | Accepted |
| [0018](./0018-scheduled-jobs.md) | Cron / scheduled jobs | Accepted |
| [0027](./0027-context-management-depth.md) | Context management depth | Accepted |
| [0028](./0028-verifier-bus-unification.md) | Unify Rust and TS verifier buses | Accepted |
| [0031](./0031-build-test-verifier-slots.md) | Build and test verifier slots | Accepted |
| [0032](./0032-plan-reason-events.md) | PlanReason trace events | Accepted |
| [0021](./0021-computer-use-tool.md) | `computer_use` tool via headless Chrome CDP | Accepted |
| [0022](./0022-anthropic-cloud-routing.md) | Anthropic cloud routing — Bedrock and Vertex | Accepted |
| [0023](./0023-workflow-engine.md) | Programmable JSON workflow engine | Accepted |
| [0029](./0029-test-partitioning.md) | Test partitioning — fast/full/coverage suites | Accepted |
| [0033](./0033-tool-retry-backoff.md) | Exponential backoff on tool-call retries | Accepted |
| [0034](./0034-mid-batch-checkpoint.md) | Mid-batch tool-result checkpointing | Accepted |

## Native KirkForge-Cli ADRs

The same directory also holds native CLI ADRs that use the 3-digit
numbering scheme (`001`–`017`). Recent additions:

- [ADR-019: VS Code extension (Option A PTY wrapper)](./019-vscode-extension.md)
- [ADR-024: Release cadence and semantic versioning](./024-release-cadence.md)
- [ADR-025: Windows parity approach](./025-windows-parity.md)
- [ADR-026: VS Code NDJSON bridge](./026-vscode-ndjson-bridge.md)
- [ADR-027: Context management depth](./0027-context-management-depth.md)
- [ADR-028: Unify Rust and TS verifier buses](./0028-verifier-bus-unification.md)
- [ADR-029: Test partitioning — fast/full/coverage suites](./0029-test-partitioning.md)
- [ADR-030: `--seed` deterministic mode](./030-deterministic-mode.md)
- [ADR-031: Build and test verifier slots](./0031-build-test-verifier-slots.md)
- [ADR-032: PlanReason trace events](./0032-plan-reason-events.md)
- [ADR-033: Exponential backoff on tool-call retries](./0033-tool-retry-backoff.md)
- [ADR-034: Mid-batch tool-result checkpointing](./0034-mid-batch-checkpoint.md)
- [ADR-035: Git worktree per session](./035-git-worktree-per-session.md)
- [ADR-036: Docker execution mode](./036-docker-execution-mode.md)
- [ADR-037: Repo-graph context retrieval (prototype)](./037-repo-graph-context-retrieval.md)
- [ADR-038: Task-benchmark harness](./038-task-benchmark-harness.md)
- [ADR-039: Execution replay + time-travel](./039-execution-replay.md)
- [ADR-040: VS Code extension full surface](./040-vscode-extension-full-surface.md)
- [ADR-041: Subagent model selection](./041-subagent-model-selection.md)
- [ADR-042: OpenCode Zen provider](./042-opencode-zen-provider.md)
- [ADR-043: Verifier-bus bridge code](./043-verifier-bus-bridge-code.md)

These are **not** part of the Plugin3 series and are therefore not
included in the 4-digit index table above.

## Cross-references

ADRs that reuse a Stratum design cite the Stratum ADR by number
rather than re-deriving it. The shared trait shapes (OffloadStore,
compression pipeline, layered content detection) are documented
once in the Stratum ADRs and inherited here.

## Reading order

Newcomers should read 0001 → 0002 → 0003 → 0005 → 0006 → 0009 →
0013 → 0015 in that order. The other ADRs are reference material.