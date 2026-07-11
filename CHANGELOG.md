# Changelog

All notable changes to kirkforge are documented here.
Format: [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [Unreleased]

### Fixed (deep audit — eighth pass)
- `npm/kirkforge-plugin/packages/tool-pyright/package.json` now declares `pyright` as a runtime dependency so the verifier ships a guaranteed binary instead of relying on a global install
- `plugins/kirkforge-plugin/tools/common.sh` `find_cli()` now resolves the JS entry point via `$KIRKFORGE_CLI_JS`, the source-layout sibling, or a global npm install of `@kirkforge/cli`; the unsafe PATH-installed `kirkforge` fallback is removed because it could select the Rust ELF binary or an npm shell wrapper, neither of which `node <path>` can execute
- `plugins/kirkforge-draw/tools/edit.sh` removed; it was never exposed in the manifest and cannot work in a null-stdin/non-TTY host environment
- `npm/kirkforge-plugin/packages/tool-tsc/src/index.ts` now resolves `tsc` from the bundled `typescript` dependency (or a local `node_modules/.bin` install) instead of `npx`, and accepts an optional `command` override for deterministic testing
- Bumped OpenTelemetry dependencies across `npm/kirkforge-plugin/package.json` and `packages/core-telemetry/package.json` to patched versions; `npm audit` now reports 0 vulnerabilities

### Fixed (deep audit — seventh pass)
- `src/session/mcp_client.rs` `McpClientManager` now collects startup warnings (failed MCP server connections, zero discovered tools) and exposes them via `warnings()`
- `src/main.rs` startup now prints MCP warnings to stderr so configured but unavailable MCP servers are visible instead of silently omitted

### Fixed (deep audit — sixth pass)
- Unified the data-directory env-var mutation lock across all tests (`src/session/mod.rs::test_data_dir_lock`) so `session_index`, `plugin_tools`, `tui/commands/plugins`, and daemon tests no longer race on `KIRKFORGE_DATA_DIR`; fixes the flaky `test_search_sessions_filters_by_id_and_date` failure seen in full `cargo test --workspace` runs
- `src/session/plugin_tools.rs` async installed-layout tests now acquire the shared lock via an async guard instead of `blocking_lock()` inside the Tokio runtime
- `src/session/mcp_client.rs` MCP server subprocesses now spawn with a sanitized PATH (same `bash_runner::sanitized_path` rules as model-driven bash and plugin tools) so a minimal or world-writable host PATH cannot shadow `npx`, `node`, or `bash`

### Fixed (deep audit — fifth pass)
- `src/session/mcp_client.rs` reader task now caps the *accumulated* JSON-RPC line length against `MAX_LINE_LEN`; the previous per-chunk check let a server stream an unbounded line in `BufReader`-sized pieces
- `src/session/bash_runner.rs` model-driven shell commands now resolve commands through a curated PATH that always includes standard system directories (`/usr/bin`, `/bin`, etc.) while still dropping relative and world-writable non-system entries; this fixes command resolution on hosts where a system directory happens to be world-writable
- `src/session/plugin_tools.rs` plugin tool subprocesses now inherit the same curated PATH as model-driven bash, so wrappers can always locate `sh`, `python3`, `node`, and other standard interpreters even when kirkforge is launched with a minimal or untrusted PATH
- `src/session/executor/helpers.rs` added lightweight dispatch-time schema validation (`validate_args_against_schema`) covering `required` fields and per-property JSON Schema types
- `src/session/executor/dispatch.rs` now validates tool arguments against the tool's JSON Schema before permission/approval logic, so malformed calls fail early with a clear error instead of reaching the tool
- `src/session/plugin_tools.rs` installed-layout stratum end-to-end test no longer mutates the global `PATH`; it copies the `stratum` binary next to the plugin script so the wrapper's sibling-binary discovery resolves it without racing other concurrent tests
- `src/session/bash_runner.rs` PATH-sanitization unit tests no longer mutate the global `PATH`, removing another source of parallel-test flakiness
- `build.rs` now propagates man-page render/write errors instead of panicking with `.expect`; a build-disk failure now produces a clean cargo error
- `crates/kirkstratum-core/src/config.rs` `PipelineConfig::default()` no longer panics if the embedded `config/pipeline.toml` fails to parse; it constructs the default struct directly, and the existing drift test still enforces parity with the TOML
- `crates/kirkforge-draw/src/render.rs` `format_validate_report_json` now returns `anyhow::Result<String>` instead of panicking on JSON serialization failure; `kfd --validate --json` propagates the error through the normal CLI failure path
- `crates/plugin3-cli/src/main.rs` `plugin3 self-check` no longer panics on internal slicing, store, or serialization failures; it now returns a `Result` and exits 1 with a diagnostic message so the host tool sees a clean error instead of a process abort
- `crates/kirkforge-video/src/pipelines/animated_explainer.rs` no longer panics if an asset or transcode plan entry is not a JSON object; the failure now propagates through the pipeline's `anyhow::Result` path
- `crates/kirkforge-video/src/pipelines/brief.rs` no longer panics on regex construction failure; it returns `None` and lets the caller continue without the stat

### Fixed (deep audit — fourth pass)
- `src/session/mcp_client.rs` reader idle timeout reduced from 5 minutes to 10 seconds so a frozen MCP server is detected quickly instead of keeping a dead client alive
- `src/session/mcp_client.rs` reader task now wakes every in-flight request with `McpError::Disconnected` when the connection drops, instead of letting each caller wait the full 30 s request timeout
- `src/session/mcp_client.rs` now routes JSON-RPC responses by a normalized string id (string or number), conforming to the JSON-RPC spec instead of dropping responses with string ids
- `src/tui/mod.rs` executor shutdown now aborts and awaits the executor task after the 3 s grace period, instead of detaching it and leaving side-effect work running in the background
- `src/tui/mod.rs` event-loop `tokio::select!` now uses `biased;` so keyboard/resize/shutdown events win over the 4 Hz slow-tick, matching the original intent
- `src/tui/mod.rs` now installs `SIGINT` (cross-platform) and `SIGTERM` (Unix) handlers that drive the same graceful shutdown Notify as pty-close, restoring the terminal and flushing state instead of killing the process
- `src/session/executor/approval.rs` approval flow now has a 5-minute timeout and defaults to denied, preventing a hung UI or missing handler from blocking the executor forever
- `src/session/executor/turn.rs` `run_turn_collecting` no longer deadlocks on high-volume turns: a forwarding task drains the bounded `TurnEvent` channel into an unbounded collector while the turn runs
- `src/session/executor/turn.rs` now checks cancellation between batched tool calls and short-circuits the rest of the batch when the user cancels
- `src/tools/read_file.rs` now enforces the same `PathGuard` deny-list/sandbox/symlink rules as `write_file`/`edit_file`; previously it could read files outside the sandbox or via symlinks
- `src/main.rs` no longer persists transient CLI flags (`--host`, `--auto-approve`, `--dry-run`) to `config.toml`; only `load_or_create_config` writes a default file on first run
- `src/main.rs` `init_tracing` now returns `Result` and reports an invalid `--log-level` as a clean error instead of panicking
- `src/session/config.rs` `load_config` now returns a parse-warning and `load_or_create_config` prints it to stderr so malformed `config.toml` is visible
- `src/session/config.rs` now expands `~` in `sandbox_dir`, `cache_dir`, `plugin_public_key_path`, `plugin_sources`, `allowed_write_dirs`, and `deny_paths` from both env vars and TOML
- `src/main.rs` now surfaces plugin-registry load failures and plugin warnings to stderr instead of leaving them in tracing logs only
- `crates/kirkforge-plugin-host/src/lib.rs` now detects and reports duplicate tool/skill/verifier names that would otherwise silently shadow each other across plugins
- `src/tools/atomic_write.rs` now creates temp files with `O_EXCL` (`create_new`) and an unpredictable name (pid + nanosecond timestamp + counter) to block symlink-race attacks on the temp file
- `src/session/access.rs` `is_gitignored` now runs `git check-ignore` in a bounded thread with a 2 s timeout instead of blocking indefinitely on a slow repo

### Fixed (deep audit — third pass)
- `plugins/kirkforge-plugin3/tools/plugin3_common.sh` `json_get_integer` now preserves an explicitly empty default, so `budget_set.sh` can detect a missing `ceiling` argument instead of silently setting the budget to `0`
- `plugins/kirkforge-draw/kirkforge.toml` no longer advertises the `draw_edit` TUI tool; the host runs tools with null stdin and no TTY, so an interactive editor cannot function
- `plugins/stratum/tools/common.sh` gained shared `stratum_args`, `json_get_string`, `json_get_integer`, `json_get_bool`, and `json_has_key` helpers with jq/python3/naive-bash fallbacks
- `plugins/stratum/tools/{run,apply,mode,rules,config_validate}.sh` now use the shared helpers, normalise empty args to `{}`, and treat `{"input":""}` as a valid (empty) payload instead of a missing field
- `plugins/kirkforge-plugin/tools/common.sh` now accepts a `KIRKFORGE_CLI_JS` override and falls back to a global npm install of `@kirkforge/cli`; shared `node_json_arg` / `node_json_file_arg` helpers catch invalid JSON and emit a clean tool error
- `plugins/kirkforge-plugin/tools/{verify,audit-verify,doctor,verify-workspace}.sh` now use the shared JSON helpers; `verify-workspace` accepts `file` as either a single path or an array and no longer splits on spaces
- `npm/kirkforge-plugin/apps/cli/package.json` now includes `"files": ["dist/"]` so `npm publish` ships the compiled entry points

### Fixed (deep audit — second pass)
- Executor→TUI `TurnEvent` channel is now bounded (10,000 events) with backpressure instead of unbounded growth
- Approval diff-preview reader now checks `canonicalize() → starts_with(cwd)` before opening any file; blocks `../../../../etc/passwd`-style read-leak via the approval dialog
- `notified_jobs` HashSet pruned each tick to registry-live IDs only — bounded at ≤64 entries instead of growing for the session lifetime
- Toolset startup `panic!` replaced with `anyhow::Result` propagation so a plugin inconsistency produces a clean error instead of a process abort
- `state.messages` display list capped at 2 000 entries; oldest 500 evicted when exceeded with index-based state (collapsed, expanded, search) remapped consistently
- Plugin shell wrappers hardened (second pass): removed legacy `KIRKFORGE_TOOL_ARGS` fallback, added `node` dependency checks for the JS plugin tools, fixed JSON escaping in `die_json` and the draw `post-turn` hook, made stratum tools default to `{}` when no args are provided, and corrected the stratum `config_validate` command-line order
- `kirkforge-video` `animated_explainer` pipeline no longer panics on I/O errors when writing artifact JSON; errors now propagate through the existing `Result` path
- Plugin READMEs and the plugin-host crate doc comment now document the canonical `KIRKFORGE_TOOL_ARGS_JSON` env var instead of the legacy `KIRKFORGE_TOOL_ARGS` alias
- Plugin tool working directory: empty/missing `sandbox_dir` now resolves to the user's current directory instead of the plugin installation root; `README.md` and `config.toml.example` updated to document the escape-hatch semantics
- Pre-tool decision hooks and lifecycle hooks now receive `KF_EVENT` and `KF_SESSION_ID` so plugin3 hooks can distinguish KirkForge from Claude-Code runtime mode
- Bundled plugin shell wrappers hardened: kirkforge-plugin no longer falls back to the Rust binary as a JS entry point, video/stratum optional flags are quoted, and `verify-workspace` safely splits space-separated file paths
- Release packaging now ships all five Rust binaries (`kirkforge`, `kfd`, `plugin3`, `stratum`, `kirkforge-video`); `install.sh` installs the suite and refuses native Windows shells
- `scripts/bump-version.sh` no longer runs `cargo check --locked` after a version bump, which previously failed because the lockfile was stale
- `plugin3-core` integration test `state_drift` now uses `EnvGuard` to prevent env-var leakage on panic
- Line-mode interactive editor no longer panics on concurrent `next_line` calls; returns a clean error instead
- `bash_runner` exotic-target timeout fallback no longer panics if the fallback `sh` command fails to spawn; the error now propagates as a `ShellError::Spawn`
- Release archives and `install.sh` now ship/install the bundled `plugins/` directory to `~/.local/share/kirkforge/plugins/`; workspace plugin sources fall back to the data directory when compile-time source paths are absent
- All five bundled filesystem plugins load without warnings; plugin3 hooks are dual-mode and emit proper KirkForge no-op responses when `KF_EVENT` is set
- `kirkforge-draw` render/edit tools use `--render` and correct argument handling for non-TTY execution
- New regression test `crates/kirkforge-plugin-host/tests/load_bundled_plugins.rs` verifies all bundled plugins load cleanly
- Dependency hardening: `bincode` replaced with `serde_json`, `paste` removed, `ratatui` upgraded to 0.30, and vulnerable/deprecated crates (`crossbeam-epoch`, `quinn-proto`, `anyhow`, `lru`) refreshed
- Flaky tests fixed in `plugin3-core` env guard and `shared::metrics` log rotation
- Release archives and `install.sh` now ship/install the bundled `npm/kirkforge-plugin` Node SDK so `kirkforge-plugin` shell tools (`health`, `doctor`, `tools`, `verify`, ...) work from an installed layout
- Added regression test `bundled_plugins_load_from_data_dir` that exercises the installed-layout plugin loading path
- Cleaned up `kirkforge-plugin` `find_cli` helper to only search the actual installed/repo layout (`npm/kirkforge-plugin` sibling to `plugins/` under the data directory) and removed misleading dead-path candidates; callers now report the real missing-Node-SDK reason
- Added installed-layout end-to-end regression tests that execute real bundled plugin tools through the host's `PluginToolWrapper`: `stratum_mode` (Rust-binary-backed) and `plugin_tools` (Node SDK-backed)
- Plugin tool subprocesses and lifecycle hook subprocesses now run with a null stdin instead of inheriting the host's terminal stdin; prevents tools such as `stratum_run` or the `kirkforge-draw` `post-turn` hook from blocking on interactive input or consuming user keystrokes
- `kirkforge-draw` `post-turn` hook only drains stdin when `KF_EVENT` is unset (Claude Code mode), so it no longer waits for terminal EOF under KirkForge
- `draw_edit` now fails with a clear message when stdin is not a terminal, instead of launching `kfd` into a captured/non-interactive plugin subprocess
- `stratum_run` schema and shell wrapper now accept an `input` field so inline context can be compressed without relying on the host to supply stdin; the `/stratum` skill prompt no longer claims the runtime pipes stdin
- `stratum_run` now treats a missing `input` field as an error instead of silently compressing an empty stdin stream; the schema marks `input` as required
- `stratum_apply` now requires a `file` field; it previously fell back to stdin which is empty under the host's null-stdin plugin execution, silently processing no input
- `kirkforge-video` manifest no longer marks `path`/`check`/`command` as required when the corresponding shell wrapper supplies a sensible default
- `src/session/plugin_tools.rs` now propagates plugin-directory read errors instead of silently defaulting to an empty warning list
- `src/session/mcp_client.rs` reader task now enforces a 5-minute idle timeout and a 1 MiB per-line cap so a misbehaving MCP server cannot hang or exhaust memory
- Bash tool and background job runners no longer hardcode `/bin/sh`; Unix keeps `/bin/sh`, Windows targets `bash` (Git for Windows / WSL) so the same safety gate applies
- Session daemon client is now stubbed on Windows so the CLI compiles and degrades to file-based session discovery; the `daemon` subcommand returns a clear unsupported-platform error on Windows
- Line-mode approval handler no longer assumes `/dev/tty` on Windows; it reads from stdin on Windows while Unix continues to use the controlling terminal
- Hardened `bash_runner` deny-list against quoting/whitespace/escape evasions: commands are normalized (strip comments, quotes, collapse whitespace, lowercase), and redirections/teed writes to system paths are detected with a tokenizer that tolerates optional spaces, fd prefixes (`2>`), clobber form (`>|`), and Windows/Git-Bash path variants (`C:\Windows`, `/c/windows`, etc.)
- `kirkforge-draw` and `stratum` shell helpers now look for their satellite binary next to the script (`<plugin>/tools/<bin>`) before the workspace target directory, so installed-layout plugin directories work when binaries are shipped alongside the wrappers
- `kirkforge-draw` `render.sh` now uses the shared `json_get_string` helper (jq/python3/bash fallback) instead of sed-only parsing, matching the robustness of the other filesystem plugins
- `kirkforge-draw` `edit.sh` now has a proper `#!/usr/bin/env bash` shebang and uses the shared JSON helper so it no longer relies on sed-only argument parsing
- `kirkforge-plugin` `verify.sh`, `audit-verify.sh`, and `verify-workspace.sh` now default an empty/missing `KIRKFORGE_TOOL_ARGS_JSON` to `{}` instead of exiting, matching the other Node SDK tools
- Extended the `clippy::unwrap_used` production lint to the satellite crates (`kirkforge-draw`, `kirkforge-video`, `plugin3`, `stratum`, and their core/host libraries) and fixed the resulting production unwrap sites.
- Satellite binary discovery in `kirkforge-draw`, `kirkforge-video`, `stratum`, and `kirkforge-plugin3` now also accepts `<bin>.exe` candidates, so the Windows release archives (which ship `.exe` binaries) work under Git Bash / WSL without requiring a separate PATH entry.

### Added
- `/plugins` slash-command family for runtime plugin mount/unmount: `list`, `enable <name>`, `disable <name>`, `reload`, `trust <name> <tier>`. The executor picks up the new registry snapshot on the next turn without restarting.
- `--log-level` flag (default `warn`; env `KIRKFORGE_LOG_LEVEL`); `RUST_LOG` still overrides
- `kirkforge completions <bash|zsh|fish|powershell>` — prints shell completion script
- Cargo.toml metadata: `repository`, `license`, `keywords`, `categories`
- Five built-in workspace plugin sources (`plugins/kirkforge-draw`, `plugins/kirkforge-video`, `plugins/stratum`, `plugins/kirkforge-plugin3`, `plugins/kirkforge-plugin`) are now registered by default and can be toggled on/off persistently with `/plugins toggle <name>`.
- `/plugins` slash-command family extended with `toggle <name>`, `sources`, `add <name> <path>`, `remove <name>`, and `setup` for managing workspace plugin sources.
- Source-level unification of all five satellite projects into this repo: Rust satellites build as `crates/*` workspace members and the KirkForge-Plugin SDK is vendored under `npm/kirkforge-plugin/`. The CLI, all satellites, and the plugin-host crate now build from a single workspace.

### Changed
- Default model changed from `deepseek-v4-flash:cloud` to `qwen2.5:7b` so fresh Ollama installs work out of the box
- `NO_COLOR` / `TERM=dumb` now detected at startup; falls back to line-mode instead of TUI

### Added (Phase 13 — testing, benchmarks, coverage)
- `src/lib.rs` library target so `benches/` and `tests/` can exercise real adapter/parser code without duplication.
- Criterion benchmark `benches/first_token_latency.rs` measuring NDJSON parser first-token latency.
- Mock Ollama server integration tests (`tests/mock_ollama.rs`) using `wiremock` so adapter streaming paths run in CI without a live model.
- Property-based tests for `edit_file` exact/fuzzy replacement invariants via `proptest`.
- Additional `ollama_ndjson` parser regression tests for malformed JSON, non-UTF-8 lines, transport errors, empty thinking, `done_reason` variants, and cached token shapes.
- Adapter-selection unit tests covering GLM/DeepSeek/Gemini/OpenAI-compat routing and override behavior.

### Fixed
- Vendored Node SDK (`npm/kirkforge-plugin`): `tool-pyright` now resolves the local `pyright` install before falling back to PATH, fixing test failures under vitest fork workers; CLI test helper no longer spawns every command twice; missing `e2e/smoke.test.ts` added.
- `kirkforge-video` integration tests skip when `ffmpeg`/`ffprobe`/`flite` are absent, and CI installs them so the suite stays green on stock Ubuntu runners.
- Config file (`~/.local/share/kirkforge/config.toml`) now created with `0o600` permissions instead of world-readable `0644`; all three write paths covered (create, hot-reload, `save_config`)
- TUI exit no longer hangs for minutes when an Ollama HTTP call is in-flight:
  - cancel signal sent before channel drop
  - `handle.await` wrapped in a 3-second timeout
- `/exit` and `/quit` slash commands now abort an in-flight model call before setting `should_exit`
- Approval dialog: `Q` / `Esc` deny without exit; `^C` deny and exit; hint line updated so users know how to escape
- Block-comment closer split across a line boundary no longer breaks syntax highlighting
- Model HTTP calls retry up to 3× on connect/timeout errors and 429/503 responses (exponential backoff: 1 s, 2 s, 4 s)
- Default deny list extended with `**/.gnupg/**` and `**/.aws/**`
