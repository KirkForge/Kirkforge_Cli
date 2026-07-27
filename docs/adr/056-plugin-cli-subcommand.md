# ADR-056: Shared plugin-ops layer and `kirkforge plugin` CLI subcommand

- **Status:** Accepted
- **Date:** 2026-07-27

## Context

Plugin management was TUI-only. The `/plugins` slash-command family
(`src/tui/commands/plugins/mod.rs`) provided `list`, `enable`, `disable`,
`toggle`, `reload`, `trust`, `setup`, `sources`, `add`, `remove` — but
only inside the interactive TUI. A user running `kirkforge run` non-
interactively (NDJSON mode, CI, cron, a wrapper script) could not enable
a plugin, list active plugins, validate a manifest, or check plugin
health without launching the TUI.

The root cause was that the plugin management operations were built as
TUI slash-commands first, with no shared layer. The `/plugins` commands
called into `PluginRegistry` and `SharedConfig` directly; there was no
"plugin ops" module that both the TUI and a CLI subcommand could call.

## Decision

1. **Extract a shared plugin-ops layer** in
   `src/session/plugin_ops.rs`. The functions take a `&Config` (or
   `&mut Config`) and return a human-readable `String` or
   `Result<String>`. Neither `AppState` nor a live `PluginRegistry`
   channel is touched here — the TUI keeps its `mpsc` reload plumbing;
   the CLI mutates the config and prints "restart to apply" when there
   is no live registry.

2. **Add a `kirkforge plugin` CLI subcommand** (`src/cli.rs`) with
   variants mirroring the TUI `PluginsOp` enum: `list`, `enable`,
   `disable`, `toggle`, `validate`, `reload`, `sources`, `add`,
   `remove`, `doctor`. The dispatch in `src/main/mod.rs` loads the
   shared config once, runs the op via the shared layer, prints the
   result, and persists any config mutation.

3. **Honest CLI semantics.** CLI plugin ops are config mutations, not
   live-registry reloads. The returned message says so: "Run
   `kirkforge run` (or `/plugins reload` in the TUI) to load it." This
   is least-surprise: a scripted invocation does not silently mutate a
   running daemon's registry.

## Consequences

- Headless / scripted / CI users now have a plugin control surface.
- The TUI and CLI share one implementation of the formatting + config
  mutation logic. Future plugin ops added to one are available to the
  other by calling the shared function.
- The TUI `handle_plugins_op` wrapper is NOT rewritten in this WO — it
  keeps the `AppState` + `mpsc` plumbing. The shared layer is the
  formatting + mutation seam; a follow-up can migrate the TUI handlers
  to call it. (Migrating the TUI in the same WO would risk a
  regression in the live-reload path; the shared layer is additive.)
- `bincode` remains rejected; the config persistence path is unchanged
  (`save_config` writes TOML).

## Why not migrate the TUI in the same commit

The WO scope is "extract a shared layer + add the CLI." Migrating the
TUI handlers to call the shared functions is a refactor that touches
the live-reload path (`plugin_reload_tx`), where a regression would
break the interactive session. The shared layer is additive and tested
in isolation; the TUI migration is a follow-up that can be gated on its
own tests.

## Notes

- `ponytail:` / `ceiling:` annotations in
  `src/tui/commands/plugins/mod.rs` are preserved (untouched).
- The `kirkforge plugin validate <path>` accepts either a plugin
  directory or a `kirkforge.toml` file path, mirroring the manifest
  loader.
- The `kirkforge plugin doctor` health check probes each enabled
  plugin's tool/hook command files for existence (the same check the
  loader applies, but reported as a summary rather than warnings).