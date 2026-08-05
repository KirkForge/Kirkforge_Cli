# kf-code Repo State — After Modularization Sprint

*Current-state-only. Resolved-issue archaeology lives in `git log`.*

## Branch

**`dev2`** at commit `287a263` — all completed work merged.

## What shipped this session

| Phase | What | Key commit(s) |
|---|---|---|
| Audit | Collapsed 4 Oellama adapters into OellamaAdapter + profile table | `bd2db8a` |
| Audit | Deleted key_file_looks_valid + 7 tests (dead code) | `43de4a6` |
| Audit | env_bool! macro for 20 bool override blocks | `6408d26` |
| Audit | Dropped _model_info param from build_ollama_chat_body | `9107c1f` |
| Audit | Dedup find_subseq/trim_ascii_whitespace, inline is_empty_object, delete dead code | `cb56d8c` |
| Audit | Batch: bedrock session_token, AnthropicBedrock profile, EventKind::all, unregister, send_reload, JobListEntry, tab_bar, Default impls, PRICING_FALLBACK, register_if, minify test gates | `d093c96` |
| Audit | Clippy lint fixes | `0533319` |

| Phase | What | Key commit(s) |
|---|---|---|
| 1+2 | Full `kirkforge` → `kf-code` rename + 16 crate renames | `ae0e37d` → `ccfbdb3` (6 commits) |
| 3 | EventBus deleted (-1565 lines), direct verifier calls | `59a1a4a` |
| 4 | Config drift guard: CONFIG_FIELD_COUNT + triple-copy test | `24417d1`, `13a355d`, `b1bfeb9` |
| 5 | CostTracking + SandboxEnforcer extracted from Executor | `f638481`, `080c7e0`, `160210c` |
| 6 | Data-driven model routing via `[adapter_routing]` config | `2e4d424` |
| 7 | ToolRegistry builder replacing `all_tools()` factory | `02e96dd` |
| Docs | AGENTS.md, CHANGELOG.md, TECHNICAL.md, state.md | `207dfe3` |
| Plugin toggle | Runtime enable/disable of plugins via `/plugins toggle` | `a4474da` |
| TUI tabs | F1-F5 tab system (Chat, Models, Plugins, Jobs, Settings) | `6ff1e95` |
| Approval panel | Full-width approval dialog (was 60-col centered popup) | `287a263` |

**Net impact**: ~2400 lines deleted, ~2500 lines added.

## Config drift guard (Phase 4)

The config system has a triple-copy pattern: struct fields, `merge_toml_into_config`, and `apply_env_overrides`. The drift guard catches mismatches:

- `CONFIG_FIELD_COUNT = 73` (ModelConfig=22, SecurityConfig=18, ToolConfig=26, SessionConfig=4, DisplayConfig=3)
- `MERGE_TOML_EXPECTED = 62` (55 top-level leaf keys + 7 `[computer_use]` sub-keys)
- `ENV_OVERRIDE_EXPECTED = 58` (KF_CODE_* env vars)
- 16 struct fields intentionally skipped by `merge_toml_into_config` (documented in test)
- 4 additional fields skipped by `apply_env_overrides` (Vec/array types without env representations)

## TUI tab system

F1-F5 switch between panels. Chat (F1) is the default and shows the existing conversation view. Other tabs render their content in the main area:

- **F2 Models**: Connection status, model info, adapter routing, token usage
- **F3 Plugins**: Plugin list with ON/OFF/— status, path, toggle hints
- **F4 Jobs**: Placeholder for scheduled job status
- **F5 Settings**: Key config values with reload hint

The active tab label appears in the status bar when on a non-Chat tab. On Chat, no indicator is shown (zero visual noise during normal usage).

## Approval dialog

The approval dialog was expanded from a 60-col centered popup to a full-width panel. This gives maximum readability for:
- JSON args preview (more content per line)
- Unified diff view (full-width context)
- Side-by-side diff (available on terminals >= 80 cols)

## Gate status

- `cargo check --workspace`: PASS
- `cargo fmt --check`: PASS
- `cargo clippy --all-targets -- -D warnings`: PASS
- `cargo test -p kf-code --lib`: 2879 passed, 1 failed (`bundled_node_sdk_tool_executes_via_host` — requires Node.js, pre-existing)
- `cargo test --workspace`: ~2910 passed across all crates

## Sub-crate rename mapping (for reference)

| Old name | New name | What it does |
|---|---|---|
| kirkforge (binary) | kf-code | Main CLI |
| kirkforge-plugin | kf-plugin-sdk | Plugin SDK |
| kirkforge-plugin-host | kf-plugin-host | Plugin host |
| kirkforge-lsp | kf-lsp | LSP client |
| kirkforge-workflow | kf-workflow | Workflow engine |
| kirkforge-context-index | kf-context-index | Tree-sitter symbol index |
| kirkforge-bench | kf-bench | Benchmark harness |
| kirkforge-draw-core | kf-draw-core | Drawing core |
| kirkforge-draw | kf-draw | Drawing TUI |
| kirkstratum-core | kf-compress-core | Context compression |
| kirkstratum-cli | kf-compress-cli | Compression CLI |
| kirkstratum-hosts | kf-compress-hosts | Compression host rules |
| kirkforge-video | kf-video | Video production |
| kirkforge-testdoctor | kf-testdoctor | Test diagnosis |
| plugin3-core | kf-budget-core | Token budget core |
| plugin3-cli | kf-budget-cli | Budget CLI |
| plugin3-hosts | kf-budget-hosts | Budget host schemas |

## Env var mapping

All `KIRKFORGE_*` env vars renamed to `KF_CODE_*`. Data dir is `~/.local/share/kf-code/`. Config dir is `.kf-code/`. Plugin manifests are `kf-code.toml`.

## Plugin architecture (for reference)

Four folded plugins use a "two-path dispatch" architecture (ADR-050):
- **stratum**: Compiled-in when `stratum` feature is on (default), shell fallback in `plugins/stratum/`
- **kf-budget**: Compiled-in when `budget` feature is on (default), shell fallback in `plugins/kf-budget/`
- **kf-draw**: Compiled-in when `draw` feature is on (default), shell fallback in `plugins/kf-draw/`
- **kf-video**: Compiled-in when `video` feature is on (**NOT default**), shell fallback in `plugins/kf-video/`

Runtime enable/disable via `enabled_plugins`/`disabled_plugins` in config. The `F3 Plugins` tab shows status.

Feature gates in Cargo.toml:
```
[features]
default = ["stratum", "draw", "budget"]
stratum = ["dep:kf-compress-core"]
draw = ["dep:kf-draw-core"]
budget = ["dep:kf-budget-core"]
video = ["dep:kf-video"]
```

## Next steps (prioritized)

1. **Stratum absorption**: Remove `stratum` feature flag, make it always-on. Requires removing 11 `#[cfg(feature = "stratum")]` gates and making `kf-compress-core` an unconditional dependency.
2. **Plugin simplification**: Remove shell fallbacks in `plugins/` for plugins that are always compiled-in (stratum, draw, budget). Video shell fallback remains since the feature is off by default.
3. **Jobs tab content**: Wire the F4 Jobs tab to show actual scheduled job status from the executor.
4. **Models tab enhancements**: Add model switching to the F2 Models tab.
5. **Permission panel**: Consider converting the approval dialog into a panel that replaces the input area (like Vix's approach)

## Rust toolchain

Rust 1.88.0 is installed at `~/.cargo/bin/`. Run `export PATH="$HOME/.cargo/bin:$PATH"` before any cargo commands.

## Known issues

- `bundled_node_sdk_tool_executes_via_host` test fails because Node.js and the kf-plugin SDK aren't built — pre-existing
- The `adr_0010_emission_site_block_uses_eprintln_for_errors` test in `kf-budget-core` fails — ADR vs impl drift, pre-existing