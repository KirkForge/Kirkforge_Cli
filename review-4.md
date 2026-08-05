# review-4.md — Full Codebase Review (Post Series 17)

**Date:** 2026-08-05
**Scope:** `src/` (session, tools, adapters, daemon, jobs, tui, main, shared), `crates/` (16 satellite crates), `plugins/` (5 bundled), `docs/` (84 ADRs + workorders + TECHNICAL.md), `.github/workflows/`, `tests/e2e/`, `benches/`.
**Method:** Six parallel explore agents (session/executor; tools+adapters; plugins; crates+workflow; daemon/tui/jobs; docs/ADRs/CI) plus direct gate runs. Post-Series-17 assessment scoring against review-2 and review-3 baselines.
**HEAD:** `8e116f9` (dev2)

---

## Headline verdict

The codebase is **architecturally strong and Series 17 has materially improved it**: the parallel fan-out workflow engine, per-provider auth, daemon instance channel, AST minification, configurable shutdown/stem caps, and E2E harness are all real shipped features. The ponytail debt is at zero untriggered items. Both review-3 CI-gate findings (C3, H11) are fixed. The test count grew from 4,658 (review-2) to 3,927 `#[test]` attrs under `src/` + `crates/` (this review's count methodology differs; review-2 counted `#[test]` + `#[tokio::test]` separately).

The weaknesses cluster in three places:

1. **One Critical plugin bug**: `folded_feature_enabled` at `loader.rs:44` matches `"kf-plugin-sdk3"` instead of `"kf-budget"`, causing the budget plugin to never be detected as folded. This can cause double-registration (shell + compiled-in) when both the feature flag and the shell plugin are present.
2. **Two security findings carried from review-3**: C1 (SSRF via `evaluate` in `computer_use`) and H1 (DNS rebinding in `web_fetch`) remain unfixed. The `evaluate` action bypasses all URL/deny-list guards. `web_fetch` resolves DNS but doesn't pin the resolved IP, allowing rebinding attacks.
3. **Three new High findings from this review**: per-plugin rlimits silently ignored by default (`SandboxConfig.harden` defaults to `false`), no audit logging for plugin tool invocations, and the jobs daemon has no auth token check on any request.

---

## Verification evidence (gates run during this review)

| Gate | Command | Result |
|---|---|---|
| Fmt | `cargo fmt` | Had 4 files with diffs; fixed with `cargo fmt` |
| Check | `cargo check --workspace --all-targets` | Clean ✅ |
| Test compile | `cargo test --locked --workspace --no-run` | Clean ✅ (all targets compiled) |
| Test run | `cargo test --locked --workspace --no-fail-fast` | Timed out (600s budget); not claimed green this session |
| Clippy | `cargo clippy --workspace --all-targets` | 6 warnings (useless `.into()` conversions); 0 errors |
| ADR count | `ls docs/adr/*.md \| grep -v README \| wc -l` | 84 |
| Bench tasks | `ls benches/tasks/*.toml \| wc -l` | 31 |
| Crate count | `ls crates/ \| wc -l` | 16 |
| `#[test]` attrs | `grep -r '#\[test\]' src/ crates/ --include='*.rs' \| wc -l` | 3,927 |

**Honesty note:** Tests timed out during this session. `cargo check` and `cargo test --no-run` are green. Clippy has 6 minor warnings (useless conversions). The fmt diffs were auto-fixed.

---

## Findings by severity

### Critical

**C1. `folded_feature_enabled` has wrong plugin name for budget feature — causes double-registration**
- `src/session/plugin_tools/loader.rs:44` — `folded_feature_enabled` matches `"kf-plugin-sdk3"` for the budget feature, but the `FOLDED_PLUGINS` table at line 41 maps plugin name `"kf-budget"` to feature `"budget"`. When both the `budget` feature flag and the `kf-budget` shell plugin are present, the shell plugin is loaded (because the name check fails) and the in-process budget tools are also registered — causing duplicate tool definitions. The `kf-plugin-sdk3` string appears to be a stale reference to an old package name.
- Fix: Change line 44 from `"kf-plugin-sdk3" => true` to `"kf-budget" => true`.

**C2. `computer_use` `evaluate` runs model-supplied JS with no network sandbox → SSRF via the browser** *(carried from review-3 C1)*
- `src/tools/computer_use.rs:374-458` — The `evaluate` action passes `args["expression"]` straight to `session.evaluate(expression)`. Once a page is loaded, `evaluate` can run `fetch('http://169.254.169.254/...')` from inside the browser, bypassing the host-level check that guards `open`/`navigate`. No `--proxy-server` or `--host-resolver-rules` are set on Chrome launch.
- Fix: Launch Chrome with `--proxy-server`/`--host-resolver-rules` that block RFC1918 + link-local, or sandbox `evaluate` to deny `fetch`/`XMLHttpRequest` to non-allowlisted hosts.

### High

**H1. `web_fetch` DNS rebinding — TOCTOU between resolve and connect** *(carried from review-3 H1)*
- `src/tools/web_fetch.rs:98-108` — Resolves the hostname to check for internal IPs, but `reqwest::Client` performs a second DNS lookup on connect. An attacker controlling DNS can return a public IP for the check and `127.0.0.1` for the connect.
- The code comment at line 196 already names the fix. Not yet implemented.

**H2. `web_fetch` `extract_host` does not percent-decode before IP check** *(new)*
- `src/tools/web_fetch.rs:259-297` — A URL like `http://%31%36%39%2e%32%35%34%2e%31%36%39%2e%32%35%34/` decodes to `169.254.169.254` but passes the literal-IP check because the percent-encoded string doesn't parse as `IpAddr`. The `reqwest` client decodes before connecting.

**H3. Per-plugin rlimits silently ignored unless `harden: true` is set** *(new)*
- `src/shared/mod.rs:279` — `SandboxConfig.harden` defaults to `false`. `PluginToolWrapper::run()` only calls `setup_rlimits` when `harden` is true. A plugin author who sets `cpu_secs: 1` in their manifest gets no enforcement. The host-crate `PluginTool::from_capability()` at `kf-plugin-host/src/tool.rs:46` never sets `resource_limits` at all, so even with `harden: true`, the host-crate path has no rlimits.
- Fix: Either default `harden` to `true` (breaking change) or always apply declared resource limits regardless of the `harden` flag.

**H4. No audit logging for plugin tool invocations** *(new)*
- `src/session/plugin_tools/wrapper.rs:197-343` — `PluginToolWrapper::run()` never calls `AuditLog`. The `AuditLog` enum has `Tool` and `Hook` entry types but no `PluginTool` variant. An operator relying on the audit log for security monitoring has no visibility into plugin tool calls.
- Fix: Add `PluginTool { name, args, exit_code, duration }` to `AuditEntry` and log it in `PluginToolWrapper::run()`.

**H5. Jobs daemon (`jobd`) has no auth token check on any request** *(new)*
- `src/jobs/daemon.rs:257-283` — Every request type is handled without calling `check_auth`. Compare the session daemon (`server.rs:349-447`) which calls `s.check_auth(auth_token.as_deref())` on every request. Any local user who can write to the Unix socket can send `Shutdown`, `QuitAll`, or schedule arbitrary jobs.
- Fix: Add `check_auth` to every request handler in `handle_client`, matching the session daemon pattern.

**H6. `ScheduledJob.timeout` is declared but never enforced** *(new)*
- `src/jobs/schedule.rs:34`, `src/jobs/runner.rs:111` — The `timeout` field is `Option<std::time::Duration>` and serialized/deserialized, but `run_bash_job` at `runner.rs:111` passes `None` for timeout: `registry.spawn(command, None, None, ...)`. A user who sets `timeout: 300` in their job config expects enforcement, but gets none.
- Fix: Pass `j.timeout` to `registry.spawn` in both `run_bash_job` and `run_workflow_job`.

**H7. Daemon client hardcodes `auth_token: None` in all call sites** *(new)*
- `src/daemon/client.rs:120,149,172,185,197,224,365` — Every `DaemonClient` method sends `auth_token: None`. If `KF_CODE_DAEMON_TOKEN_FILE` is configured, all client calls fail with "authentication required." The TUI event reader (`daemon_events.rs:59-63`) also sends `auth_token: None` for `InstanceRegister`.

**H8. Duplicate SSE frame-parsing logic across Anthropic and OpenAI adapters** *(new)*
- `src/adapters/anthropic.rs:342-359` and `src/adapters/openai_compat/mod.rs:90-110` — ~70 lines of identical SSE buffer accumulation, `data:` extraction, `\n\n`/`\r\n\r\n`/`\r\r` delimiter detection, `MAX_SSE_BUFFER_BYTES` cap, and drain logic. Only the inner JSON dispatch differs.
- Fix: Extract a shared `parse_sse_frames` generator that yields `(payload: Vec<u8>, is_done: bool)`.

**H9. Bedrock `envelope_buffer` has no size cap** *(carried from review-3 H3)*
- `src/adapters/anthropic_bedrock.rs:127` — `envelope_buffer.extend_from_slice(chunk)` grows without limit. If `extract_payload` returns `None`, the buffer is never cleared → OOM. Multi-event chunks drop all but the first event.

**H10. Bedrock `extract_payload` is O(n×m) with JSON-deserializer backtracking risk** *(new)*
- `src/adapters/anthropic_bedrock.rs:184-200` — Iterates every byte position and runs `serde_json::Deserializer::from_str` at each `{`, which may backtrack. For a garbage buffer, this is quadratic. The 8 MiB cap limits the absolute worst case but permits a meaningful CPU spike.

### Medium

**M1. `llm_compaction_summary` name is misleading** *(new)*
- `src/session/prompt/microcompaction.rs` — Despite the name, this function performs a deterministic heuristic summary, not an LLM call. Rename to `deterministic_compaction_summary` or add a doc comment clarifying it does not call an LLM.

**M2. `MicrocompactResult` has `#[allow(dead_code)]` on `summarised_messages`** *(new)*
- `src/session/prompt/microcompaction.rs:28` — The field is dead code. Remove or document its purpose.

**M3. Verifier bus stubs produce no findings — `SecurityBusVerifier` and `GitBusVerifier` are dead weight** *(new)*
- `src/session/verifier/bus.rs:258-286` — Both stubs return `Vec::new()`. They exist only to inflate `verifier_count()`. The real security and git checks only run through the legacy `VerifierSlots` path.

**M4. `VerifierSlots` has a max of 4 slots** *(new)*
- `src/session/verifier/slots.rs:13-14` — The 5th+ verifier registration silently fails with `bail!`. Adding a plugin verifier to the 6 built-in verifiers would hit this limit.

**M5. `verify_event` short-circuits on first non-Clean/Skipped verdict** *(new)*
- `src/session/verifier/handler.rs:69-83` — If a lint error and a security error occur on the same file, only the higher-priority finding is reported. The bus path does not short-circuit.

**M6. Jobs daemon has no socket guard against stale sockets** *(new)*
- `src/jobs/daemon.rs:52-57` — Unconditionally removes existing socket file before binding. If a second `jobd` starts while the first is live, it silently hijacks the socket. The session daemon at `server.rs:44-60` first tries to connect and refuses to hijack.

**M7. Workflow `run_bash` bypasses deny list and sandbox** *(new)*
- `src/tools/workflow.rs:209-232` — Runs `sh -c command` directly via `tokio::process::Command` without `check_bash_command_str` or `SandboxConfig`. A workflow step can execute arbitrary commands with no guard rails.

**M8. Workflow `ToolContext` has fresh `CancellationToken` and `dry_run: false`** *(new)*
- `src/tools/workflow.rs:247-253` — If the parent session is cancelled, workflow tool steps continue running. If the parent session is in dry-run mode, workflow tool steps execute destructively.

**M9. `CompositeToolset` resolution order is builtin > MCP > plugin > stratum > draw > budget > video** *(new)*
- `src/main/run_session.rs:371-548` — Folded plugins are appended after the generic plugin layer. A user plugin named `stratum/run` would shadow the in-process stratum tools. The module comment says "builtin > MCP > plugin > folded" but the actual order gives user plugins priority over folded tools.

**M10. `load_one` vs `load_from_dir` asymmetry for invalid manifests** *(new)*
- `crates/kf-plugin-host/src/lib.rs:185-190` vs `:334-337` — `load_from_dir` rejects invalid manifests (skips + warns), while `load_one` loads them with warnings. A manifest with an invalid name loads via `load_one` but is skipped via `load_from_dir`.

**M11. Trust tier not enforced at dispatch time** *(new)*
- `src/session/plugin_tools/wrapper.rs:197` — `effective_trust` is stored on `HostedPlugin` but never checked at tool invocation. A ReadOnly plugin's Skill prompt can indirectly cause shell execution by instructing the model.

**M12. `workflow.rs` `resolve_step_refs` operates on byte offsets, not grapheme clusters** *(new)*
- `crates/kf-workflow/src/lib.rs:437-443` — `chars[i] as char` treats each byte as a character. Multi-byte UTF-8 in `$(...)` expressions produces mojibake. The kf-draw crate has the same documented issue with upgrade triggers.

**M13. `WorkflowExecutor::run` is a ~500-line monolith** *(new)*
- `crates/kf-workflow/src/lib.rs` — Mixes budget checking, condition evaluation, step dispatch, FanOut/FanIn, batch execution, critique, and on_error routing in one function. Should be decomposed.

**M14. `stem_file_cap` const disconnect from config default** *(new)*
- `src/session/executor/turn.rs:1477-1478` — `STEM_FILE_CAP = 4096` is the fallback, but the `Config::stem_file_cap` default is also 4096. If someone changes one without the other, they silently diverge.

**M15. Config field drift guard uses hardcoded field counts** *(new)*
- `src/session/config/mod.rs:1891-2038` — `CONFIG_FIELD_COUNT == 82`, `MERGE_TOML_EXPECTED == 71`, `ENV_OVERRIDE_EXPECTED == 67`. Every new config field requires updating three constants. A derive macro would eliminate the coupling.

**M16. `append_alert` writes to `<data_dir>/.alerts.ndjson` instead of `<data_dir>/sessions/`** *(new)*
- `src/session/session_index.rs:315` — The alerts file lives at the data directory root, not inside `sessions/`. Inconsistent with the session index which is at `sessions/.index.ndjson`.

**M17. `kf-testdoctor::diagnose` hardcodes `DEFAULT_DIRS`** *(new)*
- `crates/kf-testdoctor/` — Only scans `src/session`, `src/tools`, `src/adapters`. Misses all `crates/` source files. The line-level `pub fn` counting heuristic is also inaccurate for `pub(crate)` and multi-line signatures.

**M18. KIRK-BENCH arithmetic is wrong: 31 + 19 = 50, not 40** *(carried from review-3 M21)*
- `KIRK-BENCH.md:3,256` — Headline claims 40 tasks, but implemented (31) + planned (19) = 50. The "~9 remaining" deferral should be ~22 (19 planned + 3 unmapped). TOML files also lack a `category` field for automated reporting.

**M19. Series 17 changes undocumented in CHANGELOG.md** *(new)*
- The `[Unreleased]` section covers through Series 15. WO 17.5-17.9 work (E2E harness, TUI parity, workflow engine parity, stem-agents, ponytail debt) is committed but not in CHANGELOG.

**M20. `bash.rs` Docker bind-mount source not validated against project root** *(carried from review-3 H2, partially)*
- `src/tools/bash.rs:90-104` — Canonicalizes the workdir and rejects `:` in the path, but does not verify the canonical path starts with the project root. A symlink inside the workdir pointing to `/etc` would be mounted read-write.

**M21. `computer_use.rs` `run_on_tab` and `run_on_session_sync` are near-identical** *(new)*
- `src/tools/computer_use.rs:333-480` — ~80 lines of duplicated action-dispatch logic. Extract a generic dispatch over a `ChromeTab` reference.

**M22. Bedrock `vertex_auth::service_account_token` silently returns empty string on token-fetch failure** *(carried from review-3 H4)*
- `src/adapters/vertex_auth.rs:44` — `Ok(token.token().unwrap_or_default().to_string())`. A `None` token becomes `""`, causing a generic 401 with no indication that the token was empty.

### Low

**L1. `format_verdict_report` slices `&file_line[..23]` without char boundary check** — `src/session/verifier/bus.rs:193-197`. Panics on multi-byte UTF-8 at byte 22. *(carried from review-3 L1)*

**L2. `PostTurnHookGuard::drop` fires hook synchronously** — `src/session/executor/turn.rs:28-46`. A blocked spawn blocks the drop. *(carried from review-3 L2)*

**L3. `worktree.rs::WorktreeSession::create` interpolates `session_id` with no validation** — `src/session/worktree.rs:14-38`. `session_id` accepts `&str`; `..` or `/` escapes `temp_dir()`. *(carried from review-3 L3)*

**L4. `ReadFile::minify_above_bytes` has stale `#[allow(dead_code)]`** — `src/tools/read_file.rs:9-11`. The field IS used at line 114. *(carried from review-3 L13)*

**L5. `AppState` has 44+ fields (God object)** — `src/tui/app.rs:183-523`. Structural concern from review-2 A3, still present. Low priority.

**L6. `m5_tests.rs` lives in `src/adapters/` as a sibling module** — `src/adapters/mod.rs:242`. Unusual but correct.

**L7. Minify cache eviction is not LRU** — `src/shared/minify/mod.rs:88-96`. Removes first N/2 entries from HashMap iteration, which is undefined order.

**L8. JS revalidation uses TSX grammar** — `src/shared/minify/lang.rs:274`. `Lang::JavaScript` arm in `revalidate` uses `tree_sitter_typescript::LANGUAGE_TSX` instead of `tree_sitter_javascript::LANGUAGE`. Inconsistent with the minification path which uses the dedicated JS grammar.

**L9. Minify Ruby strips only whole-line `#` comments** — `src/shared/minify/lang.rs:803-818`. Inline Ruby comments survive minification. The AST path does not handle Ruby.

**L10. `ScheduledJob::is_path_safe` does not reject backslashes or colons** — `src/jobs/schedule.rs:120-134`. Windows-path characters could cause confusion on cross-platform configs.

**L11. Clippy: 6 useless `.into()` conversions** — `src/adapters/anthropic.rs:1234`, `src/tui/commands/jobs.rs:367`, and 4 others. Minor, auto-fixable.

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
- **C1**: `folded_feature_enabled` name mismatch causes potential double-registration of the budget plugin.
- **H3**: Per-plugin rlimits silently ignored unless `harden: true` is set.
- **H4**: No audit logging for plugin tool invocations.
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
| **Security layers** | Path guard, deny list, bash safety check, URL scheme guard, cloud-metadata deny list, internal-IP rejection, atomic writes. |
| **KIRK-BENCH** | 31 tasks, verification harness, budget challenge, regression gate, delta comparison. Complete per ADR-066 data model. |
| **Ponytail debt** | Zero items with no upgrade trigger. 5 upgraded, 16 trigger-added, 26 verified (test pins + ADR references). |

### Concerns

| Area | Assessment |
|---|---|
| **Config field drift** | 3-way manual coupling between `Config`, `merge_toml_into_config`, and `apply_env_overrides`. The drift guard catches omissions but is fragile. A derive macro would be better. |
| **Dual verifier system** | Bus stubs produce zero findings. The bus path is architecturally present but operationally inert for built-in verifiers. `VerifierSlots` 4-slot limit would be hit by adding a 5th verifier. |
| **Monolithic functions** | `run_turn_inner` (~430 lines), `dispatch_tool_call_batch` (~350 lines), `WorkflowExecutor::run` (~500 lines), `record_tool_result` (~280 lines). Decompose into named sub-methods. |
| **Workflow security gap** | `run_bash` in workflow steps bypasses `check_bash_command_str` and `SandboxConfig`. A workflow step can execute arbitrary commands with no guard rails. |
| **Daemon auth gap** | Jobs daemon has no auth token check. Daemon client hardcodes `auth_token: None`. If auth is configured, both are broken. |

---

## Convention compliance check

| Convention (AGENTS.md §4/§7) | Status | Notes |
|---|---|---|
| `anyhow` for errors | ✅ clean | Consistent across subsystems |
| `CorrectionResult` is a struct | ✅ clean | But `verifier` field is hard-coded to `"verifier"` (M1) |
| `bincode` rejected | ✅ clean | `serde_json` everywhere |
| `#[allow(dead_code)]` with reason | ✅ clean | All 14 annotations have reason comments |
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
| **Total** | **40** | **31** (26 unique) | **9+ missing by spec** |

**Note**: The spec headline claims "40 tasks" but 31 implemented + 19 planned = 50. The arithmetic inconsistency from review-3 M21 remains.

### Coverage gates (ADR-065)

| Module | Threshold | Current | Status |
|---|---|---|---|
| `src/session` | 68.5% | Not measured this session | CI gate is authoritative |
| `src/tools` | 76.0% | Not measured this session | CI gate is authoritative |
| `src/adapters` | 75.0% | Not measured this session | CI gate is authoritative |

### Review-3 baseline comparison

| Review-3 Finding | Status |
|---|---|
| C1 (SSRF via evaluate) | **Open** — no sandbox on `evaluate` |
| C2 (load_from_dir skips validate) | **Fixed** — `load_from_dir` now calls `validate()` |
| C3 (CI gate theater) | **Fixed** — renamed to "Warn if..." and `cargo audit` uses `continue-on-error` |
| H1 (DNS rebinding) | **Open** — not yet implemented |
| H2 (Docker bind-mount) | **Open** — canonical path not validated against project root |
| H3 (Bedrock envelope buffer) | **Open** — no size cap |
| H4 (Vertex empty token) | **Open** — silently returns `""` |
| H5 (Docker .expect) | **Open** |
| H6 (KNOWN_EVENTS stale) | **Open** — `post-tool-write_file` not in allowlist |
| H7-H12 | Mostly **Open** |
| M1 (verifier name hard-coded) | **Open** |
| M20 (ADR-066 "30" tasks) | **Open** — still says "30" in 4 places |
| L17 (test count stale) | **Open** — now 3,927, was 1,670 in crates |

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

## Recommended fix priority

**Tier 1 — security and correctness, fix immediately:**
1. **C1** — Fix `folded_feature_enabled` name mismatch (`"kf-plugin-sdk3"` → `"kf-budget"`)
2. **C2** — Sandbox `computer_use` `evaluate` (Chrome `--proxy-server` or deny `fetch` to internal ranges)
3. **H1** — Implement DNS pin-and-recheck in `web_fetch`
4. **H2** — Add percent-decoding before IP literal check in `extract_host`
5. **H5** — Add `check_auth` to every `jobd` request handler
6. **H6** — Pass `j.timeout` to `registry.spawn` in jobs runner
7. **H7** — Read `KF_CODE_DAEMON_TOKEN_FILE` in `DaemonClient` methods and TUI event reader

**Tier 2 — real bugs, fix soon:**
8. **H3** — Default rlimits enforcement on or always apply declared limits regardless of `harden`
9. **H4** — Add `PluginTool` audit entry to `AuditLog`
10. **H9** — Cap Bedrock `envelope_buffer` and parse multi-event chunks
11. **M7** — Route workflow `run_bash` through `check_bash_command_str`
12. **M8** — Propagate parent session's `CancellationToken` and `dry_run` into workflow `ToolContext`
13. **M12** — Fix `resolve_step_refs` byte-offset issue for multi-byte UTF-8
14. **L8** — Fix JS revalidation to use `tree_sitter_javascript::LANGUAGE`

**Tier 3 — doc drift, polish, and structural improvements:**
15. **M18** — Fix KIRK-BENCH arithmetic (31+19=50, not 40)
16. **M19** — Add Series 17 changes to CHANGELOG.md
17. **M3** — Remove or make-functional the bus verifier stubs
18. **M15** — Consider a derive macro for config field count
19. **M21** — Extract shared dispatch from `run_on_tab`/`run_on_session_sync`
20. **L4** — Remove stale `#[allow(dead_code)]` on `ReadFile::minify_above_bytes`
21. **L11** — Fix 6 useless `.into()` conversions flagged by clippy

---

## One-line summary

**Series 17 shipped well; one critical plugin name mismatch, two carried SSRF findings, and an unauthenticated jobs daemon are the items that actually matter; the plugin system is solid but needs rlimits enforcement, audit logging, and a folded-plugin name fix.**