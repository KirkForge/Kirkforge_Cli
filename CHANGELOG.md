# Changelog

All notable changes to kirkforge are documented here.
Format: [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [Unreleased]

### Fixed (deep audit — second pass)
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
