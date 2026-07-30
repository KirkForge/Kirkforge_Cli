# Architecture Decision Records

This directory holds the ADRs (Architecture Decision Records) for KirkForge-Cli.
Each ADR captures a single load-bearing design decision: the context that
forced it, the options considered, and the consequences. Once an ADR is
Accepted it is treated as pinned — superseding it requires a new ADR that
explicitly references the one it replaces.

Two series live side by side in this directory:

- **Plugin3** — 4-digit numbers (`0001`–`0047`). Output-side sibling of Stratum:
  slicing, compaction, token budget, hooks, cost reporting, and the Plugin3
  fold-in. Originally a vendored design doc set; now part of the core.
- **CLI** — 3-digit numbers (`001`–`050`). Native KirkForge-Cli ADRs covering
  the TUI, model adapters, tool dispatch, event bus, verifier slots, plugin
  system, LSP, VS Code bridge, and the Stratum/Draw/Video fold-ins.

The Index table below lists every ADR across both series, sorted by ADR
number (ascending). The `adr_xref_drift` test in `crates/plugin3-core`
verifies the table and the file headers agree.

## Index

| ADR | Title | Status | Series |
|-----|-------|--------|--------|
| [0001](./0001-purpose.md) | Purpose — output-side sibling of Stratum | Accepted | Plugin3 |
| [001](./001-native-ollama-cli-in-rust.md) | Native Ollama CLI in Rust | Accepted | CLI |
| [0002](./0002-workspace.md) | Workspace layout | Accepted | Plugin3 |
| [002](./002-tui-framework-and-rendering.md) | TUI Framework and Rendering | Accepted | CLI |
| [0003](./0003-output-split.md) | SlicingTransform + CompactionTransform | Accepted | Plugin3 |
| [003](./003-model-abstraction-layer.md) | Model Abstraction — Single Stream, Per-Model Adapters | Accepted | CLI |
| [0004](./0004-offload-store.md) | OffloadStore reuse from Stratum | Accepted | Plugin3 |
| [004](./004-tool-use-and-execution-sandbox.md) | Tool Use — Client-Side Tool Dispatch with Approval Gates | Accepted | CLI |
| [0005](./0005-token-budget.md) | Three-state token budget guard | Accepted | Plugin3 |
| [005](./005-session-management-and-prompt-construction.md) | Session Management and Prompt Construction | Accepted | CLI |
| [0006](./0006-tool-output-detector.md) | Tool output detection | Accepted | Plugin3 |
| [006](./006-event-bus.md) | Event Bus — Pub/Sub Dispatcher for Tool Execution Events | Accepted | CLI |
| [0007](./0007-slicing-orchestrator.md) | Parallel slicing orchestrator | Accepted | Plugin3 |
| [007](./007-verifier-slots-and-correction-loop.md) | Verifier Slots and Correction Loop | Accepted | CLI |
| [0008](./0008-compaction-strategy.md) | Conversation-length compaction strategy | Accepted | Plugin3 |
| [008](./008-session-daemon.md) | Session Daemon — Metadata-Only Background Process for Fast Resume | Accepted | CLI |
| [0009](./0009-hooks-model.md) | Hook surface — PostToolUse, UserPromptSubmit, PreCompact | Accepted | Plugin3 |
| [009](./009-enforced-plan-mode.md) | Enforced Plan Mode — Read-Only Discovery Before Implementation | Accepted | CLI |
| [0010](./0010-cost-reporting.md) | Cost reporting — usage.jsonl + report subcommand | Accepted | Plugin3 |
| [010](./010-subagent-personas.md) | Built-In Subagent Personas — Fork-Isolated `/explore`, `/plan`, `/coder` | Accepted | CLI |
| [0011](./0011-persistent-knowledge.md) | Persistent knowledge — saved findings (REJECTED) | Rejected (2026-07-17) | Plugin3 |
| [011](./011-compaction-hooks.md) | Tail-Preserving Compaction with pre-compact / post-compact Hooks | Accepted | CLI |
| [0012](./0012-speculative-priming.md) | Speculative priming — prediction pipeline (REJECTED) | Rejected (2026-07-17) | Plugin3 |
| [012](./012-git-commit-sanitation.md) | Safe Git Commit Helper — `/commit` with Pre-Commit Sanitation | Accepted | CLI |
| [0013](./0013-output-shim.md) | Output shim — per-host payload translation | Accepted | Plugin3 |
| [013](./013-rust-native-plugin-system.md) | Rust-Native Plugin System | Accepted | CLI |
| [0014](./0014-state-management.md) | State management — XDG dirs, atomic flag file | Accepted | Plugin3 |
| [014](./014-background-bash-jobs.md) | Background Bash Jobs | Accepted | CLI |
| [0015](./0015-cli-design.md) | CLI design — clap-derive, env override precedence | Accepted | Plugin3 |
| [015](./015-undo-stack.md) | Per-Session Undo Stack for File Edits | Accepted | CLI |
| [0016](./0016-test-strategy.md) | Test strategy — parity, drift, property, golden | Accepted | Plugin3 |
| [016](./016-save-transcript.md) | `/save` Conversation Transcript | Accepted | CLI |
| [0017](./0017-build-features.md) | Build profile and feature gating discipline | Accepted | Plugin3 |
| [017](./017-plugin-api-version.md) | Plugin API version contract | Accepted | CLI |
| [0018](./0018-scheduled-jobs.md) | Cron / scheduled jobs | Accepted | Plugin3 |
| [018](./018-lsp-integration.md) | LSP integration — kirkforge-lsp crate + lsp_query tool | Accepted (2026-07-19) | CLI |
| [019](./019-vscode-extension.md) | VS Code extension — Option A PTY wrapper MVP | Accepted (2026-07-19) | CLI |
| [0020](./0020-parallel-tool-dispatch.md) | Parallel Tool Dispatch | Accepted | Plugin3 |
| [0021](./0021-computer-use-tool.md) | `computer_use` tool via headless Chrome CDP | Accepted | Plugin3 |
| [0022](./0022-anthropic-cloud-routing.md) | Anthropic cloud routing — Bedrock and Vertex | Accepted | Plugin3 |
| [0023](./0023-workflow-engine.md) | Programmable JSON Workflow Engine | Accepted | Plugin3 |
| [024](./024-release-cadence.md) | Release cadence and semantic versioning | Accepted | CLI |
| [025](./025-windows-parity.md) | Windows parity approach | Accepted (fully implemented) | CLI |
| [026](./026-vscode-ndjson-bridge.md) | VS Code NDJSON Bridge (Option B) | Accepted | CLI |
| [0027](./0027-context-management-depth.md) | Context management depth — cache-stem reuse, microcompaction, and tool-result truncation | Accepted | Plugin3 |
| [0028](./0028-verifier-bus-unification.md) | Unify the Rust and TypeScript verifier buses | Accepted | Plugin3 |
| [0029](./0029-test-partitioning.md) | Test partitioning — fast / full / coverage suites | Accepted (per-test timings + flaky-test detection shipped, WO 12.5) | Plugin3 |
| [030](./030-deterministic-mode.md) | `--seed` deterministic mode | Accepted (2026-07-21) | CLI |
| [0031](./0031-build-test-verifier-slots.md) | Build and Test Verifier Slots | Accepted | Plugin3 |
| [0032](./0032-plan-reason-events.md) | PlanReason trace events | Accepted | Plugin3 |
| [0033](./0033-tool-retry-backoff.md) | Exponential Backoff on Tool-Call Retries | Accepted | Plugin3 |
| [0034](./0034-mid-batch-checkpoint.md) | Mid-batch tool-result checkpointing | Accepted | Plugin3 |
| [035](./035-git-worktree-per-session.md) | Git Worktree Per Session | Accepted (2026-07-21) | CLI |
| [036](./036-docker-execution-mode.md) | Docker Execution Mode | Accepted (2026-07-21) | CLI |
| [037](./037-repo-graph-context-retrieval.md) | Repo-Graph Context Retrieval | Accepted (2026-07-21) | CLI |
| [038](./038-task-benchmark-harness.md) | Task-benchmark harness | Accepted | CLI |
| [039](./039-execution-replay.md) | Execution replay + time-travel | Accepted | CLI |
| [040](./040-vscode-extension-full-surface.md) | VS Code extension full surface | Accepted | CLI |
| [041](./041-subagent-model-selection.md) | Subagent model selection | Accepted | CLI |
| [042](./042-opencode-zen-provider.md) | OpenCode Zen provider | Accepted | CLI |
| [043](./043-verifier-bus-bridge-code.md) | Verifier-bus bridge code | Accepted | CLI |
| [0044](./0044-computer-use-depth.md) | Computer-use depth (multi-step browser flows) | Accepted (partially implemented) | Plugin3 |
| [045](./045-continuous-eval-pipeline.md) | Continuous Evaluation Pipeline | Accepted | CLI |
| [046](./046-stratum-fold-in.md) | Fold Stratum into Core | Accepted | CLI |
| [0047](./0047-plugin3-fold-in.md) | Fold Plugin3 into Core | Accepted | Plugin3 |
| [048](./048-draw-fold-in.md) | Draw Fold-In | Accepted | CLI |
| [049](./049-video-fold-in.md) | Video Fold-In (Non-Default Feature) | Accepted | CLI |
| [050](./050-plugin-system-consolidation.md) | Plugin System Consolidation | Accepted | CLI |
| [051](./051-stratum-budget-coordination.md) | Stratum–Budget Coordination (slicing triggers compression, budget tracks compressed size) | Accepted | CLI |
| [052](./052-cache-stem-reuse.md) | Client-side prompt cache stem reuse | Accepted | CLI |
| [053](./053-vfs-minification.md) | VFS minification for the agent loop `read_file` tool | Accepted | CLI |
| [054](./054-rlimit-sandbox-hardening.md) | rlimit sandbox hardening for the non-Docker bash path | Accepted | CLI |
| [055](./055-http-mcp-session-id.md) | HTTP MCP session-id tracking + resumable streams | Accepted | CLI |
| [056](./056-plugin-cli-subcommand.md) | Shared plugin-ops layer and `kirkforge plugin` CLI subcommand | Accepted | CLI |
| [057](./057-plugin-signature-rust.md) | In-process plugin signature verification (no minisign shell-out) | Accepted | CLI |
| [058](./058-plugin-depends-on.md) | Plugin manifest `depends_on` (dependency graph + topological load order) | Accepted | CLI |
| [059](./059-plugin-hot-reload.md) | Plugin hot-reload via file watcher | Accepted | CLI |
| [060](./060-per-plugin-resource-limits.md) | Per-plugin resource limits (extend SandboxConfig to plugin tools) | Accepted | CLI |
| [061](./061-plugin-hook-audit-log.md) | Plugin hook fail-open audit log | Accepted | CLI |
| [062](./062-plugin-verifier-ui.md) | Plugin verifier results in `/verify` panel + cost report | Accepted | CLI |
| [063](./063-plugin-init-scaffolding.md) | Plugin init scaffolding command (`kirkforge plugin init`) | Accepted | CLI |
| [064](./064-plugin-system-e2e-test.md) | Plugin system end-to-end integration test suite | Accepted | CLI |
| [065](./065-coverage-threshold-policy.md) | Coverage-gate threshold policy (75% target, headroom, `--skip` workaround) | Accepted | CLI |

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
- [ADR-044: Computer-use depth (multi-step browser flows)](./044-computer-use-depth.md)
- [ADR-045: Continuous evaluation pipeline](./045-continuous-eval-pipeline.md)
- [ADR-046: Fold Stratum into core](./046-stratum-fold-in.md)
- [ADR-047: Fold Plugin3 into Core](./0047-plugin3-fold-in.md)
- [ADR-048: Draw fold-in (in-process .td.json rendering)](./048-draw-fold-in.md)
- [ADR-049: Video fold-in (non-default feature)](./049-video-fold-in.md)
- [ADR-050: Plugin system consolidation](./050-plugin-system-consolidation.md)
- [ADR-051: Stratum and budget guard coordination](./051-stratum-budget-coordination.md)
- [ADR-052: Client-side prompt cache stem reuse](./052-cache-stem-reuse.md)
- [ADR-053: VFS minification for the agent loop `read_file` tool](./053-vfs-minification.md)
- [ADR-054: rlimit sandbox hardening for the non-Docker bash path](./054-rlimit-sandbox-hardening.md)
- [ADR-055: HTTP MCP session-id tracking + resumable streams](./055-http-mcp-session-id.md)

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