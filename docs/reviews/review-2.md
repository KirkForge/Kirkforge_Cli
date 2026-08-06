# KirkForge-Cli Full Codebase Review — `review-2.md`

**Date**: 2026-07-31  
**Version under review**: v0.3.6  
**Files**: ~10,124 loc executor, ~8,400 loc adapters, ~4,658 tests, 16 workspace crates, 84 ADRs

---

## 1. Executive Summary

KirkForge-Cli is a production-quality Rust CLI coding agent. The architecture is sound, test coverage is strong (4,658 tests), CI is comprehensive (9 CI jobs + release pipeline + bench workflow), and the codebase follows consistent patterns. The review found **3 high-severity**, **8 medium-severity**, and **12 low-severity** findings. No critical/blocking issues.

---

## 2. Architecture Assessment

### 2.1 Strengths

| Area | Assessment |
|---|---|
| **Provider abstraction** | `ModelAdapter` trait is clean. Six providers unified behind one interface. Model-name routing heuristics + explicit override. NDJSON and SSE stream parsers are well-factored and shared. |
| **Verification pipeline** | Dual verifier system (event-driven + bus-based) provides defense-in-depth. Correction loop auto-applies fixes up to 3 iterations. Both built-in and plugin verifiers feed the same `CorrectionResult` pipeline. |
| **Tool dispatch** | Three-phase batch dispatch (ADR-020) handles pre-gating, parallel execution, and ordered recording. Read-before-edit gate prevents stale-read conflicts. Deterministic mode for reproducibility. |
| **Plugin system** | Manifest-based with trust tiers, minisign signatures, topological load order, hot-reload, resource limits, and fail-open audit logging. Two-path dispatch: compiled-in Rust vs. shell-out. |
| **CI pipeline** | 9 quality/coverage/bench/integration/windows/node-sdk/vscode jobs. Bench regression gate at 10pp. Scheduled leaderboard. Three-threshold coverage enforcement (session 68.5%, tools 76.0%, adapters 75.0%). |

### 2.2 Architectural Concerns

| ID | Severity | Finding | Location |
|----|----------|---------|----------|
| **A1** | HIGH | **Config field drift** — `merge_toml_into_config` (~270 loc), `apply_env_overrides` (~399 loc), and `config_diff_summary` must be manually kept in sync with the `Config` struct. Adding a field but missing one of these three is a silent regression with no compile-time guard. This pattern caused drift in `plugin3-core` README test counts and is acknowledged in AGENTS.md. | `src/session/config/mod.rs`, `env_overrides.rs` |
| **A2** | MEDIUM | **Two coexisting verifier systems** — The event-driven `Verifier` trait (Path A) and the bus-based `BusVerifier` trait (Path B) run in parallel for file-modifying tools, potentially producing duplicate findings. No end-to-end test validates both systems fire without conflicting results. | `src/session/verifier/bus.rs`, `src/session/executor/dispatch.rs` |
| **A3** | MEDIUM | **AppState is a 44-field God object** — Every feature adds fields to a single struct. The `new()` constructor is 70 lines. Adding a feature requires touching the struct, its constructor, and potentially render-cache invalidation. | `src/tui/app.rs` |
| **A4** | MEDIUM | **`find_cargo_root` duplicated in 3 files** — `build.rs`, `lint.rs`, `test.rs` each have an identical copy. Tests are also duplicated. Fixing a bug requires 3 changes. | `src/session/verifier/{build,lint,test}.rs` |
| **A5** | LOW | **`event_kinds.rs` is dead code** — `verifier_event_kinds()` is exported but never called outside its own tests. The `VerifierHandler` hardcodes its own subscription list. | `src/session/verifier/event_kinds.rs` |

---

## 3. Code Quality

### 3.1 Strengths

- **Consistent patterns**: `crate::send_or_warn!` macro, `PreRunVerdict::Skip`, RAII guards (`PostTurnHookGuard`, `ApprovalResponder`, `CleanupFile`), `ToolOutcome` taxonomy.
- **Well-documented**: Extensive doc comments, dated incident annotations (SIGHUP orphan fix 2026-06-12), ADR cross-references.
- **Defense-in-depth**: Multiple security layers (path guard, deny list, bash safety check, plan mode, permission rules, approval flow, pre-tool hooks).
- **Error handling**: `anyhow::Result` propagation, timeout wrappers, cancellation awareness, structured `ToolError` variants.

### 3.2 Code Smells

| ID | Severity | Finding | Location |
|----|----------|---------|----------|
| **C1** | HIGH | **Monolithic functions** — `run_turn_inner` (224 loc), `dispatch_tool_call_batch` (285 loc), `record_tool_result` (280 loc), `run_tui` (430 loc), `run_event_loop` (310 loc). These could be decomposed into named sub-methods without changing behavior. | `src/session/executor/{turn,loop_}.rs`, `src/tui/mod.rs` |
| **C2** | MEDIUM | **Code duplication in hooks** — `run_decision` and `run_decision_with_context` share ~50 lines of identical logic (in-process hook eval, built-in hook eval, plugin hook eval, decision aggregation). | `src/session/hooks.rs` |
| **C3** | MEDIUM | **Redundant body serialization** — `build_anthropic_body()` returns `serde_json::Value`. `AnthropicBedrockAdapter` re-serializes with `to_vec` for SigV4 signing; other adapters pass to `.json(&body)` which also re-serializes. The body is identical across paths. | `src/adapters/anthropic.rs`, `anthropic_bedrock.rs` |
| **C4** | MEDIUM | **`computer_use.rs` triple lock acquisition** — The same `Mutex<Option<BrowserSession>>` is locked, checked, dropped, re-locked, and re-locked again in a single `run()` call. Between acquisitions, the boolean is stale. | `src/tools/computer_use.rs` |
| **C5** | LOW | **Constructor chain has 3 levels** — `with_log() → with_log_and_undo() → with_log_and_undo_and_plugins()` with thin convenience shims. | `src/session/executor/mod.rs` |
| **C6** | LOW | **`#[allow(dead_code)]` on `ConnectionState::Connecting`** — The variant exists but is never emitted. Either remove it or make it reachable. | `src/tui/app.rs:27` |
| **C7** | LOW | **Naive command parsing with `split_whitespace()`** — Used in `correction.rs` for formatter commands. Cannot handle arguments with spaces or quotes. Low risk since only formatter commands (no args) are split. | `src/session/verifier/correction.rs:199` |

---

## 4. Security & Correctness

### 4.1 Strengths

- **Atomic writes**: `O_EXCL` temp files with fsync before rename. PID+ns+counter naming prevents symlink attacks.
- **Path guard**: Per-tool read/write/traversal checks with deny extensions, deny paths, dotfile blocking, symlink containment, sandbox boundary, filesize caps.
- **Network safety**: URL scheme guard (http/https only), cloud-metadata deny list, literal internal-IP rejection (IPv4 + IPv6), 1 MiB response cap.
- **Bash safety**: Command deny-list check, timeout clamping (max 24h), cancellation support, optional Docker sandbox (`--network=none`), optional rlimit hardening.
- **URL validation**: Shared between `web_fetch` and `computer_use` via `host_is_literal_internal_ip()`.

### 4.2 Security Findings

| ID | Severity | Finding | Location |
|----|----------|---------|----------|
| **S1** | HIGH | **Shell pattern scanner flags documentation/comments as secrets** — Patterns like `rm -rf /` and `:(){ :\|:& };:` are substring-matched in ALL file content, including comments and docstrings. Returns `Verdict::Unfixable`, blocking the correction loop. | `src/session/verifier/security.rs:269-276` |
| **S2** | MEDIUM | **Only 9 entropy prefixes checked** — `ENTROPY_PREFIXES` covers `sk-`, `ghp_`, `AKIA`, etc. Many API key formats (`xai-`, `claude-`, `hf_`, `key-`) pass through undetected. The entropy detector is supplementary, not comprehensive. | `src/session/verifier/security.rs:41` |
| **S3** | MEDIUM | **Git worktree check flags staged files as `Unfixable`** — After `git add`, staged files appear as "dirty worktree" errors. The model can commit them, but the error presentation is misleading. | `src/session/verifier/git.rs:166-193` |
| **S4** | LOW | **DNS rebinding not addressed** — The internal-IP check only operates on literal host strings in URLs, not on DNS-resolved IPs. Documented as a known ceiling. | `src/tools/web_fetch.rs` |
| **S5** | LOW | **Stderr discarded on successful bash execution** — `cargo build` warnings and other informational stderr are invisible to the model. The build-log minifier only operates on stdout. | `src/tools/bash.rs:295-315` |
| **S6** | LOW | **LSP `path_to_uri()` doesn't percent-encode** — Paths with spaces or non-ASCII characters produce invalid URIs. | `src/tools/lsp_query.rs` |
| **S7** | LOW | **Bedrock event-stream envelope buffer lacks size cap** — Unlike the 8 MiB cap on the inner SSE/NDJSON parsers, the outer AWS event-stream parser has no size limit. | `src/adapters/anthropic_bedrock.rs` |

---

## 5. Test Coverage & Quality

### 5.1 Test Statistics

| Metric | Count |
|---|---|
| `#[test]` attributes | 3,858 (2,188 in `src/`, 1,670 in `crates/`) |
| `#[tokio::test]` attributes | 800 (757 in `src/`, 43 in `crates/`) |
| **Total tests** | **4,658** |
| Integration/smoke tests | 11 (2 smoke, 7 ignored integration, 2 mock) |
| Ignored tests | 10+ (integration, `#[ignore]` for slow/flaky) |
| CI coverage gates | session 68.5%, tools 76.0%, adapters 75.0% |

### 5.2 Test Coverage Gaps

| ID | Severity | Finding |
|----|----------|---------|
| **T1** | MEDIUM | **No end-to-end test crossing both verifier systems** — Path A (event-driven) and Path B (bus-based) are tested independently. No test validates both fire for a `write_file` event and produce non-conflicting results. |
| **T2** | MEDIUM | **All integration tests are `#[ignore]`** — CI relies on mock tests (`wiremock`) for adapter coverage. The full adapter→executor→tool pipeline is only exercised in ignored tests. |
| **T3** | MEDIUM | **Smoke tests cover only 2 CLI subcommands** — No smoke tests for `run --help`, `plugin list`, `config`, `version`, `session list`, etc. |
| **T4** | LOW | **No test for `ToolError` event through `VerifierHandler`** — Despite subscribing to `ToolError`, no test exercises this path. |
| **T5** | LOW | **No test for `CorrectionLoop` reaching max iterations** — The loop breaks at 3 iterations, but no test verifies this boundary. |
| **T6** | LOW | **No test for duplicate verifier registration on `VerifierBus`** — `VerifierBus::register` does not reject duplicates (unlike `VerifierSlots`). |
| **T7** | LOW | **No test for `PluginToolWrapper.run` `Cancelled` path** — Cancellation handling in plugin tool execution is untested. |

### 5.3 Stale Documentation

| ID | Severity | Finding |
|----|----------|---------|
| **D1** | LOW | **`plugin3-core/README.md` test count is stale** — Reports 1,649 tests under `crates/`; actual count is 1,670 (21-test drift). |

---

## 6. Performance & Resource Usage

### 6.1 Strengths

- **Release profile**: `opt-level="z"`, LTO, single codegen unit, stripped binary (~5.4 MB).
- **Response cache**: Two-tier (in-memory + disk) with content-addressed hashing. Caches complete streams only.
- **Frame-pacing v2**: TUI renders only when dirty, 8 Hz spinner updates.
- **Mid-batch checkpointing**: Tool results are persisted immediately, not at end-of-turn.
- **Channel sizing**: 4096-capacity mpsc channels (increased from 128 after a thinking-model incident).

### 6.2 Performance Findings

| ID | Severity | Finding | Location |
|----|----------|---------|----------|
| **P1** | LOW | **All 6 verifiers run on `ToolError` events then immediately skip** — `VerifierHandler` subscribes to `[Edit, FileWrite, BashExec, GitOperation, ToolError]`. On `ToolError`, all 6 verifiers fire, all return `Skipped`. 6 wasted async calls per non-relevant event. | `src/session/verifier/handler.rs` |
| **P2** | LOW | **Vertex adapter fetches token per request** — `yup-oauth2` caches internally, so no practical impact, but the token fetch is outside `send_with_retry` so a fetch failure is not retried. | `src/adapters/anthropic_vertex.rs:108` |
| **P3** | LOW | **Test verifier runs all crate tests for `main.rs`/`lib.rs` changes** — An empty module prefix means `cargo test` without filter. A failure in an unrelated test module would be attributed to the current file's verifier. | `src/session/verifier/test.rs:53-58` |

---

## 7. Dependency & Size Audit

### 7.1 Direct Dependencies (root binary)

| Dependency | Justification |
|---|---|
| `tokio` (full) | Async runtime for executor, tools, adapters |
| `ratatui` + `crossterm` | Interactive TUI |
| `reqwest` (rustls) | HTTP client for provider adapters |
| `serde` + `serde_json` | Serialization |
| `clap` (derive + env) | CLI argument parsing |
| `handlebars` | System prompt templating |
| `rustyline` | Line-mode input |
| `similar` | Diff rendering |
| `notify-debouncer-mini` | Plugin hot-reload watcher |
| `ignore` + `globset` | Gitignore-aware file search |
| `shellexpand` | Tilde/path expansion |
| `cron` | Scheduled bash jobs |
| `pulldown-cmark` | Markdown rendering |
| `arboard` | Clipboard integration |
| `headless_chrome` | `computer_use` browser tool |
| `aws-sigv4` + `aws-credential-types` + `aws-smithy-runtime-api` | Bedrock SigV4 signing |
| `yup-oauth2` | Vertex AI GCP auth |
| `sha2` + `hex` | Cache key digestion |
| `base64` | Image encoding for vision models |
| `textwrap` | Output formatting |
| **Feature-gated**: `opentelemetry` (+3 crates) | Optional OTel tracing |

### 7.2 Assessment

All dependencies earn their place. The AWS and GCP auth crates would be good candidates for feature-gating (`bedrock`, `vertex`) if binary size becomes a concern. `headless_chrome` is the heaviest dependency — it could also be feature-gated behind `computer-use`.

---

## 8. CI/CD Assessment

### 8.1 Strengths

- **9 CI jobs**: `fmt`, `changelog`, `quality` (test+clippy+build), `windows`, `integration` (Ollama), `bench` (regression gate), `coverage` (tarpaulin + Codecov), `audit` (cargo audit), plus `node-sdk` and `vscode` jobs
- **Bench regression gate**: PR bench job fails on 10pp success-rate regression
- **Bench leaderboard**: Scheduled daily multi-model leaderboard commits to `main`
- **Coverage enforcement**: Python gate on cobertura.xml with per-module thresholds
- **Release pipeline**: 6 platform targets, SHA256SUMS, cosign signing, cross-compilation via `Cross.toml`
- **Self-healing CI**: Bench baseline workflow has 3-attempt Ollama pull retry loop

### 8.2 CI Findings

| ID | Severity | Finding |
|----|----------|---------|
| **CI1** | LOW | **`ci-local.sh` does not run `adr_xref_drift` test** — AGENTS.md requires this when ADRs are touched, but the local script omits it. The test runs in CI `quality` job, so remote coverage is fine. |
| **CI2** | LOW | **`cargo test --doc` is not in `ci-local.sh`** — If doc-tests exist, they are not run locally. |
| **CI3** | LOW | **Windows `test_cache_results` mtime race** — Documented as resolved in state.md (commit `4bdc13f`, WO 14.0 follow-on). Cache is now scanned by path only. |

---

## 9. Documentation Quality

### 9.1 Strengths

- **740-line `docs/TECHNICAL.md`**: Comprehensive architecture map covering identity, layout, agent core, verification, context index, plugins, CI, and bench.
- **84 ADRs** in `docs/adr/`: Pinned architectural decisions with cross-references. Two-source-of-truth system (file headers + index table) enforced by `adr_xref_drift` test.
- **Well-documented config**: `config.toml.example` is 326 lines with every field explained.
- **Workorder system**: ~90 workorders organized by series with status tables.
- **`KIRK-BENCH.md`**: Bench spec document.
- **Runbooks**: Operability docs for quota-exceeded, docker-timeout, trust-rejection scenarios.

### 9.2 Documentation Gaps

| ID | Severity | Finding |
|----|----------|---------|
| **DOC1** | LOW | **No architecture diagram** — `docs/TECHNICAL.md` has ASCII boxes and tables but no visual dependency graph or data-flow diagram. |
| **DOC2** | LOW | **Config field documentation is manual** — The three config sync points (merge, env, diff) rely on the AGENTS.md checklist rather than code generation. |

---

## 10. Deferred / Known Issues (from state.md)

These are explicitly acknowledged as deferred, not regressions:

| Item | Status |
|---|---|
| 75% coverage on `src/session` | Deferred — measured at 68.6%, threshold set to 68.5%. Async executor + MCP-HTTP code needs integration test work. |
| `use_workflow_run` bench task | Deferred — no `Tool` impl exists for `kirkforge-workflow` in-process. |
| 11 pre-existing bench tasks fail `verify-only` | Pre-existing flaw in `file_contains` verify specs from WO 7.8. Not yet addressed. |
| `sudo -E git` not detected by git verifier | Documented `ponytail: ceiling`. |
| DNS rebinding mitigation | Documented ceiling in `web_fetch.rs`. |

---

## 11. Summary of Findings by Severity

### HIGH (3)

1. **Config field drift** — `merge_toml_into_config` / `apply_env_overrides` / `config_diff_summary` must be manually synced with `Config` struct. No compile-time guard. (A1)
2. **Shell pattern scanner false positives on documentation** — `security` verifier flags `rm -rf /` in comments/docstrings as `Unfixable`, blocking the correction loop. (S1)
3. **Monolithic functions** — 5 functions >200 loc that mix concerns and are hard to test in isolation. (C1)

### MEDIUM (8)

1. **Two coexisting verifier systems** with no end-to-end cross-test. (A2, T1)
2. **AppState God object** — 44 fields, 70-line constructor. (A3)
3. **`find_cargo_root` triplicated** across build/lint/test verifiers. (A4)
4. **Hook `run_decision` duplication** — ~50 shared lines. (C2)
5. **Triple lock on `computer_use` session** — stale boolean between acquisitions. (C4)
6. **Git worktree `Unfixable` for staged files** — misleading error presentation. (S3)
7. **Only 9 entropy prefixes** — many API key formats undetected. (S2)
8. **All integration tests `#[ignore]`** — CI relies entirely on mock tests. (T2)

### LOW (12)

1. `event_kinds.rs` dead code (A5)
2. Constructor chain 3 levels deep (C5)
3. `ConnectionState::Connecting` dead code (C6)
4. Naive `split_whitespace()` command parsing (C7)
5. DNS rebinding not addressed (S4)
6. Stderr discarded on bash success (S5)
7. LSP path URI doesn't percent-encode (S6)
8. Bedrock envelope buffer lacks size cap (S7)
9. No test for `ToolError` through `VerifierHandler` (T4)
10. No test for `CorrectionLoop` max iterations (T5)
11. No test for `PluginToolWrapper` cancellation (T7)
12. `plugin3-core/README.md` test count stale (D1)

---

## 12. Recommendations

### Immediate (this version)

1. **Add a compile-time test** that verifies every `Config` field appears in all three sync points (`merge_toml_into_config`, `apply_env_overrides`, `config_diff_summary`). A `#[test]` that uses field count assertions or a procedural approach would prevent drift.
2. **Fix the security scanner** to skip comment/docstring content for shell-pattern checks. Parse file type before applying substring patterns.
3. **Decompose `dispatch_tool_call_batch` and `record_tool_result`** into named sub-methods for readability and testability.

### Next version (v0.4.0)

1. **Extract `find_cargo_root`** to a shared utility module (`session::verifier::helpers` or similar).
2. **Add an end-to-end test** crossing both verifier systems for `write_file`/`edit_file`.
3. **Add `kirkforge <subcommand> --help` smoke tests** for the remaining 8+ subcommands.
4. **Remove dead code**: `event_kinds.rs` and `ConnectionState::Connecting`.

### Long-term

1. **Unify the two verifier systems** into a single `BusVerifier`-based approach, deprecating the event-driven `Verifier` trait.
2. **Migrate config sync** to a derive macro (`#[derive(ConfigField)]`) that auto-generates merge/env/diff code.
3. **Add a real HTML parser** to `web_fetch` (the regex-based `html_to_text` is a documented `ponytail:` simplification).

---

## 13. Gate Verification

Per AGENTS.md, the following gates pass at the time of this review (state.md line 75-80):

```
cargo test --locked --workspace --no-fail-fast    = all pass
cargo clippy --all-targets -- -D warnings         = clean
cargo fmt --check                                  = clean
cargo check --workspace --all-targets             = clean
cargo test -p plugin3-core --test adr_xref_drift  = 3 passed
Feature-gated builds compile and pass
```

---

*End of review-2.md — 4,658 tests, 3 HIGH / 8 MEDIUM / 12 LOW findings*
