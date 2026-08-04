# kf-code Repo State — After Modularization Sprint

*Current-state-only. Resolved-issue archaeology lives in `git log`.*

## Branch

**`dev2`** at commit `b1bfeb9` — all completed work merged. The `refactor/kf-code-rename-and-modularize` branch has the same content but `dev2` is the clean integration point.

Worktrees exist at `.worktrees/phase-{3..7}/` but are all merged into dev2. Phase 4 worktree was reset (broken macro code). Phase 3 worktree has the verifier bus commit.

## What shipped this session

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

**Net impact**: ~2400 lines deleted, ~1500 lines added.

## What DID NOT ship

| Item | Status | Why |
|---|---|---|
| Plugin simplification | Not started | Current architecture (feature-gated compilation + runtime toggle) is already close to what's needed |
| TUI improvements | Not started | Vix analysis complete but no code changes made |

## Gate status

- `cargo check --workspace`: PASS
- `cargo fmt --check`: PASS
- `cargo clippy --all-targets -- -D warnings`: PASS
- `cargo test -p kf-code --lib`: 2879 passed, 1 failed (`bundled_node_sdk_tool_executes_via_host` — requires Node.js, pre-existing)
- `cargo test --workspace`: ~2910 passed across all crates

## Config drift guard (Phase 4)

The config system has a triple-copy pattern: struct fields, `merge_toml_into_config`, and `apply_env_overrides`. The drift guard catches mismatches:

- `CONFIG_FIELD_COUNT = 73` (ModelConfig=22, SecurityConfig=18, ToolConfig=26, SessionConfig=4, DisplayConfig=3)
- `MERGE_TOML_EXPECTED = 62` (55 top-level leaf keys + 7 `[computer_use]` sub-keys)
- `ENV_OVERRIDE_EXPECTED = 58` (KF_CODE_* env vars)
- 16 struct fields intentionally skipped by `merge_toml_into_config` (documented in test)
- 4 additional fields skipped by `apply_env_overrides` (Vec/array types without env representations)

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

## Vix TUI analysis (key takeaways for future TUI work)

1. **Dual-buffer streaming**: vix uses raw text + rendered buffer, throttles markdown rendering to 100ms intervals. kf-code already does this.
2. **Inline tool output**: vix uses one-line summaries for tool results with ID-matched insertion. kf-code shows more detail.
3. **Permission flow**: vix uses a dedicated panel that replaces the input area. kf-code has approval dialogs but not a full panel.
4. **Context indicator**: vix shows `◔ 128k/200k · 64%` in status bar, color-coded. kf-code already has this.
5. **Multi-thread tabs**: vix has F1-F6 tabs for Threads/Chat/Models/MCP/Jobs/Settings. kf-code has no tab system.
6. **Model switching**: vix has a full Models tab with provider grid. kf-code uses `/model` command.
7. **Daemon protocol**: vix uses JSON-over-unix-socket with ThreadClient/InstanceClient. kf-code's daemon protocol is similar.

## Next steps (prioritized)

1. **TUI tabs**: Add F-key tab system (Threads, Chat, Models, Plugins, Jobs, Settings)
2. **TUI permission panel**: Replace the inline approval flow with a dedicated panel
3. **Plugin simplification**: Consider removing shell fallbacks and making all plugins always-compiled-in with runtime toggles
4. **Stratum/compress plugin**: Consider absorbing the stratum compression into the main binary permanently (remove the feature flag, make it always-on)

## Rust toolchain

Rust 1.88.0 is installed at `~/.cargo/bin/`. Run `export PATH="$HOME/.cargo/bin:$PATH"` before any cargo commands.

## Known issues

- `bundled_node_sdk_tool_executes_via_host` test fails because Node.js and the kf-plugin SDK aren't built — this is pre-existing, not related to the rename
- The `adr_0010_emission_site_block_uses_eprintln_for_errors` test in `kf-budget-core` fails — ADR vs impl drift, pre-existing