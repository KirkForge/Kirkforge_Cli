# review-3.md — Full Codebase Review

**Date:** 2026-07-31
**Scope:** `src/` (session, tools, adapters, daemon, jobs, tui, main, shared), `crates/` (16 satellite crates), `plugins/` (5 bundled), `docs/` (84 ADRs + workorders + TECHNICAL.md), `.github/workflows/`, `scripts/`.
**Method:** Four parallel explore agents (session/executor; tools+adapters; crates+plugins; docs/ADRs/tests/CI) plus direct gate runs and targeted file reads to verify the highest-severity findings.

---

## Headline verdict

The codebase is **structurally strong** and **architecturally coherent**. The three headline counts (84 ADRs / 31 bench tasks / 16 crates) are exactly right; the `adr_xref_drift` test is green; all `#[ignore]` attributes are honestly justified; there is **zero debug spam** in production code (no `println!`/`eprintln!`/`dbg!`/`tracing::debug!` outside tests); the WO 14.8 dead-code audit removed the accumulated bloat; the bash safety gate is a single shared chokepoint for every shell path; the `ChromeTab` trait correctly handles the `headless_chrome::Tab` weak-ref gotcha; `bincode` is rejected project-wide; `CorrectionResult` is a struct, not an enum.

The weaknesses cluster in three places:

1. **Two CI steps that look like gates but don't gate** (`.github/workflows/ci.yml:294` and `:388`) — direct violations of AGENTS.md §4 ("Do not add `|| true`... to make red go green").
2. **Narrative doc-drift** where status docs contradict themselves (state.md lists `use_workflow_run` as deferred while WO 9.1 is Done, the tool exists, and the bench task exists; ADR-066 says "30 tasks" four times while the rest of the docs say 31; KIRK-BENCH says "40 tasks" but the planned list has 19 and 31+19≠40).
3. **A cluster of real bugs in the Bedrock/Vertex/cloud paths** (unbounded envelope buffer, dropped multi-event chunks, silent empty-token-on-failure) that the maintainer likely doesn't hit on Ollama but will bite on cloud-provider turns.

There is **one Critical security finding** (the `computer_use` `evaluate` action is a prompt-injection → SSRF chain via the browser) and **one Critical plugin finding** (`load_from_dir` skips `PluginManifest::validate()`, the WO 8.8 error-collecting contract, on the primary load path).

---

## Verification evidence (gates run during this review)

| Gate | Command | Result |
|---|---|---|
| Fmt | `cargo fmt --check` | clean (no output) |
| Check | `cargo check --workspace --all-targets` | clean (`Finished dev profile in 32.16s`) |
| Test | `cargo test --locked --workspace --no-fail-fast` | **timed out at 600s** — not verified green this session |
| Clippy | (not run — 3-4 min budget per AGENTS.md §7) | not verified this session |
| ADR count | `ls docs/adr/*.md \| grep -v README \| wc -l` | 84 (matches docs) |
| Bench count | `ls benches/tasks/*.toml \| wc -l` | 31 (matches docs) |
| Crate count | `ls crates/ \| wc -l` | 16 (matches docs) |
| crates/ `#[test]` | `grep -r '#\[test\]' crates/ --include='*.rs' \| wc -l` | **1670** (README claims 1649 — stale by 21) |
| testdoctor `#[test]` | `grep -r '#\[test\]' crates/kirkforge-testdoctor/ --include='*.rs' \| wc -l` | **96** (state.md/CHANGELOG claim 88 — stale by 8) |

**Honesty note:** I cannot claim "gates green" for test/clippy this session — the test run timed out and clippy was not run. The fmt + check gates are green. The count verifications are my own shell output, not paraphrased. HEAD at review time: `d447fb3` (dev).

---

## Findings by severity

### Critical

**C1. `computer_use` `evaluate` runs model-supplied JS with no network sandbox → SSRF via the browser**
- `src/tools/computer_use.rs:453-456` — `args["expression"]` is passed straight to `session.evaluate(expression)`.
- The only URL gate (`host_is_literal_internal_ip`) runs on the `url` arg of `open`/`navigate`. Once a page is loaded, `evaluate` can run `fetch('http://169.254.169.254/...')` *from inside the browser*, bypassing the host-level check. The browser has no `--proxy-server` / `--host-resolver-rules` (see `src/main/chrome_launcher.rs`). A prompt-injected page (or a model driven to one) exfiltrates cloud metadata.
- Fix: launch Chrome with `--proxy-server`/`--host-resolver-rules` that block RFC1918 + link-local, or sandbox `evaluate` to deny `fetch`/`XMLHttpRequest` to non-allowlisted hosts.

**C2. `load_from_dir` skips `PluginManifest::validate()` on the primary bulk-load path**
- `crates/kirkforge-plugin-host/src/lib.rs:175` calls only `validate_api_version()`, never `validate()`.
- `load_one` (line 322) *does* call `validate()` and surfaces every error as a warning — but `load_from_dir` is the path the plugin loader uses. Manifest schema errors (bad name, bad semver, duplicate triggers, unknown hook events, untrusted command paths) are silently accepted during normal plugin load. This defeats the WO 8.8 "show every issue at once" contract for the load path that actually runs in production.
- Fix: add `if let Err(errs) = plugin.manifest().validate() { for e in errs { warnings.push(format!("{}: {}", path.display(), e)); } continue; }` after line 178.

**C3. `ci.yml:294` "Fail if success rate drops below 10%" is gate theater — never fails**
- `.github/workflows/ci.yml:294-306` — the step is *named* `Fail if success rate drops below 10%` but its body only emits `::warning::` and the `if` branch (the warning) runs when the rate is below 10%, then exits 0. A sub-10% bench success rate passes CI.
- This is exactly the anti-pattern AGENTS.md §4 forbids ("Do not add `|| true`, `|| echo "non-fatal"`... to make red go green"). The real regression gate lives in `bench-baseline.yml:170` (`bench-pr-delta` with `--fail-on-regression 10`), so this step is vestigial *and* misleadingly named.
- Fix: rename to "Warn if..." or make the `if` branch `exit 1`.

### High

**H1. `web_fetch` has no DNS-rebinding check; only literal-IP hosts are blocked**
- `src/tools/web_fetch.rs:91-95`, acknowledged at `:176-178`. A model fetching `http://attacker.example.com/` whose A record resolves to `127.0.0.1` or `169.254.169.254` defeats the prefix-match deny-list. Cloud-metadata exfil path.
- The code comment already names the fix ("pin the resolved IP and re-check it"); it's just not implemented.

**H2. `bash` Docker bind-mount string is built from user `workdir` with no sanitization; sandbox gate bypassed on the Docker path**
- `src/tools/bash.rs:62-63` — `format!("{workdir_str}:/work")`. If the model passes `workdir: "/tmp/evil:/etc:ro"`, Docker parses the first `:` as the host/container split and mounts `/tmp/evil` at `/etc`.
- Worse: the Docker branch (`run_docker`) does **not** call `check_bash_command_str` at all — only the foreground `run_shell_with_token` path does. With `docker_config.enabled = true` the deny-list/sandbox-workdir gate is bypassed for every command (only the `docker run` invocation itself is built from config, not gated).
- Fix: canonicalize + sandbox-contains-check the workdir before string-formatting the bind mount; route the inner `cmd` through `check_bash_command_str` on the Docker path too.

**H3. Bedrock `parse_bedrock_event_stream` has an unbounded `envelope_buffer` and drops events after the first in a chunk**
- `src/adapters/anthropic_bedrock.rs:127` — `envelope_buffer.extend_from_slice(chunk)` grows without limit. If `extract_payload` returns `None` (the `{"type"` substring isn't found, e.g. a binary prelude or a split chunk), the buffer is never cleared → OOM.
- `:140-145` — `if let Some(inner) = extract_payload(&envelope_buffer) { ... envelope_buffer.clear(); }`. If a single chunk contains multiple event-stream frames (common with Bedrock's chunked encoding), only the first is parsed; the rest are discarded by `clear()`. Mid-turn tool-call deltas are silently dropped.
- The downstream `parse_anthropic_stream` has `MAX_SSE_BUFFER_BYTES = 8 MiB`; this outer buffer has no cap.

**H4. `vertex_auth::service_account_token` silently returns `""` on token-fetch failure**
- `src/adapters/vertex_auth.rs:44` — `Ok(token.token().unwrap_or_default().to_string())`. A `None` token (yup-oauth2 returns it when the token endpoint fails or scopes are wrong) becomes `""`. The Vertex adapter then sends `Authorization: Bearer ` (empty); GCP returns a generic 401 and the actual cause (empty token) is hidden.
- Fix: `token.token().ok_or_else(|| anyhow!("service account token endpoint returned None"))?`.

**H5. `Bash::run_docker` calls `.expect("docker_config is Some")` on `Option<DockerConfig>`**
- `src/tools/bash.rs:50`. The current caller guards with `docker_config.as_ref().map_or(false, |c| c.enabled)` before calling, so it can't fire today — but the invariant is implicit. A future call site panics. Use `?` or return `ShellError::Spawn`.

**H6. Bundled `kirkforge-plugin3` manifest declares a hook event not in `KNOWN_EVENTS`**
- `plugins/kirkforge-plugin3/kirkforge.toml:94` declares `event = "post-tool-write_file"`, but `crates/kirkforge-plugin/src/lib.rs:297-305` `KNOWN_EVENTS` only allows `session-start, pre-turn, post-turn, pre-tool-bash, post-tool-bash, pre-compact, post-compact`. The runtime emits `post-tool-write_file` (`src/session/budget.rs:664`), so the hook set is correct but the validator allowlist is stale.
- Masked by C2 (the validate() call that would catch this is skipped on the primary load path). Fix: add `post-tool-write_file` (and any other emitted-but-unlisted events) to `KNOWN_EVENTS`.

**H7. `dispatch_tool_call_batch` Phase 2 spawns tasks after `cancelled` flips, leaking detached tasks**
- `src/session/executor/turn.rs:1066-1096`. When cancellation arrives mid-spawn, already-spawned `JoinHandle`s in `running` are abandoned by the `break` and never awaited — the tasks run to completion detached, holding subprocess/network resources. Each tool has its own per-tool timeout, so it's bounded, but a slow bash command can outlive the user's cancel by up to `tool_timeout_secs` (default 30s, max 3600s).
- The `tool_cancel_token` is built from the `cancelled` flag at prep time, so tasks spawned before the flag flipped don't see cancellation.
- Fix: propagate `cancelled` into `run_prepared_call` and abort un-awaited handles on `break`.

**H8. `init_default_verifiers` spawns bus-handler registration fire-and-forget; if it fails the executor silently runs with fewer verifiers than reported**
- `src/session/executor/mod.rs:518-548`. `rt.spawn(async { if let Err(e) = bus.register(h).await { tracing::warn!(...) } })` is not awaited. The returned `count` includes the handler even if registration later fails. The executor reports "6 verifiers registered" when only 5 are.
- Defense-in-depth (the bus is advisory, not a safety gate), but a silent-degradation pattern.

**H9. Model-name prefix hardcoding in the trait layer (`adapter_kind_for`)**
- `src/adapters/mod.rs:165-208` — `match model_name.to_lowercase().starts_with("glm"|"deepseek"|"gemini"|"kimi"|"moonshot"|"claude-"|"anthropic.claude-"|...)`. A new model (e.g. `qwen3-max`) silently falls through to `OpenAiCompat`, which may or may not be correct. Each new model requires a code change here. The `ModelAdapter` trait is provider-agnostic but the *factory* hardcodes provider names — the central abstraction leak.

**H10. `OpenAiCompatAdapter::model_info` hardcodes vision/cache support by model name; emits Anthropic-style `cache_control` markers for gpt-4o**
- `src/adapters/openai_compat/mod.rs:408-437` — `is_claude3 || is_gpt4o || is_gpt5` sets `supports_cache: true`, then the body builder emits `{"cache_control": {"type": "ephemeral"}}` which gpt-4o ignores or rejects depending on the proxy. A `gpt-4.5`/`gpt-6` model gets `supports_images: false, supports_cache: false` even though it almost certainly supports both. Same leak class as H9.

**H11. `ci.yml:388` `cargo audit || true` masks all RUSTSEC advisories**
- `.github/workflows/ci.yml:388` — `cargo audit || true` swallows the exit code. Any dependency vulnerability passes CI. The release workflow does not exclude `audit` from its "all checks must pass" gate, but since `audit` always passes, a vulnerable dependency would not block a release. AGENTS.md §4 forbids `|| true` to make red go green.
- Fix: `continue-on-error: true` on the step (visible in the run UI) rather than `|| true` (hides the exit code), or make it a real gate.

**H12. `scripts/ci-local.sh` diverges from CI — missing coverage gate and explicit `adr_xref_drift`**
- `scripts/ci-local.sh` runs fmt/test/clippy/check/release-build/audit but **not** the tarpaulin coverage threshold check and does not call `cargo test -p plugin3-core --test adr_xref_drift` explicitly (AGENTS.md §7 calls this out as a required gate when ADRs are touched). `adr_xref_drift` is covered by the local `cargo test --workspace` run so it's lower severity than it first appears, but the coverage gate has no local equivalent — a contributor can pass `ci-local.sh` and fail the `coverage` CI job.
- Fix: add an optional `ci-local.sh full` mode that runs tarpaulin with the same thresholds as `ci.yml:336-364`.

### Medium

**M1. `CorrectionResult.verifier` is hard-coded to `"verifier"` on the event-driven path**
- `src/session/verifier/correction.rs:107-126`. The executor writes `tool_name: format!("verifier:{}", cr.verifier)` into the conversation, so the model sees `verifier:verifier` for every correction — useless for diagnosing which verifier (lint/build/git/rustfmt/test) fired. The `BusVerifier` path (`dispatch.rs:161`) does it right (`format!("{}", entry.source)`).

**M2. `EventBus` idempotency key for `BashExec` is `command` only — repeated commands dedup and skip the verifier**
- `src/session/event_bus.rs:130-132, 402-426`. Two `git add .` calls in a batch produce the same idem key; the second dispatch's handlers (including the git verifier) never see the event. The `exit_code`/`stdout_len`/`stderr_len` are in the struct but not hashed. The in-flight guard also spans the entire handler fan-out (a 30s `cargo clippy`), not just the dedup check.
- Fix: include `exit_code`/`stdout_len`/`stderr_len` in the `BashExec` idem key; same for `FileWrite` (currently `path + content_length` — overwriting the same file twice with the same length dedups).

**M3. Phase 3 `record_tool_result` re-runs `path_guard.check_write(path)` even though Phase 1 already returned `GuardVerdict::Allowed(resolved)`**
- `src/session/executor/turn.rs:723-737`. Wastes a second canonicalize + sandbox-contains check + a second `git check-ignore` subprocess spawn per edit. Worse: a TOCTOU window where a parallel tool in the batch flips the guard state between Phase 1 and Phase 3, producing a denial for a tool that already executed.
- Fix: carry `resolved` from Phase 1 into Phase 3 and skip the re-check, as the `pre_run_verdict` docstring already claims it does.

**M4. Phase 2.5/Phase 3 double-records `AccessDenied` for denied file edits**
- `src/session/executor/turn.rs:1163-1214, 1223-1300`. A deferred file call denied by the read gate inserts `ToolOutcome::Failure(AccessDenied)` into `results`; Phase 3 then sees `has_result = true` and calls `record_tool_result`, which re-runs the path guard + read gate and emits a fresh denial message. The model sees two identical "Access denied" tool results for one failed edit.

**M5. `bedrock_signing::sign_request` silently drops headers with non-ASCII values**
- `src/adapters/bedrock_signing.rs:83-96`. A header value that isn't valid HTTP header chars is dropped; the signed request would be missing it on the wire but present in the signature, causing a 403 from Bedrock with no local error. The `Authorization` header is always ASCII today, so probably fine in practice — but a future UTF-8 user-agent would break silently.

**M6. `ResponseCache::get` reads the disk file with no size cap**
- `src/adapters/cache.rs:64` — `std::fs::read(&path).ok()?`. A corrupted or crafted multi-GB cache file is read entirely into memory before `serde_json::from_slice` fails → OOM. `put` has no size guard either.

**M7. `CacheKey` uses `DefaultHasher` (SipHash-1-3) with a fixed seed — not collision-resistant**
- `src/adapters/cache.rs:131-158`. The doc comment says "content-addressed by hash of inputs" but `DefaultHasher::new()` is deterministic. Two different `(messages, tools)` tuples that hash to the same 64-bit value return the wrong response. ~2^-64 per pair — unlikely but the "content-addressed" claim overclaims.

**M8. Anthropic `parse_anthropic_stream` EOF drops a pending tool_use with no input silently**
- `src/adapters/anthropic.rs:454-461`. If `content_block_start` arrives but the connection drops before any `partial_json`, `pending_tool.input.is_none()` is true and the tool is dropped (the `if tool.input.is_some()` guard). The executor sees a `Done` with no preceding `ToolCall` and assumes the turn was empty — the model's intent is lost.

**M9. `AnthropicAdapter::model_info` hardcodes `max_context_tokens: 200_000` for all Claude models**
- `src/adapters/anthropic.rs:59` (also `anthropic_bedrock.rs:63`, `anthropic_vertex.rs:80`). Claude 3.7 Sonnet is 500k (with beta), Claude 4 is 1M. A user on 3.7 sees the TUI truncate at 200k. The `supports_thinking` check already special-cases `claude-3-7-sonnet` and `claude-4`; the context window could too.

**M10. `build_anthropic_body` hardcodes `max_tokens: 8192`**
- `src/adapters/anthropic.rs:197`. Every Anthropic request, regardless of model or config, asks for 8192 output tokens. Claude 3.7 supports 64k, Claude 4 supports 128k. A user who needs a long tool-call chain sees truncated output. Should be configurable via `Config`.

**M11. `PluginTool::execute` (host crate) does NOT apply rlimits — only `PluginToolWrapper` (binary crate) does**
- `crates/kirkforge-plugin-host/src/tool.rs:73-79` spawns with plain `Command::new`, no `setup_rlimits`. ADR-060 says rlimits apply to "plugin tools" but the host-crate `PluginTool` is a separate, unhardened spawn path. The ADR's wording is ambiguous; the host crate is unprotected. The binary-crate `PluginToolWrapper` (`src/session/plugin_tools/wrapper.rs:226`) does call `setup_rlimits`.

**M12. Verifier bus env-var contract differs between the two paths**
- Legacy `PluginVerifierAdapter` (`src/session/verifier/plugin.rs:64-67`) sets `KF_VERIFIER_NAME`/`KF_EVENT_KIND`/`KF_EVENT_JSON`; bus path `PluginBusVerifier` (`src/session/verifier/bus.rs:289-297`) sets `KF_VERIFIER_NAME`/`KF_CHANGED_FILES` (no `KF_EVENT_*`). A plugin verifier script relying on `KF_EVENT_JSON` works under the legacy path but gets nothing under the bus path (the preferred path per ADR-028). Not documented as a divergence.

**M13. `bash_minify::extract_file_path` reads files outside the sandbox/deny-list**
- `src/tools/bash_minify.rs:345-357`. `try_minify_bash_output` extracts a path from the *command string* and calls `minify_source_safe` which reads the file from disk — bypassing `PathGuard::check_read` and `DenyList`. A benign-looking `cat /tmp/output` where `/tmp/output` is a symlink to `~/.ssh/id_rsa` follows the symlink with no recheck. (The safety gate would block `cat ~/.ssh/id_rsa` directly, but not a symlink target.)

**M14. `bash_jobs::BashJobRegistry` watcher tasks are detached; a panic leaves the job in `Running` forever**
- `src/session/bash_jobs.rs:178-280`. The watcher `tokio::spawn` is detached; a panic in `child.wait()` is silently swallowed (no panic hook). Combined with the `unwrap_or_else(|e| e.into_inner())` on the jobs mutex (`:119, :147`), a panicked watcher can leave the registry inconsistent with no operator-visible warning.

**M15. `atomic_write` does not fsync the parent directory after rename**
- `src/tools/atomic_write.rs:45-54`. `write_fsync_rename` syncs the temp file then renames, but the parent dir's dirent update is not fsynced. A power loss after a successful `rename` can still lose the file. For a coding agent probably acceptable (`git checkout` recovers), but the function name `atomic_write` overclaims.

**M16. `apply_command_fix` in the correction loop spawns the verifier-supplied command with the user's environment**
- `src/session/verifier/correction.rs:198-257`. The `command` field on `FixSuggestion` is `"rustfmt"` for the built-in, but the `Verifier` trait is public — a plugin verifier can return `FixSuggestion { command: Some("/tmp/evil.sh") }`. No `check_bash_command_str`, no PATH sanitize, no env clear. Plugins are already trusted for tool execution so this is consistent with the threat model, but it should be documented.

**M17. Bench `verify_only` reports SKIP as `success: true`**
- `crates/kirkforge-bench/src/lib.rs:466-478`. A `requires_model` task returns `success: true, error: Some("skipped (requires model)")`. A CI gate that sums `success` counts SKIPs as passes; the data model can't distinguish SKIP from PASS without inspecting `error`. A consumer aggregating `success_rate` inflates the rate.

**M18. `compare_with_threshold` uses `< -threshold` (strict) — an exactly-10pp drop is NOT a regression**
- `crates/kirkforge-bench/src/lib.rs:377`. The doc says "a drop from 80% to 69% is a regression" at threshold 0.10 (which is -11pp < -0.10, correct), but an exactly-10pp drop (80% → 70%, delta = -0.10) is `-0.10 < -0.10` = false → not a regression. The test uses -8pp so it doesn't pin the boundary. Off-by-one in the boundary condition.

**M19. state.md "Deferred items" table falsely lists `use_workflow_run` as deferred**
- `state.md:52`. WO 9.1 is **Done** (`docs/workorders/9.1-workflow-tool-wrapper.md:3`), `WorkflowTool` exists (`src/tools/workflow.rs:24`), `benches/tasks/use_workflow_run.toml` exists. state.md L5 even says "the `use_workflow_run` task shipped in WO 9.1" — contradicting L52 in the same file. AGENTS.md §7 warns: "Multiple items listed as 'open' turned out to be already shipped. Thirty seconds of grep saves an hour of duplicate work." This is that anti-pattern, live.

**M20. ADR-066 says "30 tasks" in four places while the rest of the docs say 31**
- `docs/adr/066-kirk-bench-spec.md:10,15,31,62,70`. WO 14.7 (which ADR-066 pins) added `token_budget_challenge.toml` (the 31st task) and `use_workflow_run.toml` shipped in WO 9.1 — both pre-existing relative to ADR-066's 2026-07-30 date. The ADR was not updated to reflect the count it itself increased. `TECHNICAL.md:565` and `KIRK-BENCH.md:10` correctly say 31.

**M21. KIRK-BENCH "40 tasks" arithmetic doesn't add up**
- `KIRK-BENCH.md:3,256`. Headline says "40 tasks"; closing deferral says "the spec documents 40 tasks; this workorder builds the signature one and maps the existing 31. The remaining ~9 are future WOs." But the "Planned tasks" table (`KIRK-BENCH.md:234-254`) lists **19** planned spec-task numbers. 31 implemented + 19 planned = **50**, not 40. The "~9" deferral count is also wrong (it's ~19). Undermines the spec's authority as the contract.

**M22. `apply_ignore_slow` regex matches any `fn test_foo(` — not anchored to a test attribute**
- `crates/kirkforge-testdoctor/src/apply.rs:67`. Matches any `fn test_foo(` on its own line without checking the line above is `#[test]`/`#[tokio::test]`. A non-test helper named `test_foo` gets an `#[ignore]` added. The function only checks for an *existing* `#[ignore]` above, not for a test attribute.

**M23. `apply_env_guard` does not add the `use EnvGuard` import**
- `crates/kirkforge-testdoctor/src/apply.rs:170-185`. `apply --yes` produces code that won't compile without a manual `use` addition. Documented as intentional, but `--yes` should either add the import or refuse to write.

**M24. `gaps.rs::DEFAULT_THRESHOLDS` hardcodes CI thresholds with no drift test**
- `crates/kirkforge-testdoctor/src/gaps.rs:55-59`. `src/session=68.0, src/tools=76.0, src/adapters=75.0` with a "Must match the CI thresholds" comment. `default_thresholds_match_ci` (line 416) only asserts the hardcoded values match themselves, not that they match `.github/workflows/ci.yml`. A CI threshold change drifts silently.

**M25. `git_sanitation` reads only the first 1 MiB of each file**
- `src/session/git_sanitation.rs:22` (`SCAN_CAP_BYTES = 1024*1024`). A secret placed after the 1 MiB mark in a large generated file (bundled asset, lockfile with trailing comment) passes the scan. The user sees "clean" when their secret is still in the file. Not documented in the commit-blocker message.

**M26. `trufflehog_scan` runs with the user's environment and no timeout**
- `src/session/verifier/security.rs:175-208`. If `trufflehog` hangs (network, git fetch) the verifier blocks the correction loop indefinitely. The other verifiers spawn `cargo` which has its own timeouts; `trufflehog` does not.

### Low

**L1. `format_verdict_report` slices `&file_line[..23]` without checking char boundaries** — `src/session/verifier/bus.rs:193-197`. Panics on a path containing a multi-byte UTF-8 char at byte index 22 (e.g. `café.txt`). `truncate_tool_output` in `helpers/mod.rs:195-199` does the right thing; this site does not.

**L2. `PostTurnHookGuard::drop` runs the hook synchronously in `Drop`** — `src/session/executor/turn.rs:28-46`. `HookRunner::run` is fire-and-forget (spawn + return) so normally microseconds, but a blocked spawn (EMFILE, fork failure) blocks the drop.

**L3. `worktree.rs::WorktreeSession::create` interpolates `session_id` into a path with no validation** — `src/session/worktree.rs:14-38`. `session_id` is internally generated today (`{date}-session-{seq}`, safe), but the signature accepts `&str` — a future caller passing user-controlled input with `..` or `/` escapes `temp_dir()`.

**L4. `executor/loop_.rs:168` cancel-watcher task is detached** — the `tokio::spawn` JoinHandle is dropped. Clean exit on normal shutdown (`cancel_rx` closes); on abnormal shutdown the send fails and logs a warn. Cosmetic.

**L5. `executor/scout.rs::StubTool::run` uses `unimplemented!`** — `:138`. If a future test calls `run` on a stub, it panics instead of returning `ToolOutcome::Error`. Test-only.

**L6. `collect_carryover` substring-matches `cmd.contains("cargo test")`** — `src/session/executor/mod.rs:908-920`. `echo "cargo test is great"` would increment `record_test_after_change`. Heuristic, not a safety gate.

**L7. `synth_status_killed` partial stdout loss on timeout** — `src/session/bash_runner/mod.rs:475-492`. If the drain task hadn't copied the last few KB before the kill, the partial stdout is silently dropped. Mitigated by the `[timed out after N seconds]` marker.

**L8. `bash_jobs` watcher race: child handle removed by `cancel()` between spawn and watcher registration** — `src/session/bash_jobs.rs:170-194`. Status is right (`Cancelled`), output is wrong (empty `stdout`). The drain tasks are never started in that case.

**L9. `bedrock_signing` envelope buffer OOM** is H3; the `extract_payload` `{"type"` literal-match is also fragile — `src/adapters/anthropic_bedrock.rs:163`. A payload with different key ordering or whitespace (`{ "type"`) is silently dropped.

**L10. `m5_tests.rs` lives in `src/adapters/` as a `#[cfg(test)]` sibling module** — `src/adapters/mod.rs:242`. Unusual but correct; could fold into `mod.rs`'s own `#[cfg(test)] mod tests`.

**L11. `vertex_auth::key_file_looks_valid` accepts any valid JSON, not just service-account JSON** — `src/adapters/vertex_auth.rs:48-54`. `[1,2,3]` passes. Dead in production (only tests call it). Should check `"type": "service_account"`.

**L12. `CachingAdapter::stream` forwarder task drains the inner adapter after the consumer drops** — `src/adapters/caching.rs:86-116`. If `rx_out` is dropped before the inner stream produces `Done`, the task continues draining (and the network) for up to 30s. No `CancellationToken` in the `ModelAdapter::stream` trait.

**L13. `ReadFile::minify_above_bytes` has a stale `#[allow(dead_code)]`** — `src/tools/read_file.rs:9-11`. The field IS used at line 114. Remove the attribute.

**L14. `RealChromeTab` and `BrowserSessionOwner` have identical `ChromeTab` impls (~80 lines copy-paste)** — `src/main/chrome_launcher.rs:16-97` vs `:149-232`. Extract a single `ChromeTabImpl` wrapping `(_browser, tab)` or delete `RealChromeTab`.

**L15. `ManifestError::UnsupportedApiVersion` variant is never constructed** — `crates/kirkforge-plugin/src/lib.rs:597-598`. `validate_api_version` only matches the existing `V1`. Forward-compat placeholder but currently unreachable.

**L16. `plugin3-hosts::cursor`/`::aider`/`::kirkforge` are stub modules whose only test asserts the module path string** — `crates/plugin3-hosts/src/cursor.rs:22-27` etc. `ponytail: stub-only` — intentional but ships no behavior coverage.

**L17. `plugin3-core` README `| Tests | 1649 passing |` is stale** — actual `#[test]` count under `crates/` is 1670. Drift of 21 tests. Per AGENTS.md §7 this row "counts `#[test]` attributes under `crates/` only."

**L18. state.md/CHANGELOG "testdoctor total 88" is stale** — actual is 96. Drift of 8.

**L19. `compare_reports` defaults `difficulty` to `Easy` for tasks missing from both sides** — `crates/kirkforge-bench/src/lib.rs:312-316`. `all_names` is the union so this is unreachable in practice, but the fallback hides a logic gap.

**L20. `compare_with_threshold` within-threshold test uses -8pp, doesn't pin the boundary** — see M18.

**L21. `verify_task` for `TestPasses`/`CommandExitsZero` does not inherit the curated env** — `crates/kirkforge-bench/src/lib.rs:163-170`. A bench task's verify command inherits the test runner's env, which can flake under tarpaulin (the testdoctor's own `EnvGuard` suggestion warns about this).

**L22. `WorkflowExecutor::run` marks all `batch_outputs` as completed even if `run_batch` returns partial results** — `crates/kirkforge-workflow/src/lib.rs:257-344`. The default `run_batch` is all-or-nothing, but a custom `StepRunner` that returns partial results breaks the invariant.

**L23. ADR index README has a duplicate/stale prose list** — `docs/adr/README.md:111-149` repeats ADR-019..066 as bullets in addition to the Index table, but the prose list is missing ADR-056..065. The Index table is authoritative; the prose list is drifting.

**L24. `ci.yml:334` `--skip test_build_fork_tree_nests_children` name does not match the fixed flake** — state.md:70 says the flake was on `test_build_fork_tree_orphan_fork_is_a_root`. The skipped test is a different name. The belt-and-suspenders guard may be protecting the wrong test.

**L25. ADR-028 status transition (partial → full Accepted) is undocumented in the ADR body** — `docs/adr/0028-verifier-bus-unification.md`. The header + index agree (drift test passes), but the body has no amendment note explaining when it was promoted. The history lives only in state.md.

---

## Convention compliance check

| Convention (AGENTS.md §7) | Status | Notes |
|---|---|---|
| `anyhow` for errors | clean | consistent across subsystems |
| `Verifier` (event) vs `BusVerifier` (sync) coexist | clean | not unified in one pass, as required |
| `CorrectionResult` is a struct, not enum | clean | but `verifier` field is hard-coded to `"verifier"` (M1) |
| `bincode` rejected | clean | `serde_json` everywhere; `Cargo.toml:177` comment + `ponytail:` pin in context-index |
| `block_in_place` in single-thread runtime | clean | no `block_in_place` calls; `std::thread::sleep` in `verifier/bus.rs:439` is sync `BusVerifier::verify`, not async |
| `.map_or(true,...)` → `.is_none_or(...)` | clean | no violations found |
| `println!`/`eprintln!`/`dbg!`/`tracing::debug!` in committed code | clean | zero matches outside tests; only user-facing `eprintln!` (worktree Drop warning, `--harden` Windows warning, config banner) |
| `#[ignore]` to make red green | clean | all 16 `#[ignore]` carry honest reasons (Ollama/Docker/Chrome/cargo/spawn/timing); none mask red |
| `|| true` to silence failures | **violated** | `ci.yml:388` `cargo audit \|\| true` (H11); `ci.yml:294` "Fail if..." theater (C3) |
| `headless_chrome::Tab` weak-ref | clean | `BrowserSessionOwner { _browser, tab }` pattern respected |
| `ContextIndex` private `symbols` | clean | serialization uses separate `CachedIndex` struct |
| `PluginManifest::validate` returns `Result<(), Vec<ValidationError>>` | clean | but `load_from_dir` doesn't call it (C2) |
| ADR two-source-of-truth (header + index) | clean | `adr_xref_drift` green; both agree |
| Debug spam | clean | none |
| Dead code | clean | WO 14.8 removed 17+3 items + 740-line dead `dispatch_tool_call` |

The repo honors its own worker contract on every dimension **except** the two `|| true`-style CI gates (C3, H11), which are direct §4 violations. Those are the most consequential compliance gaps because they make "CI green" mean less than it claims.

---

## Architectural notes (positive)

1. **The bash safety gate is a single shared chokepoint.** Every shell path (model bash tool, `!` bang, `/test`, hooks, background jobs) routes through `check_bash_command_str`. The deny-list + normalization + word-boundary + IFS-evasion + ANSI-C-quoting + dangerous-redirection + tee + sandbox-workdir checks are comprehensive. The `normalize_for_safety` preprocessor is explicitly not a shell parser (the `ponytail: ceiling` annotation on `git.rs:71` documents the same philosophy). This is the correct architecture.
2. **The three-phase tool dispatch (`pre_run_verdict` → `run_prepared_call` → `record_tool_result`) is well-documented** but leaks at M3 (Phase 1/3 path-guard duplication) and M4 (denial double-record). A cleaner design carries the Phase-1 `GuardVerdict::Allowed(resolved)` into Phase 3.
3. **The `CorrectionLoop` iteration cap (3) and `DoomLoopTracker` (3 identical errors) are independent guards** — one bounds auto-fix retries, the other bounds the model repeating the same error. Together they prevent unbounded loops. Good design.
4. **The `UndoStack` atomicity is correct**: snapshot via temp+rename before the tool writes, metadata sidecar after, in-memory `ops` only after both are durable, FIFO trim only after the new snapshot is on disk. `MAX_ENTRIES=50` / `MAX_TOTAL_SNAPSHOT_BYTES=50MiB` caps are bounded.
5. **The `VerifierBus` + `EventBus` + `CorrectionLoop` triple is over-engineered for the current verifier count** (6 built-in + plugin). The `SecurityBusVerifier` and `GitBusVerifier` stubs in `bus.rs:244-267` are explicitly documented as "stub registers on the bus so it's counted" — they return `Vec::new()`. The actual security/git verification happens via the event-driven `Verifier` trait. This is the documented "don't try to unify them in one pass" decision, but `verifier_bus.verifiers().len()` reports 2 stubs that do nothing.
6. **The plugin signature verification, topological load, hot-reload, and resource-limit infrastructure is real and tested** (ADR-057/058/059/060). The gap is that `load_from_dir` (C2) bypasses the validation that would make the trust-tier system trustworthy on the primary path.
7. **The streaming parsers (NDJSON/SSE) correctly handle multibyte split chunks.** The Anthropic SSE parser was rewritten in WO 10.7 from a `data:`-only parser to a full `field: value` parser. The Bedrock envelope parser is the weak point (H3).
8. **The KIRK-BENCH `requires_model: bool` fix (WO 9.9) is the correct anti-pattern reversal** — verify specs now check *post-model* content, so `verify-only` correctly fails on the unedited setup. The `debug_log_trace.toml` task is a good example.

---

## Recommended fix priority

**Tier 1 — direct honesty/safety violations, fix first:**
1. C3 + H11 — fix the two `|| true`-style CI gates (rename `ci.yml:294` to "Warn if..." or `exit 1`; replace `cargo audit || true` with `continue-on-error: true`).
2. C2 — add `validate()` to `load_from_dir` (`kirkforge-plugin-host/src/lib.rs:175`).
3. C1 — sandbox the `computer_use` `evaluate` action (Chrome `--proxy-server` or deny `fetch` to internal ranges).
4. H2 — sanitize the Docker bind-mount workdir + route the Docker `cmd` through `check_bash_command_str`.
5. H1 — implement the DNS-rebinding pin-and-recheck the code comment already names.
6. M19 — fix the `use_workflow_run` false deferral in state.md (delete the row; it's done).

**Tier 2 — real correctness bugs, fix soon:**
7. H3 — cap the Bedrock `envelope_buffer` and stop dropping multi-event chunks (fixes both the OOM and the dropped tool-call deltas).
8. H4 — bubble the Vertex empty-token as an error, not `""`.
9. H7 — propagate `cancelled` into `run_prepared_call` and abort un-awaited handles on `break`.
10. M1 — replace `verifier: "verifier".into()` with the decisive verifier's `name()` in `correction.rs`.
11. L1 — use `is_char_boundary` before slicing `file_line` in `format_verdict_report` (prevents a real panic on non-ASCII paths).
12. M2 — expand the `BashExec` idem key to include `exit_code`/`stdout_len`/`stderr_len`.
13. M3 — carry `resolved` from Phase 1 into Phase 3, skip the re-check (removes the TOCTOU and the duplicate `git check-ignore` spawn).

**Tier 3 — doc drift and polish:**
14. M20/M21 — fix ADR-066 "30" and KIRK-BENCH "40" arithmetic.
15. L17/L18 — bump the stale test counts (1649→1670, 88→96).
16. H6 — add `post-tool-write_file` to `KNOWN_EVENTS`.
17. H12 — add an optional `ci-local.sh full` mode with the coverage gate.
18. L13 — remove the stale `#[allow(dead_code)]` on `ReadFile::minify_above_bytes`.
19. L14 — extract `ChromeTabImpl` or delete `RealChromeTab`.

---

## What this review did NOT verify

- **`cargo test --locked --workspace --no-fail-fast`** — timed out at 600s during this session. Not claimed green.
- **`cargo clippy --all-targets -- -D warnings`** — not run (3-4 min budget per AGENTS.md §7). Not claimed clean.
- **Integration tests** (`scripts/run-integration-tests.sh`) — require live Ollama + `qwen2.5:0.5b`; not run. Not part of the default gate per AGENTS.md §4.
- **Coverage gate** (tarpaulin thresholds) — not run locally. The `ci.yml:336-364` `coverage` job is the authority; `ci-local.sh` has no local equivalent (H12).
- **The four explore agents' findings are their analysis, not my own line-by-line read of every file.** I directly verified C1, C2, H2, H5, the `use_workflow_run` deferral, the test counts, and the two CI-gate-theater claims by reading the actual code/output. The remaining findings are cited to the agent that produced them and should be treated as high-confidence leads, not as independently re-verified.

---

## One-line summary

**Structurally strong; narratively drifted at the edges; two CI gates that don't gate and one browser-SSRF chain are the items that actually matter.**