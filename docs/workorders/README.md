# Workorders — Planned and In-Progress Work

This directory contains numbered workorders that define scoped tasks for
KirkForge-Cli. Each workorder lists the problem, root cause, files to touch,
approach, gate, and done condition.

Completed series 6-45 (450 workorders) are archived in
[docs/archive/workorders/](../archive/workorders/) — index in that directory's README.
Only the two most recent series are kept live.

## Active series

### Series 46 — Post-WO-45 area audit + external review findings

14 workorders from the post-WO-45 codebase audit + GPT/Claude fresh-copy review.

| 46.1 | [FileAuditSink::flush advances hash chain before write succeeds — breaks tamper-evidence on partial failure](46.1-audit-flush-partial-failure-breaks-tamper-evidence.md) | Done |
| 46.10 | [context-index mtime_rebuild misses new files not in cache](46.10-context-index-cache-mtime-misses-new-files.md) | Done |
| 46.11 | [ci-merge.yml missing bench TOML [verify].type validation present in ci-pr.yml](46.11-ci-merge-missing-bench-toml-validation.md) | Done |
| 46.12 | [check-artifact-consistency grep-echo produces double output (false failure)](46.12-check-artifact-consistency-grep-double-output.md) | Done |
| 46.13 | [plugin_consent_ledger defaults to false — WO 45.61 fix only matters when ledger is opt-in](46.13-plugin-consent-ledger-default-off.md) | Done |
| 46.14 | [run_id/parent_run_id always None in workflow and scheduled jobs — WO 45.1 identity gap](46.14-run-id-threading-incomplete-workflow-jobs.md) | Done |
| 46.2 | [apply_patch_to_parent missing setup_process_group — grandchild survives timeout](46.2-parallel-orchestrator-patch-missing-process-group.md) | Done |
| 46.3 | [Daemon concurrency semaphore starved by long-lived instance push channels](46.3-daemon-semaphore-starved-by-instance-channels.md) | Done |
| 46.4 | [kf-memory-store eviction is count-only — TTL and max_entries limits are non-functional](46.4-memory-store-eviction-count-only-limits-nonfunctional.md) | Done |
| 46.5 | [minify_write_side serde default true vs Default false — config construction paths disagree](46.5-config-minify-write-side-serde-default-divergence.md) | Done |
| 46.6 | [EventBus inflight counter leaks when emit future is dropped — graceful_shutdown always times out](46.6-event-bus-inflight-leak-on-cancelled-emit.md) | Done |
| 46.7 | [WorkflowExecutor::run_fan_out does not honour cancellation mid-fan-out](46.7-workflow-fan-out-no-cancellation-mid-batch.md) | Done |
| 46.8 | [grep/glob spawn_blocking tasks not cancellable by tool timeout — hung subprocess leaks](46.8-grep-glob-spawn-blocking-not-cancellable.md) | Done |
| 46.9 | [bench compare --fail-on-regression bypasses telemetry shutdown via std::process::exit(1)](46.9-bench-compare-bypasses-telemetry-shutdown.md) | Done |
### WO 46 additions — DeepSeek + MiniMax audit findings

| 46.15 | [kf-memory-store self-deadlock in write_run_and_emissions fallback (CRITICAL)](46.15-memory-store-self-deadlock-write-run-and-emissions.md) | Done |
| 46.16 | [Plugin hot-reload watcher dropped at call site — silently dead](46.16-plugin-hot-reload-watcher-dropped.md) | Done |
| 46.17 | [Process group missing in 3 more subprocess spawn sites (CRITICAL)](46.17-process-group-missing-three-more-spawn-sites.md) | Done |
| 46.18 | [kf-orchestrator observation outcome stores routing mode, not verdict — empirical router learns nothing](46.18-orchestrator-outcome-stores-mode-not-verdict.md) | Done |
| 46.19 | [Daemon state mutex held across write_response .await — single wedged client stalls whole daemon](46.19-daemon-state-mutex-held-across-await.md) | Done |
| 46.20 | [Never-ending bash job blocks scheduler and graceful shutdown — unshutdownable daemon](46.20-never-ending-job-blocks-scheduler-shutdown.md) | Done |
| 46.21 | [web_fetch buffers entire HTTP body before size cap — OOM from untrusted server](46.21-web-fetch-unbounded-body-read.md) | Done |
| 46.22 | [McpClient::Http Drop arm empty — in-flight calls hang 60s on swap](46.22-mcp-http-drop-empty-inflight-hangs.md) | Done |
| 46.23 | [ResponseCache::put holds sync Mutex across disk write + unbounded in-memory cache](46.23-response-cache-mutex-across-disk-write.md) | Done |
| 46.24 | [9 atomic-write sites use predictable .tmp + no O_NOFOLLOW — TOCTOU symlink race](46.24-predictable-tmp-filenames-toctou.md) | Done |
| 46.25 | [ci-local.sh set -e defeats run_step — failing step kills CI before remaining gates run](46.25-ci-local-set-e-defeats-gate-summary.md) | Done |
| 46.26 | [handle_bench_run_models silently exits 0 on 0/N pass rate — CI blind](46.26-handle-bench-run-models-missing-zero-guard.md) | Done |
| 46.27 | [mtime_rebuild/incremental_rebuild silently drop cached embeddings](46.27-context-index-rebuild-drops-cached-embeddings.md) | Done |
| 46.28 | [prune_oldest_in_dir ignores keep semantics — silent leak of oldest sessions](46.28-prune-oldest-ignores-keep-semantics.md) | Done |
| 46.29 | [start_daemon leaks a zombie child on every invocation](46.29-start-daemon-zombie-child.md) | Done |
| 46.30 | [bench run_task env-var leak on error paths](46.30-bench-run-task-env-var-leak.md) | Done |
| 46.31 | [apply_budget_slice TOCTOU — state read then re-lock](46.31-apply-budget-slice-toctou.md) | Done |
| 46.32 | [Daemon Unix socket has no explicit perms + auth token length leaks via timing](46.32-daemon-socket-no-perms-token-leak.md) | Done |
| 46.33 | [openai_compat spurious Error on DONE-before-finish_reason ordering](46.33-openai-compat-spurious-error-done-ordering.md) | Done |
| 46.34 | [kf-budget-core InMemoryOffloadStore evicts arbitrary entries, not oldest](46.34-budget-core-offload-store-arbitrary-eviction.md) | Done |
| 46.35 | [Parallel tool batches leave a ghost streaming card and mis-pair result entries](46.35-parallel-tool-batches-ghost-streaming-card.md) | Done |
| 46.36 | [bash_runner normal-exit path can spuriously fail + leak grandchild holding the pipe](46.36-bash-runner-normal-exit-drain-failure.md) | Done |
| 46.37 | [web_fetch/web_search ignore tool cancel token — cancelled turn waits full 30s](46.37-web-fetch-web-search-ignore-cancel-token.md) | Done |
| 46.38 | [verify_task cmd.env() doesn't env_remove() first — leaked parent env affects gate](46.38-verify-task-env-not-stripped.md) | Done |
| 46.39 | [Doc drift batch: CLI about, lib.rs path, test-doctor refs, test count, stale Cargo.lock, install.sh dead paths](46.39-doc-drift-batch-fixes.md) | Done |

### Series 47 — Lean KirkForge convergence (size audit findings)

14 workorders from the post-WO-46 size audit. Target: ~110K prod lines → ~85-88K without losing the verification/context/budget thesis.

| 47.1 | [Table-driven verifier registration (11 of 16 verifiers share the same 90-160-line shape)](47.1-table-driven-verifiers.md) | Done |
| 47.10 | [send_or_warn! ceremony → single emit! macro (47 sites × 4-6 lines)](47.10-send-or-warn-ceremony.md) | Done |
| 47.11 | [Freeze/delete computer_use (513 lines, default-off mini browser framework)](47.11-computer-use-freeze.md) | Done |
| 47.12 | [Daemon becomes opt-in (~5.6K lines out of the default path)](47.12-daemon-opt-in.md) | Done |
| 47.13 | [TUI command diet (trim set ≈ 3-4K shipped lines)](47.13-tui-command-diet.md) | Done |
| 47.14 | [Unify the two verifier trait systems (LAST + riskiest)](47.14-unify-verifier-traits.md) | Done |
| 47.2 | [Generic env-override loader (91 hand-parsed KF_* vars in 4 layers)](47.2-generic-env-loader.md) | Done |
| 47.3 | [Delete the dead JWT/JWKS half of kf-rbac (keep the live RBAC core)](47.3-delete-kf-rbac.md) | Done |
| 47.4 | [Fold kf-routing + kf-memory-store into kf-orchestrator (0 direct src/ refs)](47.4-fold-routing-memory-crates.md) | Done |
| 47.5 | [Feature-gate bench + testdoctor out of the default binary (~5K lines)](47.5-devtools-out-of-binary.md) | Done |
| 47.6 | [Six live compression layers → two (stratum modes should map to 2 pipelines)](47.6-compression-layers-6-to-2.md) | Done |
| 47.7 | [# WO 47.6 — MCP transport trait (every op exists 3×: enum + stdio_* + http)](47.7-mcp-transport-trait.md) | Done |
| 47.8 | [Wire or delete the 9 fuzz targets (100% unwired dead weight)](47.8-wire-or-delete-fuzz-targets.md) | Planned |
| 47.9 | [Archive the completed workorder corpus (490 files, 45.6K lines = 74% of docs)](47.9-archive-workorder-corpus.md) | Done |

### WO 47 additions — multi-model audit findings (vetted)

23 workorders from vetting mm/ds/kimi/sonnet/gpt bug reports against the code. Top-severity claims verified at file:line before filing.

| 47.15 | [Secret-env scrub missing at 3 spawn sites: docker, shell hooks, verifier formatter](47.15-secret-env-scrub-three-missed-spawn-sites.md) | Done |
| 47.16 | [jobd: token-length timing oracle + world-readable socket (the disclosed WO 46.32 deferral)](47.16-jobd-auth-timing-oracle-and-socket-perms.md) | Done |
| 47.17 | [workflow_run template argument loads arbitrary workflow JSON via path traversal](47.17-workflow-template-path-traversal.md) | Done |
| 47.18 | [Bash tool foreground path has no safety gate — direct callers bypass pre_run](47.18-bash-foreground-gate-in-tool.md) | Done |
| 47.19 | [Verifier apply_text_fix symlink-swap TOCTOU + VerifierBus catch_unwind double-panic](47.19-verifier-apply-text-fix-symlink-tocou.md) | Done |
| 47.20 | [Response cache: key omits generation config (wrong-model replays) + 64MiB sync disk reads on the async path](47.20-response-cache-key-and-async-disk.md) | Done |
| 47.21 | [ensure_private_data_dir OnceLock caches the first path globally — variable data dirs break](47.21-ensure-private-data-dir-oncelock.md) | Done |
| 47.22 | [edit_file fuzzy fallback corrupts the trailing newline on multi-line matches](47.22-edit-file-fuzzy-newline-corruption.md) | Done |
| 47.23 | [panic=abort makes every catch_unwind containment guard dead code in release — contract drift](47.23-panic-abort-vs-catch-unwind-contract.md) | Done |
| 47.24 | [Glob: max_matches never early-stops the walk + base_dir not pre-guarded](47.24-glob-early-stop-and-base-dir-guard.md) | Done |
| 47.25 | [Workflow condition: field runs unsandboxed shell — deny-list only, no landlock](47.25-workflow-condition-landlock-bypass.md) | Done |
| 47.26 | [Verifiers run sequentially per turn (up to 7 min) + sync preamble reads + unbounded verdict cache](47.26-verifier-parallel-execution.md) | Done |
| 47.27 | [Memory extractor: secrets land in filenames + same fact added up to 14× + LIKE wildcard escape](47.27-memory-slug-secrets-and-dupes.md) | Done |
| 47.28 | [HTTP MCP bearer token silently ignored + EventBus buffer identity collision](47.28-mcp-bearer-token-and-event-identity.md) | Done |
| 47.29 | [Adapter wire fixes: Bedrock content-length signing, SSE line-anchoring, /v1 over-trim, Vertex URL-encoding](47.29-adapter-wire-format-fixes.md) | Done |
| 47.30 | [jobd: notify_one loses shutdown/reload + run_bash_job double-completion race](47.30-jobd-shutdown-and-double-completion.md) | Done |
| 47.31 | [ci-local failures[] integrity (tarpaulin bypass + compound run_step) + CHANGELOG 23 missing WO 46 entries](47.31-ci-local-integrity-and-changelog.md) | Done |
| 47.32 | [Docker bash path: unbounded output buffering + timeout/signal codes conflated](47.32-docker-output-caps-and-signals.md) | Done |
| 47.33 | [Web/read hardening bundle: IPv6 unspecified, DNS-pin fail-open, body-cap config, read_file streaming, computer_use SSRF gate, scan_files cap](47.33-web-and-read-hardening-bundle.md) | Done |
| 47.34 | [write_file parent-dir TOCTOU + permission glob matcher catastrophic backtracking](47.34-write-file-tocou-and-glob-matcher.md) | Done |
| 47.35 | [Untrusted-content delimiters for tool output (prompt-injection defense) + template push_value hardening](47.35-untrusted-content-delimiters.md) | Done |
| 47.36 | [Mutex-poison + expect hygiene batch + lib unwrap gate + test EnvGuard fixes](47.36-mutex-poison-and-expect-hygiene.md) | Done |
| 47.37 | [Test-theater batch: e2e scenarios accept any outcome + bench TOMLs missing requires_model + tarpaulin nightly verify](47.37-test-theater-batch.md) | Done |

## Conventions

- Each workorder is a single markdown file named `<number>-<slug>.md`.
- Status is one of: Planned, In Progress, Done, Superseded.
- The gate must match AGENTS.md §4 (fmt --check, check, clippy, test).
- When a workorder is done, update its Status to "Done" and note the commit SHA.
- When a workorder is superseded, update its Status and link to the replacement.
- The scratch `workplan.md` at the repo root (gitignored) is for the current
  task's working notes; the workorders here are the persistent plan.

### Series 48 — Post-WO-47 baseline audit findings

10 workorders from the fresh-area baseline audit (2026-08-28). 0 P0 / 4 P1 / 6 P2 — includes one WO 47.12 regression.

| 48.1 | [minify_python strips # inside string literals — corrupts source on read AND on disk](48.1-minify-python-string-corruption.md) | Done |
| 48.2 | [Pre-tool hooks run twice per file-tool call — second run is post-mutation (WO 43.30 tail)](48.2-pre-tool-hook-double-run.md) | Done |
| 48.3 | [WO 47.12 regression: Sessions tab shows 'No recent sessions' in default (daemon-less) config](48.3-sessions-tab-empty-without-daemon.md) | Done |
| 48.4 | [daemon ThreadsChanged opens the session-picker MODAL at every TUI startup](48.4-session-picker-modal-pops-at-startup.md) | Done |
| 48.5 | [MCP tools/call isError:true classified as Success — doom-loop breaker blinded](48.5-mcp-iserror-ignored.md) | Done |
| 48.6 | [set_json_mode(false) never clears response_format — hot-reload keeps json_object forever; Anthropic arm never worked](48.6-json-mode-toggle-never-clears.md) | Done |
| 48.7 | [tee -a bypasses the tee dangerous-path gate (gate only checks the token right after 'tee')](48.7-tee-append-gate-bypass.md) | Done |
| 48.8 | [minify VFS cache ignores preserve_tests on read — test-stripped entries served to the every-turn stem](48.8-minify-cache-ignores-preserve-tests.md) | Done |
| 48.9 | [--no-default-features build broken — 3 un-gated kf_budget_core refs (ADR-0017 documents this build as supported)](48.9-no-default-features-broken.md) | Done |
| 48.10 | [Windows daemon stubs return Ok(None) with no disk fallback — --attach errors, --auto-resume no-ops despite help promising fallback](48.10-windows-daemon-stub-no-fallback.md) | Done |
| 48.11 | [minify_shell drops heredoc body lines starting # — disk write-back deletes them](48.11-minify-shell-heredoc-corruption.md) | Done |
| 48.12 | [minify_js_like truncates at // inside regex literals — js/ts corruption on the disk write-back chain](48.12-minify-js-regex-literal-corruption.md) | Done |
| 48.13 | [minify ruby: same heredoc/# string blindness family](48.13-minify-ruby-heredoc-and-comment-blindness.md) | Done |
| 48.14 | [Startup picker gate misses can_run_tui conditions — headless runs crash (os error 6)](48.14-startup-picker-launches-where-tui-impossible.md) | Done |
| 48.15 | [collect_batch pattern-matches body-produced AccessDenied as gate denial — skips record_tool_result](48.15-collect-batch-denial-classification.md) | Done |
| 48.16 | [mark_read runs with no outcome check — failed reads satisfy the read-before-edit gate](48.16-mark-read-on-failed-read.md) | Done |
| 48.17 | [notebook_edit ships but is absent from pre_run file-tool list, symlink walk, and audit](48.17-notebook-edit-outside-file-tool-pipeline.md) | Done |
| 48.18 | [reload_config never re-pushes set_response_format — hot-reload with json_mode=false now DELETES a live response format](48.18-reload-config-response-format-regression.md) | Done |
| 48.19 | [normalize_for_safety mid-word # truncation + permission deny-glob case mismatch](48.19-normalize-hash-and-case-gaps.md) | Done |
| 48.20 | [Every non-nav key leaks through the open picker modal — 48.4 fixed k/j only](48.20-picker-non-nav-key-leak.md) | Done |
| 48.21 | [count_tokens minimal-build off-arm (bytes/4) under-estimates 25-50% on code/CJK — feeds context-fit truncation ladder](48.21-count-tokens-minimal-underestimate.md) | Done |
| 48.22 | [P0: nightly subprocess-lifecycle test(=bare_name) filter matches zero tests — the two timeout tests have NEVER run](48.22-nightly-subprocess-filter-matches-zero.md) | Done |
| 48.23 | [TECHNICAL.md self-contradiction: 13 satellite crates vs 10 (post-47.4 fold)](48.23-technical-crate-count-contradiction.md) | Done |
| 48.26 | [fallback_c_like inserts a space after every colon — std::cout becomes std: : cout on disk write-back](48.26-fallback-c-like-colon-corruption.md) | Done |
