# KirkForge-Plugin3 plugin for KirkForge-Cli

This directory is a [KirkForge filesystem plugin](https://github.com/KirkForge/KirkForge-Cli/blob/main/docs/adr/013-rust-native-plugin-system.md) that exposes the `plugin3` output-side token-budget tool as tools, a skill, and lifecycle hooks inside the `kirkforge` TUI/CLI.

## Install

1. Build the `plugin3` binary from the repo root:
   ```bash
   cargo build --release --bin plugin3
   ```
   Make sure `plugin3` is on your `PATH`, e.g. by copying `target/release/plugin3` to `~/.cargo/bin/` or symlinking it.

2. Copy this directory into the KirkForge plugins folder:
   ```bash
   mkdir -p ~/.local/share/kirkforge/plugins
   cp -R /path/to/KirkForge-Plugin3/plugin ~/.local/share/kirkforge/plugins/kirkforge-plugin3
   ```

3. Set `max_plugin_trust = "shell"` (or higher) in `~/.local/share/kirkforge/config.toml`, because the plugin shells out to `plugin3`.

4. Restart `kirkforge run`. The TUI status bar should show the plugin3 tools loaded.

## Tools exposed

| Tool name | What it calls | Typical args |
|-----------|---------------|--------------|
| `plugin3_budget_status` | `plugin3 budget status` | `{}` |
| `plugin3_budget_set` | `plugin3 budget set <ceiling>` | `{"ceiling": 100000}` |
| `plugin3_budget_compact` | `plugin3 budget compact` | `{}` |
| `plugin3_budget_report` | `plugin3 report` | `{}` |
| `plugin3_store_get` | `plugin3 store get <marker>` | `{"marker": "my-marker"}` |
| `plugin3_config_validate` | `plugin3 config --validate` | `{}` |
| `plugin3_self_check` | `plugin3 self-check` | `{}` |

All tool arguments are passed via the `KIRKFORGE_TOOL_ARGS` environment variable as JSON. Tools write their results to stdout.

## Skill

| Trigger | Purpose |
|---------|---------|
| `/budget` | Guides the assistant through checking status, setting ceilings, compacting, and reporting. |

## Hooks mapped

| KirkForge event | plugin3 hook | Script |
|-----------------|--------------|--------|
| `post-tool-bash` | `hook post-tool-use` | `hooks/post-tool-bash.sh` |
| `post-tool-write_file` | `hook post-tool-use` | `hooks/post-tool-write_file.sh` |
| `session-start` | `hook user-prompt-submit` | `hooks/session-start.sh` |
| `pre-compact` | `hook pre-compact` | `hooks/pre-compact.sh` |

Each hook constructs a JSON payload from `KF_EVENT`, `KF_TOOL_NAME`, `KF_TOOL_ARGS_JSON`, and `KF_SESSION_ID`, then pipes it to the relevant `plugin3 hook ...` subcommand.

## Binary discovery

The shell scripts look for `plugin3` in this order:

1. Next to the script itself (for local development).
2. `../../target/release/plugin3` and `../../target/debug/plugin3` relative to the script.
3. Any `plugin3` on `PATH`.

If you installed the binary somewhere else, add it to `PATH` or symlink it next to the scripts.

## Trust tier

The manifest declares `trust = "shell"`. The plugin does not execute arbitrary user commands, but it does spawn the `plugin3` binary. Do not raise `max_plugin_trust` above what you need.

## License

MIT — same as the rest of KirkForge-Plugin3.
