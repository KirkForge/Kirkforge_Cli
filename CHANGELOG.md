# Changelog

All notable changes to kf-code are documented here.
Format: [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

Each entry links to its workorder for full details. WO files live in
`docs/workorders/` and are the single source of truth for what changed,
why, and the gate evidence.

## [Unreleased]

### Changed

- WO 43.0-43.17 — serialized the honest-assessment backlog into 17 verified
  workorders (6 analysis agents; ~11 stale claims corrected in-line).
- WO 43.18-43.24 — round-3 fresh segment audit: 7 workorders of NEW findings
  (concurrency/shutdown, TUI, deps/size, persistence, adapters, subprocess,
  test quality).
- WO 43.25-43.39 — round-4 full-coverage segment sweep: 15 workorders, 24 NEW
  findings (UTF-8 slice panics, prompt-compaction OOB, background-bash secret
  scrub, dead env-override layer, context-index retrieval smear, file-tool
  hook veto, unguarded subprocesses, crates bugs).
- WO 43.28 — applied `scrub_secrets_from_child_env` to background/scheduled
  (`BashJobRegistry::spawn`) and PTY (`pty::run_with_pty`) bash spawn paths;
  the foreground-only scrub leaked provider secrets to the model via
  `bash(background=true)` + `bash_status`. Added `pub(crate)` helper + pinning
  test.
- WO 43.38: spawn_blocking for glob walks, tokio::time::sleep for
  computer_use wait, glob-metacharacter redirection gate — [43.38](docs/workorders/43.38-async-blocking-glob-sleep-redirection.md)
- WO 43.18-43.24 — fresh segment audit round: abrupt-exit safety, TUI
  hardening, dep/size audit, persistence crash-robustness, adapter
  transport, subprocess lifecycle, test quality (7 analysis agents).
- docs: update stale WO statuses — 14 WOs marked Done (shipped but never updated): 14.6, 27.0-27.7, 28.6, 31.0, 32.3, 32.4, 33.0, 33.4, 33.9
### Performance
- WO 38.9/42.6 items 4-6: memory mtime cache, CachedIndex embeddings in query path, prompt stem stability — [38.9](docs/workorders/38.9-session-performance.md), [42.6](docs/workorders/42.6-performance-items.md)

### Fixed
- WO 43.25: char-boundary-safe truncation at 3 byte-slice panic sites (loop_.rs:72, stream.rs:111, events.rs:477) — non-ASCII Error text / file content / PTY output with a multibyte char straddling the cut point no longer panics the turn (two sites were outside catch_unwind) — [43.25](docs/workorders/43.25-utf8-byte-slice-panic-hardening.md)
- WO 43.26: guard workflow bash steps + plugin-bus verifier subprocesses — workflow `run_bash`/`run_batch` Bash arm now set `kill_on_drop` + 30s step timeout + cancel-token select; plugin-bus verifier timeout (WO 38.3 watchdog) pinned by a bus-wrapper test — [43.26](docs/workorders/43.26-unguarded-subprocess-workflow-pluginbus.md)
- WO 43.27: preserve target permissions on atomic write (Unix mode copy before rename) + fix notebook_edit undo ordering (push after write succeeds) — [43.27](docs/workorders/43.27-atomic-write-permissions-undo-order.md)
- WO 43.30: pre-tool hook veto now blocks file tools before mutation — file tools (read_file/write_file/edit_file) short-circuited to Spawn in pre_run before the hook block, so a hook `deny` fired after the write was already applied; now file tools run the same hook gate as non-file tools (path resolved first, then hook, then spawn) — [43.30](docs/workorders/43.30-file-tool-hook-veto.md)
- WO 43.31: doom-loop banner now highlights the user's actual arrow-key selection (was hardcoded to index 0 / "Break"); Enter now matches the highlighted action — [43.31](docs/workorders/43.31-doom-loop-banner-selection.md)
- WO 43.32: wire `apply_env_overrides` into production `load_config` — `KF_CODE_*` env vars were documented as layer-2 overrides but only applied under `#[cfg(test)]`; the shipped binary silently ignored them — [43.32](docs/workorders/43.32-config-env-override-layer-dead.md)
- WO 43.33: jobd `--stop` sends auth token, validates response, 30s timeout, conditional pid removal — `kf-code jobd --stop` was rejected by auth-enabled daemons, returned Ok on Error, hung on unresponsive daemons, and deleted a live daemon's pid file; now reads the token, bails on Error/Busy/timeout, and only removes the pid on confirmed stop — [43.33](docs/workorders/43.33-jobd-stop-auth-timeout.md)
- WO 43.34: `retrieve()` no longer smears unresolved import edges into all matched symbols — filter now `resolved_file == Some(sym.file)`, matching the already-fixed `to_retrieval_result`; fixes multi-MB system-prompt inflation (7MB / >1M tokens observed) — [43.34](docs/workorders/43.34-context-index-retrieve-smear.md)
- WO 43.36: kf-compress-core Lite mode now applies transforms (was a no-op identical to Off) — `Pipeline::run` gate short-circuited on `!offloads_bloat()` before the transform loop; restructured to independent gates — [43.36](docs/workorders/43.36-compress-core-lite-noop.md)
- WO 43.37: remove dead `pending_corrections` queue (write-only state; correction loop applies fixes directly) + fix MCP `stdio_send_request` `Ok(Err(_))` branch leaking pending-map entry on channel-close — [43.37](docs/workorders/43.37-verifier-corrections-queue-mcp-leak.md)
- WO 42.11 / WO 42.6 item 2: wire content_hash into verifier path — verdict cache keyed by (file, content_hash); skip re-running cargo build/clippy/test for unchanged file content across correction iterations — [42.11](docs/workorders/42.11-content-hash-wiring.md)
- WO 42.5: MCP .mcp.json content-based re-approval — approvals now store a sha256 content hash; a modified `.mcp.json` under an approved path re-gates — [42.5](docs/workorders/42.5-mcp-content-approval.md)
- WO 42.7: offload store FIFO eviction + byte cap — replace random-order HashMap eviction with insertion-ordered VecDeque, add `max_bytes` cap — [42.7](docs/workorders/42.7-offload-fifo.md)
- WO 41.1: apply coder patch to parent + rename to PipelineOrchestrator — [41.1](docs/workorders/41.1-patch-application.md)
- WO 41.2: escape handoff delimiters in content to prevent fence spoofing — [41.2](docs/workorders/41.2-handoff-delimiter-spoof.md)
- WO 41.3: permission docs fix + ADR audit — accurate permission engine description, stale ADR amendments — [41.3](docs/workorders/41.3-permission-docs.md)
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
- WO 40.4: sleep elimination — all gratuitous test-sync sleeps eliminated; 2 residual sleeps documented (mock behavior + unconvertible race) — [40.4](docs/workorders/40.4-sleep-elimination.md)
- WO 42.1: delete dead testdoctor test referencing deleted ci.yml — [42.1](docs/workorders/42.1-dead-testdoctor.md)
- WO 42.2: audit chain resumes on restart + `FileAuditSink::verify_chain` — [42.2](docs/workorders/42.2-audit-chain-verify.md)
- WO 43.29: guard OOB write in `minify_old_messages` on over-budget path — index-out-of-bounds panic when microcompaction collapsed the middle and the compacted form still exceeded budget — [43.29](docs/workorders/43.29-prompt-compaction-oob-panic.md)
- WO 43.35: memory store stale-lock recovery via PID-liveness check — crashed process's `.lock` file is reclaimed (dead PID removal + age fallback) instead of permanently latching the store — [43.35](docs/workorders/43.35-memory-store-stale-lock.md)


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
- WO 42.3: secret scrubbing expanded — AWS creds, passwords, private keys, conn strings — [42.3](docs/workorders/42.3-secret-scrubbing.md)
- WO 42.4: rlimits/unshare fail-closed when `--harden` is set — [42.4](docs/workorders/42.4-rlimits-fail-closed.md)
- WO 32.15: landlock FS confinement integration test — verifies real bash job confined by landlock cannot read outside the sandbox — [32.15](docs/workorders/32.15-landlock-fs-confinement-test.md)



### Changed
- WO 40.4: test sleeps → structural sync (10 tests: 8 in commit 941377a3 + 2 in this commit) — [40.4](docs/workorders/40.4-sleep-elimination.md)

### Performance
- WO 42.12: populate `Message.token_count` at append time — estimators use cached value, eliminating redundant full-history BPE passes — [42.12](docs/workorders/42.12-token-count-cache.md)

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










