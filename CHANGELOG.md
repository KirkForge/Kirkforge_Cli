# Changelog

All notable changes to kf-code are documented here.
Format: [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

Each entry links to its workorder for full details. WO files live in
`docs/workorders/` and are the single source of truth for what changed,
why, and the gate evidence.

## [Unreleased]

### Fixed
- WO 38.1: security chokepoints — [38.1](docs/workorders/38.1-security-chokepoints.md)
- WO 38.2: panic containment + terminal survival — [38.2](docs/workorders/38.2-panic-containment.md)
- WO 38.3: liveness — unblock TUI event loop, bound subprocesses — [38.3](docs/workorders/38.3-liveness.md)
- WO 38.4: orchestration correctness — real parallel cancel, collision-proof ids, fenced handoffs — [38.4](docs/workorders/38.4-orchestration-correctness.md)
- WO 38.5: adapter robustness — session survives turn errors, Anthropic usage capture, pricing, truncation — [38.5](docs/workorders/38.5-adapter-robustness.md)
- WO 38.6: crash-corruption recovery — heal torn logs, surface StartedEmpty, atomic config save — [38.6](docs/workorders/38.6-crash-recovery.md)
- WO 38.7: resource lifecycle — worktree drop fallback + boot sweep, MCP kill_on_drop, tmp cleanup — [38.7](docs/workorders/38.7-resource-lifecycle.md)
- WO 38.10: CLI first-run — banner to stderr, empty-model guard, `-p` flag, exit codes — [38.10](docs/workorders/38.10-cli-first-run.md)
- WO 38.11: TUI state hygiene — cache invalidation, bounded buffers, z-order fix — [38.11](docs/workorders/38.11-tui-state-hygiene.md)
- WO 38.12: test hygiene — kill wall-clock margins, gated-start cancel tests, un-ignore job-cancel — [38.12](docs/workorders/38.12-test-hygiene.md)
- WO 38.13: docs truth pass — fix overclaims, reconcile WO statuses, drift test, config example — [38.13](docs/workorders/38.13-docs-truth-pass.md)
- WO 39.1: bench suite repair — load fix, free-win verifies, export-tasks, script fix — [39.1](docs/workorders/39.1-bench-cross-tool.md)
- WO 40.2: Windows cross-compile gate in ci-local.sh — [40.2](docs/workorders/40.2-windows-cross-compile-gate.md)
- WO 40.4: sleep elimination — 8 test-sync sleeps converted — [40.4](docs/workorders/40.4-sleep-elimination.md)

### Added
- WO 35.1: real scout→coder→reviewer pipeline — [35.1](docs/workorders/35.1-pipeline-semantics.md)
- WO 35.2: per-subagent worktree isolation + patch return — [35.2](docs/workorders/35.2-subagent-worktrees.md)
- WO 35.3: cooperative subagent cancellation — [35.3](docs/workorders/35.3-cooperative-cancellation.md)
- WO 35.4: sandbox posture indicator — [35.4](docs/workorders/35.4-sandbox-posture-indicator.md)
- WO 35.5: cross-component integration tests — [35.5](docs/workorders/35.5-cross-component-integration-tests.md)
- WO 35.6: ModelClient for kf-orchestrator — [35.6](docs/workorders/35.6-orchestrator-modelclient-wiring.md)
- WO 35.7: version-badge consistency gate — [35.7](docs/workorders/35.7-version-badge-gate.md)
- WO 36.1: rusqlite binary-size measurement — [36.1](docs/workorders/36.1-binary-size-rusqlite.md)
- WO 36.2: bash-job owner tracking + cancel-by-owner — [36.2](docs/workorders/36.2-bash-job-owner-tracking.md)
- WO 36.3: abort model streams on cancel — [36.3](docs/workorders/36.3-stream-abort.md)
- WO 36.4: live cancel token for parent session — [36.4](docs/workorders/36.4-parent-cancel-token.md)
- WO 36.5: production-wire ModelClient seam — [36.5](docs/workorders/36.5-modelclient-production-wiring.md)
- WO 36.6: EventSink → EventBus bridge — [36.6](docs/workorders/36.6-eventsink-bridge.md)
- WO 37.1: registry hardening — global task ids, bounded remove, no phantom jobs — [37.1](docs/workorders/37.1-registry-hardening.md)
- WO 37.2: the reducer — DelegationResult.packet is real — [37.2](docs/workorders/37.2-reducer.md)
- WO 38.8: wire budget guard into production — [38.8](docs/workorders/38.8-budget-guard-wiring.md)
- WO 38.9: long-session performance — kill O(N²) checkpoints, coalesce verifiers, cache token counts — [38.9](docs/workorders/38.9-session-performance.md)
- WO 39.2: Claude compat phase 1 — skill trigger derivation, commands loader, .mcp.json discovery — [39.2](docs/workorders/39.2-claude-compat-phase1.md)
- WO 39.3: Claude compat phase 2 — dynamic agents, tool-name aliases — [39.3](docs/workorders/39.3-claude-compat-phase2.md)
- WO 40.1: CI workflow architecture reset — [40.1](docs/workorders/40.1-ci-workflow-reset.md)
- WO 40.3: nextest profiles + timeout policy — [40.3](docs/workorders/40.3-nextest-profiles.md)
- WO 40.5: global state isolation — CwdGuard, env injection, data dir override — [40.5](docs/workorders/40.5-global-state-isolation.md)
- WO 41.7: property/fuzz testing of glob_matcher and command evaluation — [41.7](docs/workorders/41.7-glob-fuzz.md)

### Changed
- WO 40.4: test sleeps → structural sync (8 tests) — [40.4](docs/workorders/40.4-sleep-elimination.md)

## [0.3.10] - 2026-08-16

Release prep — version bump only. WO 33-34 series highlights:

### Fixed
- Windows stdin detach (P0 CI hang), kf-budget-core env-guard race.

### Changed
- TUI IA reset (WO 34.1-34.10): command palette, /help overlay, welcome screen, tab rewrites, action-first approval dialog, simplified status bar.
- CI architecture reset (ADR-074): split ci.yml into ci-pr/ci-merge/ci-nightly.
- Test optimization (WO 33.14/33.16): CommandRunner trait, EnvGuard, event-driven sync.
- Path-aware changed-package test selection (WO 33.6).

### Added
- GitHub Discussions enabled.
- WO 32.16-32.20: Windows stub tests, computer_use beta, security emitter, multi-language verifiers, parallel orchestration, self-update.