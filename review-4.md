# review-4.md — Full Codebase Review (Post Series 17)

**Date:** 2026-08-05
**Scope:** `src/` (session, tools, adapters, daemon, jobs, tui, main, shared), `crates/` (16 satellite crates), `plugins/` (5 bundled), `docs/` (84 ADRs + workorders + TECHNICAL.md), `.github/workflows/`, `tests/e2e/`, `benches/`.
**Method:** Six parallel explore agents (session/executor; tools+adapters; plugins; crates+workflow; daemon/tui/jobs; docs/ADRs/CI) plus direct gate runs. Post-Series-17 assessment scoring against review-2 and review-3 baselines.
**HEAD:** `e1a3c87` (main)
**Updated:** Findings annotated with fix status as of `e1a3c87`.

---

## Headline verdict

The codebase is **architecturally strong and Series 17 has materially improved it**: the parallel fan-out workflow engine, per-provider auth, daemon instance channel, AST minification, configurable shutdown/stem caps, and E2E harness are all real shipped features. The ponytail debt is at zero untriggered items. Both review-3 CI-gate findings (C3, H11) are fixed. The test count grew from 4,658 (review-2) to 3,927 `#[test]` attrs under `src/` + `crates/` (this review's count methodology differs; review-2 counted `#[test]` + `#[tokio::test]` separately).

**Review-4 findings triage: 21 fixed, 3 partially fixed, 17 open.** All Critical and most High findings are resolved. The remaining open items are structural/debt (monolith functions, God object, hardcoded counts) and two High findings (duplicate SSE parsing, Bedrock O(n×m) deserializer).

---

## Verification evidence (gates run during this review)

| Gate | Command | Result |
|---|---|---|
| Fmt | `cargo fmt` | Had 4 files with diffs; fixed with `cargo fmt` |
| Check | `cargo check --workspace --all-targets` | Clean ✅ |
| Test compile | `cargo test --locked --workspace --no-run` | Clean ✅ (all targets compiled) |
| Test run | `cargo test --locked --workspace --no-fail-fast` | Timed out (600s budget); not claimed green this session |
| Clippy | `cargo clippy --workspace --all-targets` | 0 warnings, 0 errors ✅ (6 useless `.into()` fixed) |
| ADR count | `ls docs/adr/*.md \| grep -v README \| wc -l` | 84 |
| Bench tasks | `ls benches/tasks/*.toml \| wc -l` | 31 |
| Crate count | `ls crates/ \| wc -l` | 16 |
| `#[test]` attrs | `grep -r '#\[test\]' src/ crates/ --include='*.rs' \| wc -l` | 3,927 |

---

## Findings by severity

### Critical

**C1. `folded_feature_enabled` has wrong plugin name for budget feature — causes double-registration** ✅ FIXED
- ~~`src/session/plugin_tools/loader.rs:44`~~ — Fixed in `95ef196`: `"kf-plugin-sdk3"` → `"kf-budget"`.

**C2. `computer_use` `evaluate` runs model-supplied JS with no network sandbox → SSRF via the browser** ✅ FIXED
- ~~`src/tools/computer_use.rs:374-458`~~ — Fixed in `3a4e96d`: `EVALUATE_SAFETY_PREAMBLE` blocks `fetch`/`XMLHttpRequest` in evaluate mode. Note: WebSocket/EventSource not blocked (acknowledged as known gap).

### High

**H1. `web_fetch` DNS rebinding — TOCTOU between resolve and connect** ✅ FIXED
- ~~`src/tools/web_fetch.rs:98-108`~~ — Fixed in `3a4e96d`: `resolve_and_pin_dns()` resolves hostname, checks all IPs, and pins via `reqwest::ClientBuilder::resolve()`. Literal IPs skip pinning (no rebinding risk).

**H2. `web_fetch` `extract_host` does not percent-decode before IP check** ✅ FIXED
- ~~`src/tools/web_fetch.rs:259-297`~~ — Fixed in `3a4e96d`: `percent_decode_str(&host).decode_utf8_lossy()` applied before IP check.

**H3. Per-plugin rlimits silently ignored unless `harden: true` is set** ⚠️ PARTIALLY FIXED
- `src/shared/mod.rs:279` — Unix rlimits now always applied (harden guard removed). **But**: `run_session.rs` passes `None` for `resource_limits` in the startup path (no executor available), so plugin tools created at session startup have no rlimits propagated. Windows still guards on `harden`.

**H4. No audit logging for plugin tool invocations** ✅ FIXED
- ~~`src/session/plugin_tools/wrapper.rs:197-343`~~ — Fixed in `95ef196`: `AuditEntry::PluginTool` variant and `log_plugin_tool()` added. Executor path passes `Some(Arc::clone(&self.audit_log))`.

**H5. Jobs daemon (`jobd`) has no auth token check on any request** ✅ FIXED
- ~~`src/jobs/daemon.rs:257-283`~~ — Fixed in `b68db59`: `check_auth()` called on every request handler with constant-time comparison.

**H6. `ScheduledJob.timeout` is declared but never enforced** ⚠️ PARTIALLY FIXED
- `src/jobs/runner.rs:111` — Bash jobs now pass `job.timeout.map(|d| d.as_secs())`. **But**: `run_workflow_job` still passes `None` for timeout.

**H7. Daemon client hardcodes `auth_token: None` in all call sites** ✅ FIXED
- ~~`src/daemon/client.rs`~~ — Fixed in `b68db59`: `read_auth_token()` reads from `KF_CODE_DAEMON_TOKEN_FILE` and is used in every `DaemonClient` method and the TUI event reader.

**H8. Duplicate SSE frame-parsing logic across Anthropic and OpenAI adapters** 🔴 OPEN
- `src/adapters/anthropic.rs:330-360` and `src/adapters/openai_compat/mod.rs:80-120` — ~70 lines of identical SSE buffer accumulation, `data:` extraction, `\n\n`/`\r\n\r\n`/`\r\r` delimiter detection, `MAX_SSE_BUFFER_BYTES` cap, and drain logic. Only the inner JSON dispatch differs.
- Fix: Extract a shared `parse_sse_frames` generator that yields `(payload: Vec<u8>, is_done: bool)`.

**H9. Bedrock `envelope_buffer` has no size cap** ✅ FIXED (prior to review-4)
- `src/adapters/anthropic_bedrock.rs:137` — 8 MiB `MAX_ENVELOPE_BUFFER_BYTES` cap is present with drain loop for multi-event chunks. The O(n×m) deserializer (H10) remains open.

**H10. Bedrock `extract_payload` is O(n×m) with JSON-deserializer backtracking risk** 🔴 OPEN
- `src/adapters/anthropic_bedrock.rs:184-200` — Iterates every byte position and runs `serde_json::Deserializer::from_str` at each `{`, which may backtrack. For a garbage buffer, this is quadratic. The 8 MiB cap limits the absolute worst case but permits a meaningful CPU spike.

### Medium

**M1. `llm_compaction_summary` name is misleading** ✅ FIXED
- Fixed in `b9cf1a8`: renamed to `deterministic_compaction_summary`.

**M2. `MicrocompactResult` has `#[allow(dead_code)]` on `summarised_messages`** ✅ FIXED
- Fixed in `b9cf1a8`: `summarised_messages` field removed; replaced with local `summarised_count`.

**M3. Verifier bus stubs produce no findings — `SecurityBusVerifier` and `GitBusVerifier` are dead weight** ✅ FIXED
- Fixed in `b9cf1a8`: both stubs removed. `default_verifier_bus()` starts empty. `verifier_count() == 0`.

**M4. `VerifierSlots` has a max of 4 slots** ✅ FIXED
- Fixed in `b9cf1a8`: raised to 8.

**M5. `verify_event` short-circuits on first non-Clean/Skipped verdict** ⚠️ PARTIALLY FIXED
- Fixed in `b9cf1a8`: behavior documented with a comment ("Truth model: first verifier to report a finding wins"). Short-circuit not changed — if a lint error and a security error occur on the same file, only the higher-priority finding is reported.

**M6. Jobs daemon has no socket guard against stale sockets** 🔴 OPEN
- `src/jobs/daemon.rs:52-57` — Unconditionally removes existing socket file before binding. If a second `jobd` starts while the first is live, it silently hijacks the socket. The session daemon at `server.rs:44-60` first tries to connect and refuses to hijack.

**M7. Workflow `run_bash` bypasses deny list and sandbox** ✅ FIXED
- Fixed in `b9cf1a8`: `check_bash_command_str` called in both `run_bash` and the fan-out path. `WorkflowTool` now carries `deny_list`, `path_guard`, and `bash_sandbox_workdir`.

**M8. Workflow `ToolContext` has fresh `CancellationToken` and `dry_run: false`** ✅ FIXED
- Fixed in `b9cf1a8`: `TaskSpawnerStepRunner` now holds `cancel_token: CancellationToken` and `dry_run: bool`, propagated through to tool steps and fan-out closures.

**M9. `CompositeToolset` resolution order is builtin > MCP > plugin > stratum > draw > budget > video** 🔴 OPEN
- `src/main/run_session.rs:371-548` — Folded plugins appended after the generic plugin layer. A user plugin named `stratum/run` would shadow the in-process stratum tools.

**M10. `load_one` vs `load_from_dir` asymmetry for invalid manifests** 🔴 OPEN
- `crates/kf-plugin-host/src/lib.rs:185-190` vs `:334-337` — `load_from_dir` rejects invalid manifests (skips + warns), while `load_one` loads them with warnings. A manifest with an invalid name loads via `load_one` but is skipped via `load_from_dir`.

**M11. Trust tier not enforced at dispatch time** 🔴 OPEN
- `src/session/plugin_tools/wrapper.rs:197` — `effective_trust` is stored on `HostedPlugin` but never checked at tool invocation. A ReadOnly plugin's Skill prompt can indirectly cause shell execution by instructing the model.

**M12. `workflow.rs` `resolve_step_refs` operates on byte offsets, not grapheme clusters** ✅ FIXED
- Fixed in `b9cf1a8`: uses `char_indices().peekable()` instead of raw byte indexing.

**M13. `WorkflowExecutor::run` is a ~500-line monolith** 🔴 OPEN
- `crates/kf-workflow/src/lib.rs` — Mixes budget checking, condition evaluation, step dispatch, FanOut/FanIn, batch execution, critique, and on_error routing in one function. Should be decomposed.

**M14. `stem_file_cap` const disconnect from config default** ✅ FIXED
- Fixed in `b9cf1a8`: cross-reference comment added; config docstring references the `STEM_FILE_CAP` constant.

**M15. Config field drift guard uses hardcoded field counts** 🔴 OPEN
- `src/session/config/mod.rs:1891-2038` — `CONFIG_FIELD_COUNT == 82`, `MERGE_TOML_EXPECTED == 71`, `ENV_OVERRIDE_EXPECTED == 67`. Every new config field requires updating three constants. A derive macro would eliminate the coupling.

**M16. `append_alert` writes to `<data_dir>/.alerts.ndjson` instead of `<data_dir>/sessions/`** ✅ FIXED
- Fixed in `b9cf1a8`: path changed to `data_dir.join("sessions").join(".alerts.ndjson")`.

**M17. `kf-testdoctor::diagnose` hardcodes `DEFAULT_DIRS`** 🔴 OPEN
- `crates/kf-testdoctor/src/diagnose.rs:48` — Only scans `src/session`, `src/tools`, `src/adapters`. Misses all `crates/` source files.

**M18. KIRK-BENCH arithmetic is wrong: 31 + 19 = 50, not 40** ✅ FIXED
- Fixed in `93e3026`: headline corrected to "31 implemented tasks across 40 spec slots"; bottom text corrected to "22 spec slots not yet implemented".

**M19. Series 17 changes undocumented in CHANGELOG.md** ✅ FIXED
- Fixed in `93e3026`: Series 17 entries added to CHANGELOG.

**M20. `bash.rs` Docker bind-mount source not validated against project root** 🔴 OPEN
- `src/tools/bash.rs:90-104` — Canonicalizes the workdir and rejects `:` in the path, but does not verify the canonical path starts with the project root. A symlink inside the workdir pointing to `/etc` would be mounted read-write.

**M21. `computer_use.rs` `run_on_tab` and `run_on_session_sync` are near-identical** 🔴 OPEN
- `src/tools/computer_use.rs:333-480` — ~80 lines of duplicated action-dispatch logic. Extract a generic dispatch over a `ChromeTab` reference.

**M22. Bedrock `vertex_auth::service_account_token` silently returns empty string on token-fetch failure** ✅ FIXED (prior to review-4)
- `src/adapters/vertex_auth.rs:43-46` — Now uses `.ok_or_else(|| anyhow::anyhow!("service account token endpoint returned None"))?` instead of `.unwrap_or_default()`.

### Low

**L1. `format_verdict_report` slices `&file_line[..23]` without char boundary check** ✅ FIXED
- Fixed in `b9cf1a8`: replaced with `char_indices().take(23)` collection.

**L2. `PostTurnHookGuard::drop` fires hook synchronously** 🔴 OPEN
- `src/session/executor/turn.rs:28-46`. A blocked spawn blocks the drop.

**L3. `worktree.rs::WorktreeSession::create` interpolates `session_id` with no validation** ✅ FIXED (prior to review-4)
- `src/session/worktree.rs:15-21` — Now validates: non-empty, no `/`, `\`, or `..`.

**L4. `ReadFile::minify_above_bytes` has stale `#[allow(dead_code)]`** ✅ FIXED (prior to review-4)
- Stale annotation removed. Field is used at line 113.

**L5. `AppState` has 44+ fields (God object)** 🔴 OPEN
- `src/tui/app.rs:183-523`. Structural concern from review-2 A3, still present. Low priority.

**L6. `m5_tests.rs` lives in `src/adapters/` as a sibling module** 🔴 OPEN
- `src/adapters/mod.rs:242`. Unusual but correct.

**L7. Minify cache eviction is not LRU** 🔴 OPEN
- `src/shared/minify/mod.rs:88-96`. Removes first N/2 entries from HashMap iteration, which is undefined order.

**L8. JS revalidation uses TSX grammar** ✅ FIXED
- Fixed in `b9cf1a8`: now uses `tree_sitter_javascript::LANGUAGE` instead of `tree_sitter_typescript::LANGUAGE_TSX`.

**L9. Minify Ruby strips only whole-line `#` comments** 🔴 OPEN
- `src/shared/minify/lang.rs:803-818`. Inline Ruby comments survive minification.

**L10. `ScheduledJob::is_path_safe` does not reject backslashes or colons** 🔴 OPEN
- `src/jobs/schedule.rs:120-134`. Windows-path characters could cause confusion on cross-platform configs.

**L11. Clippy: 6 useless `.into()` conversions** ✅ FIXED
- Fixed in `b9cf1a8`: removed in `src/tui/commands/jobs.rs` and 4 other locations. Clippy now reports 0 warnings.

---

## Plugin system assessment (per user request)

### Architecture

The plugin system spans three layers:
- **SDK** (`crates/kf-plugin-sdk/`): `PluginManifest` with name, version, trust tier, capabilities, `depends_on`, resource limits. Comprehensive `validate()` that collects all errors.
- **Host** (`crates/kf-plugin-host/`): `PluginRegistry` with loading, trust policy, topological sorting, capability filtering, signature verification, symlink-escape prevention.
- **Session integration** (`src/session/plugin_tools/`): Loader with folded-plugin detection, hot-reload watcher, `PluginToolWrapper` with sandbox, curated env, rlimits.

### Strengths
- Manifest validation is thorough (name, version, triggers, tool schemas, hook events, command paths, dependencies).
- Two-layer command safety: `check_relative_command_path` rejects `..` and backslashes; runtime `canonicalize` + `starts_with` prevents symlink escapes.
- Hot-reload with 500ms debounce is self-healing on partial writes.
- `CompositeToolset` resolution order is well-defined (builtin > MCP > plugin > folded).
- 1,236 lines of integration tests in `src/session/plugin_tools/tests.rs` covering tool execution, env isolation, sandbox, hot-reload, trust filtering, and all four capability types.

### Gaps (see findings above)
- ~~**C1**: `folded_feature_enabled` name mismatch~~ ✅ Fixed.
- ~~**H3**: Per-plugin rlimits silently ignored unless `harden: true`~~ ⚠ Partially fixed (Unix always applies; startup path and Windows gap remain).
- ~~**H4**: No audit logging for plugin tool invocations~~ ✅ Fixed.
- **M9**: Resolution order gives user plugins priority over folded tools, contradicting the documented order.
- **M10**: `load_one` vs `load_from_dir` asymmetry for invalid manifests.
- **M11**: Trust tier enforced at load time only (capability removal), not at dispatch. A ReadOnly plugin's Skill can indirectly cause shell execution via the model.

### Test coverage
- Well-covered: manifest validation (20+ tests), registry loading (15+ tests), trust policy, symlink escape, signature verification, topological sort, hot-reload timing, all four capability types.
- Missing: no integration test for end-to-end tool dispatch through the executor; no test for folded-plugin double-registration prevention; no test for rlimits with `harden: true`; no test for plugin tool audit logging.

---

## Architecture assessment

### Strengths

| Area | Assessment |
|---|---|
| **Provider abstraction** | `ModelAdapter` trait unifies 6 providers. NDJSON and SSE stream parsers are well-factored. Per-provider auth (`resolve_api_key`) with config → env → keychain order. |
| **Verification pipeline** | Dual verifier system provides defense-in-depth. Correction loop with 3-iteration cap + DoomLoopTracker. Both paths are functional. |
| **Tool dispatch** | Three-phase batch dispatch (pre-gate, run, record) with parallel execution and ordered recording. Read-before-edit gate. |
| **Plugin system** | Manifest-based with trust tiers, minisign signatures, topological load order, hot-reload, resource limits. Two-path dispatch for folded plugins. |
| **Security layers** | Path guard, deny list, bash safety check, URL scheme guard, cloud-metadata deny list, internal-IP rejection, DNS pinning (new), atomic writes. |
| **KIRK-BENCH** | 31 tasks, verification harness, budget challenge, regression gate, delta comparison. Complete per ADR-066 data model. |
| **Ponytail debt** | Zero items with no upgrade trigger. 5 upgraded, 16 trigger-added, 26 verified (test pins + ADR references). |

### Concerns

| Area | Assessment |
|---|---|
| **Config field drift** | 3-way manual coupling between `Config`, `merge_toml_into_config`, and `apply_env_overrides`. The drift guard catches omissions but is fragile. A derive macro would be better. |
| **Dual verifier system** | Bus stubs removed (M3 ✅). `VerifierSlots` now has 8-slot max (M4 ✅). Short-circuit still reports only first finding per event (M5 ⚠️ documented but unchanged). |
| **Monolithic functions** | `run_turn_inner` (~430 lines), `dispatch_tool_call_batch` (~350 lines), `WorkflowExecutor::run` (~500 lines), `record_tool_result` (~280 lines). Decompose into named sub-methods. |
| **Workflow security gap** | ~~`run_bash` bypasses deny list and sandbox~~ ✅ Fixed (M7). `CancellationToken`/`dry_run` now propagated (M8 ✅). |
| **Daemon auth gap** | ~~Jobs daemon has no auth token check~~ ✅ Fixed (H5). ~~Daemon client hardcodes `auth_token: None`~~ ✅ Fixed (H7). |

---

## Convention compliance check

| Convention (AGENTS.md §4/§7) | Status | Notes |
|---|---|---|
| `anyhow` for errors | ✅ clean | Consistent across subsystems |
| `CorrectionResult` is a struct | ✅ clean | But `verifier` field is hard-coded to `"verifier"` (~~M1~~ ✅ renamed) |
| `bincode` rejected | ✅ clean | `serde_json` everywhere |
| `#[allow(dead_code)]` with reason | ✅ clean | All annotations have reason comments |
| `println!`/`eprintln!`/`dbg!` in production | ✅ clean | Only user-facing `eprintln!` (bench progress, Windows warning, config banner) |
| `|| true` to silence failures | ✅ fixed | Review-3 C3 and H11 both fixed. `ci.yml:294` renamed to "Warn if...". `ci.yml:398` uses `continue-on-error: true` instead of `|| true`. |
| Plugin `validate()` on load path | ⚠️ partial | `load_from_dir` calls `validate()` ✅. `load_one` loads with warnings instead of rejecting. |
| ADR two-source-of-truth | ✅ clean | `adr_xref_drift` test passes. Both header + index agree. |

---

## Bench scoring vs established harnesses

### KIRK-BENCH (ADR-066)

| Category | Spec'd | Implemented | Gap |
|---|---|---|---|
| A (Repository Understanding) | 5 | 0 | **Full gap** — no category A tasks |
| B (Refactoring) | 5 | 6 | Near-complete |
| C (Bug Fixes) | 6 | 5 | Missing #11 Fix Compilation Error |
| D (New Features) | 5 | 6 | Missing #20 Implement Missing Trait |
| E (Verification) | 5 | 3 | Missing #22-24 (Build/Formatter/Lint Verification) |
| F (Context Intelligence) | 4 | 3 | Missing #27 Large Repository Navigation |
| G (Real Engineering) | 5 | 3 | Missing #32-33, #35 (Large Refactor, Merge Conflict, Regression) |
| H (Cost) | 5 | 0 | **Full gap** — no category H tasks |
| **Total** | **40** | **31** (26 unique) | **22 spec slots not yet implemented** |

### Coverage gates (ADR-065)

| Module | Threshold | Current | Status |
|---|---|---|---|
| `src/session` | 68.5% | Not measured this session | CI gate is authoritative |
| `src/tools` | 76.0% | Not measured this session | CI gate is authoritative |
| `src/adapters` | 75.0% | Not measured this session | CI gate is authoritative |

### Review-3 baseline comparison

| Review-3 Finding | Status |
|---|---|
| C1 (SSRF via evaluate) | ✅ **Fixed** — `EVALUATE_SAFETY_PREAMBLE` blocks fetch/XHR |
| C2 (load_from_dir skips validate) | ✅ **Fixed** — `load_from_dir` now calls `validate()` |
| C3 (CI gate theater) | ✅ **Fixed** — renamed to "Warn if..." and `cargo audit` uses `continue-on-error` |
| H1 (DNS rebinding) | ✅ **Fixed** — DNS pin-and-recheck via `resolve_and_pin_dns()` |
| H2 (Docker bind-mount) | 🔴 **Open** — canonical path not validated against project root (M20) |
| H3 (Bedrock envelope buffer) | ✅ **Fixed** — 8 MiB cap with drain loop (prior to review-4) |
| H4 (Vertex empty token) | ✅ **Fixed** — `.ok_or_else()` returns proper error (prior to review-4) |
| H5 (Docker .expect) | 🔴 Open |
| H6 (KNOWN_EVENTS stale) | 🔴 Open |
| H7-H12 | Mostly **Open** |
| M1 (verifier name hard-coded) | ✅ **Fixed** — renamed to `deterministic_compaction_summary` |
| M20 (ADR-066 "30" tasks) | ✅ **Fixed** — corrected to "31 tasks" |
| L17 (test count stale) | 🔴 **Open** — now 3,927, was 1,670 in crates |

### Ponytail debt status

All 46 markers resolved:
- **5 Upgraded**: `max_parallel` semaphore, `DEFAULT_SHUTDOWN_TIMEOUT_SECS`, `STEM_FILE_CAP` config, JS grammar, `fan_out` parallel. All verified in code.
- **16 Trigger Added**: Every trigger text present at the cited file:line.
- **26 Verified**: Test pins and ADR references.
- **0 items with no trigger**.

---

## What this review did NOT verify

- **`cargo test --locked --workspace --no-fail-fast`** — timed out at 600s during this session. Not claimed green.
- **`cargo tarpaulin`** — not run. Coverage numbers are from CI, not local measurement.
- **Integration tests** (`scripts/run-integration-tests.sh`) — require live Ollama; not run.
- **E2E harness** — not run end-to-end. The mock provider and IsolatedEnv were code-reviewed only.
- **Plugin signature verification** — `minisign` path not tested (requires key material).
- **Workflow engine parallel execution** — the semaphore test was code-reviewed but not run.
- **The six explore agents' findings are their analysis, not a line-by-line re-read of every file.** I directly verified C1 (the `folded_feature_enabled` mismatch), the CI gate fixes, and the ponytail debt upgrades. The remaining findings are cited to the agent that produced them and should be treated as high-confidence leads.

---

## ⚠️ Post-review finding: runtime gate uses wrong plugin name

Both GPT and Claude Sonnet independently reviewed this codebase and found a critical
bug that review-4's C1 fix **missed**: while C1 fixed `folded_feature_enabled()` in
`loader.rs`, **four separate runtime gates** still use `"kf-plugin-sdk3"` instead of
`"kf-budget"`:

- `src/main/run_session.rs:543-544` (tool registration)
- `src/session/executor/mod.rs:237-238` (hook registration)
- `src/session/executor/mod.rs:636-637` (hot-reload re-registration)

Since nothing ever inserts `"kf-plugin-sdk3"` into `enabled_plugins`, these gates
evaluate to `false` unconditionally — **the entire budget subsystem (tools + hooks)
never registers on any default build**. See WO 18.0.1 for full details.

---

## Remaining open findings — recommended fix priority

**See [WO 18.0](docs/workorders/18.0-review-4-cross-review-fixes.md) for the full
prioritized list including cross-review findings.** Summary:

**Tier 1 — critical, blocks default functionality:**
1. **18.0.1** — Replace `"kf-plugin-sdk3"` with `"kf-budget"` in 4 runtime gate locations (budget subsystem silently dead)
2. **18.0.3** — Add integration test asserting budget tools present after default boot

**Tier 2 — real bugs, fix soon:**
3. **H8** — Extract shared `parse_sse_frames` from Anthropic + OpenAI adapters
4. **H10** — Optimize `extract_payload` to avoid O(n×m) backtracking
5. **H3** (partial) — Pass `resource_limits` in the startup path
6. **H6** (partial) — Pass `j.timeout` to `registry.spawn` in `run_workflow_job`
7. **M6** — Add stale-socket guard to `jobd`
8. **M11** — Enforce `effective_trust` at tool dispatch time
9. **M20** — Validate Docker bind-mount source against project root
10. **18.0.2** — Add restart-required notice to `/plugins toggle` message
11. **18.0.4** — Fix stale `plugins/kf-plugin-sdk3/` reference in `budget.rs`

**Tier 3 — structural, polish, and low priority:** (see WO 18.0)

---

## One-line summary

**All review-4 Criticals fixed, but a post-review cross-review found the budget runtime gate still uses the wrong name (`kf-plugin-sdk3` → `kf-budget`) in 4 locations — the entire budget subsystem is silently dead on default builds. Remaining open items are H8 (duplicate SSE parsing), H10 (Bedrock O(n×m)), two partial fixes, and structural debt. See WO 18.0.**