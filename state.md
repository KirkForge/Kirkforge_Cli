# kf-code Repo State

*Current-state-only. Resolved-issue archaeology lives in `git log`.*

## Branch

**`wo/21-integrate`** at commit `6d6c28e`. WO 21 + WO 22 series in progress. ~50 commits ahead of `origin/dev`.

## WO 22 series — in progress

Workorders 22.0–22.12 defined. Critical fixes and over-engineering cleanup underway.

### Per-workorder status (verified against code)

| WO | Done | Partial | Deferred | Tracking |
|----|------|---------|----------|----------|
| 22.1 critical-landlock | R1 (ABI rewrite, correct struct layout, access flags, /dev//proc allow-list) | — | — | DONE |
| 22.2 critical-plugin-defaults | R1 (stratum + kf-budget in default_enabled_plugins) | — | — | DONE (in prior session) |
| 22.3 mcp-resource-security | R1 (URI validation, capabilities advertised) | — | — | DONE (in prior session) |
| 22.4 memory-hardening | R1 (MAX_FACTS_PER_TURN=3, EXTRACT_EVERY_N_TURNS=3, FNV hash slugs, is_preference_like) | — | — | DONE |
| 22.5 tui-broken-pipelines | R2 (sessions_dirty refresh) | R1(F2-F5 Enter), R3(GitOperationEvent), R4(file-tool duration) | — | PONYTAIL-DEBT |
| 22.6 architecture-smells | R1(token estimation unified), R5(CorrectionResult file/line) | — | R2(offload store scoping), R3(AppState decomposition), R4(hook return type), R6(compaction tail) | PONYTAIL-DEBT |
| 22.7 over-engineering | R1(kf-budget-hosts deleted), R2(ToolChoice kept — not over-engineering), R3(TruncationStrategy deleted), R4(aws_profile deleted), R5(CompactionTransform collapsed), R6(ADR-049 flipped) | — | — | DONE |
| 22.8 doc-overhaul | R1-R18 (all doc fixes from review Part 8) | — | — | DONE |
| 22.9 adapter-gaps | R1(moot), R3(persona.rs — not a bug) | — | R2(JSON-schema), R4(Bedrock/Vertex test), R6(keychain — depends on R1), R7(ADR-070 written), R8(ADR-070 written) | PONYTAIL-DEBT |
| 22.10 design-flaw-fixes | R5(format_verdict_report documented) | — | R1(verifier skip→CorrectionResult), R2(=22.6-R2), R3(=22.7-R3), R4(=22.7-R5) | PONYTAIL-DEBT |
| 22.11 verifier-hardening | R1(sync catch_unwind), R2(async catch_unwind), R3(Skipped verdict), R4(format_verdict_report pub(crate)) | — | — | DONE |
| 22.12 adr-drift | R1(~30 ADRs updated) | — | — | DONE |

### Key commits this session (WO 22)

- `1b72e69` fix(22.1): landlock ABI rewrite
- `af7c1f6` fix(22.4+22.11): memory hardening + verifier catch_unwind
- `4791f00` docs: WO 22 series workorders
- `d21e450` chore(22.7-R1): delete kf-budget-hosts crate
- `ec28bc6` refactor(22.7-R3): remove TruncationStrategy enum
- `76fbefc` refactor(22.7-R4): remove aws_profile config field
- `53ae5bc` refactor(22.7-R5): collapse CompactionTransform trait
- `3e3690d` feat(22.6-R5): add file/line fields to CorrectionResult
- `f049aef` docs(22.9-R7/R8): ADR-070 adapter-gap decisions
- `6d6c28e` style: cargo fmt after WO 22 merges

## WO 21 series — completed (prior session)

All `wo/21.*` topic branches merged. See git log for details.

## Remaining deferred items (all tracked in PONYTAIL-DEBT.md)

### High priority (security/correctness)

1. **21.7-R1**: Landlock — DONE in WO 22.1
2. **21.7-R3**: Diff-review-before-apply

### Medium priority (functionality gaps)

3. **21.6-R1**: LSP federation
4. **21.5-R2**: Bash PTY/streaming
5. **22.5-R1**: F2-F5 TUI Enter handlers
6. **22.6-R2**: Per-session offload store (OnceLock → session-scoped + LRU cap)

### Low priority (defer with reason)

7. **21.7-R2**: seccomp syscall filter
8. **22.6-R3**: AppState decomposition (~55 fields → sub-structs)
9. **22.6-R4**: Hook return type split (post-hooks don't need HookDecision)
10. **22.9-R2**: JSON-schema structured output
11. **22.9-R4**: Bedrock/Vertex test hardening

## Gate status

- `cargo check --workspace`: PASS
- `cargo fmt --check`: PASS
- `cargo clippy --workspace -- -D warnings`: PASS
- `cargo test -p kf-budget-core --test adr_xref_drift`: PASS
- HEAD: `6d6c28e`

## Known pre-existing test failures (NOT from WO 21/22)

- `compaction_use_llm_alias_backward_compat` — test expects `compaction_use_llm` under `[session]` TOML but parser only reads it from top level
- `bundled_node_sdk_tool_executes_via_host` (requires Node.js)
- `adapters::m5_tests::openai_cache_mode_marks_last_two_prefix_messages` — stale vs WO 17.5
- `session::plugin_tools::*` (7), `tools::bash::*` (2) — env/binary-dependent

## Rust toolchain

Rust 1.88.0 at `~/.cargo/bin/`. Run `export PATH="$HOME/.cargo/bin:$PATH"` before cargo commands.
