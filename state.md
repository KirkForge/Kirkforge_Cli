# kf-code Repo State

*Current-state-only. Resolved-issue archaeology lives in `git log`.*

## Branch

**`wo/21-integrate`** at commit `d8c7240`. WO 21 + WO 22 series nearly complete. ~70 commits ahead of `origin/dev`.

## WO 22 series — nearly complete

All critical fixes done. Remaining items are explicitly deferred below.

### Per-workorder status (verified against code)

| WO | Done | Partial | Deferred | Tracking |
|----|------|---------|----------|----------|
| 22.1 | R1 (landlock ABI rewrite) | — | — | DONE |
| 22.2 | R1 (default plugins: stratum+kf-budget) | — | — | DONE |
| 22.3 | R1 (MCP URI validation), R2 (capabilities handshake) | — | — | DONE |
| 22.4 | R1 (MAX_FACTS=3, FNV hash, rate limit) | — | R2(TUI visibility), R3(config flag), R4(cross-turn dedup) | deferred |
| 22.5 | R1(F2-F5 Enter handlers), R2(jobs_dirty refresh) | — | R3(GitOperationEvent documented), R4(file-tool duration) | deferred |
| 22.6 | R1(token estimation), R2(offload store per-session), R3(SearchState extraction), R4(PostHook split), R5(CorrectionResult file/line) | — | R6(compaction tail pinning) | deferred |
| 22.7 | R1-R6 (all done) | — | — | DONE |
| 22.8 | R1-R18 (all doc fixes) | — | — | DONE |
| 22.9 | R1(moot), R3(not a bug), R5(done=22.7-R4), R6(done=22.7-R1), R7(ADR-070), R8(ADR-070) | — | R2(JSON-schema), R4(Bedrock/Vertex tests) | deferred |
| 22.10 | R1(Skipped→CorrectionResult), R4(=22.7-R5 done), R5(format_verdict_report documented) | — | R2(=22.6-R2 done), R3(=22.7-R3 done) | DONE |
| 22.11 | R1-R4 (catch_unwind, Skipped, pub(crate)) | — | — | DONE |
| 22.12 | R1 (28 ADRs updated, path literals fixed) | — | — | DONE |
| 21.11-R7 | ADR-057 "unchanged" claim fixed | — | — | DONE |

### Key commits this session (WO 22)

- `1b72e69` fix(22.1): landlock ABI rewrite
- `af7c1f6` fix(22.4+22.11): memory hardening + verifier catch_unwind
- `4791f00` docs: WO 22 series workorders
- `d21e450` chore(22.7-R1): delete kf-budget-hosts
- `ec28bc6` refactor(22.7-R3): remove TruncationStrategy
- `76fbefc` refactor(22.7-R4): remove aws_profile
- `53ae5bc` refactor(22.7-R5): collapse CompactionTransform
- `3e3690d` feat(22.6-R5): CorrectionResult file/line
- `f049aef` docs(22.9-R7/R8): ADR-070
- `d4e7b61` fix(22.2+22.7-R3+R6): default plugins + TruncationStrategy + ADR-049
- `d109f54` fix(22.5): jobs_dirty refresh + F2-F5 Enter handlers
- `b9c9923` fix(22.10-R1): verifier Skipped → CorrectionResult
- `39f85ac` docs(21.11-R7): ADR-057 contract fix
- `5e9a5dd` refactor(22.6-R2/R3/R4): per-session offload + SearchState + PostHook
- `d8c7240` fix(22.6-R4): budget hook test assertions
- `de4b5ee` fix(22.3): MCP URI validation + capabilities
- `ce3518b` fix(22.12): ADR path literal drift

## Remaining deferred items

### High priority

1. **21.7-R3**: Diff-review-before-apply

### Medium priority

2. **21.6-R1**: LSP federation
3. **21.5-R2**: Bash PTY/streaming
4. **22.4-R2/R3**: TUI memory visibility + config flag
5. **22.6-R6**: Verifier findings in compaction tail
6. **22.9-R2**: JSON-schema structured output

### Low priority

7. **21.7-R2**: seccomp syscall filter
8. **22.5-R3/R4**: GitOperationEvent delete/wire + file-tool duration
9. **22.9-R4**: Bedrock/Vertex test hardening

## Gate status

- `cargo check --workspace`: PASS
- `cargo fmt --check`: PASS
- `cargo clippy --workspace -- -D warnings`: PASS
- `cargo test -p kf-budget-core --test adr_xref_drift`: PASS
- HEAD: `d8c7240`

## Known pre-existing test failures (NOT from WO 21/22)

- `compaction_use_llm_alias_backward_compat` — test expects `compaction_use_llm` under `[session]` TOML but parser only reads it from top level
- `bundled_node_sdk_tool_executes_via_host` (requires Node.js)
- `adapters::m5_tests::openai_cache_mode_marks_last_two_prefix_messages` — stale vs WO 17.5
- `session::plugin_tools::*` (7), `tools::bash::*` (2) — env/binary-dependent

## Rust toolchain

Rust 1.88.0 at `~/.cargo/bin/`. Run `export PATH="$HOME/.cargo/bin:$PATH"` before cargo commands.
