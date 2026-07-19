# ADR 013: Rust-Native Plugin System

## Status

Accepted

## Context

KirkForge started with a simple `SKILL.md` loader (`src/session/skills.rs`).
Skills are reusable slash-command prompts backed by YAML frontmatter.
They are easy to write but limited: they cannot declare tools, lifecycle
hooks, verifiers, or trust boundaries.

Several agent CLIs (Kimi Code, Codex) treat capabilities as a unified
registry: built-in tools, MCP tools, IDE-registered tools, and user
plugins all live in one toolset. We wanted the same extensibility in
KirkForge without rebuilding the runtime from scratch every time a user
adds a new capability.

## Decision

Introduce a Rust-native plugin system, **vendored in-repo** since Phase-6
under `crates/plugin3-*` (`plugin3-core`, `plugin3-hosts`, `plugin3-cli`),
not a separate repository consumed as path dependencies. The npm-side
orchestrator and tool packages live under `npm/kirkforge-plugin/` in the
same repo. A prior framing — "a separate repository `KirkForge-Plugin`
consumed by `KirkForge-Cli` as path dependencies" — is superseded; both
halves now ship from this single repository.

A plugin is a directory containing:

- `kirkforge.toml` — manifest with name, version, description, trust tier,
  and capability declarations.
- Optional `SKILL.md` — skill prompt for slash-command capabilities.
- Optional hook/verifier/tool scripts — invoked by the host runtime.

Trust tiers:

- `read-only` — skills/verifiers only.
- `shell` — may invoke shell commands (tools/hooks).
- `network` — may fetch URLs or talk to network services.
- `unsafe` — blocked by default; reserved for future native/WASM plugins.

The host (`kirkforge-plugin-host`) provides:

- `PluginRegistry` — load, index, and filter plugins by trust policy.
- `SandboxPolicy` — map each capability kind to the minimum required tier.
- `PluginTool`, `PluginHook`, `PluginVerifier` — wrap shell-script
  capabilities with the same invocation conventions as built-ins.
- `Toolset` / `CompositeToolset` — uniform view of plugin tools for later
  unification with built-in and MCP tools.
- Compatibility loader — existing `.claude/skills/<name>/SKILL.md` directories
  are treated as read-only plugins with a single skill capability.

A standalone CLI, `kirkforge-plugin`, ships in-repo at `crates/plugin3-cli`:

- `kirkforge-plugin init <name>` — scaffold a new plugin directory.
- `kirkforge-plugin check <dir>` — validate a plugin manifest.
- `kirkforge-plugin list [--dir <dir>]` — list installed plugins and warnings.

## Consequences

- Plugin authors get a clear manifest format and a validation workflow
  without installing the full `KirkForge-Cli`.
- `KirkForge-Cli` keeps the skill compatibility loader, so existing
  `.claude/skills/` directories continue to work.
- The trust model is centralized in the plugin3 crates; the CLI and the
  plugin system share the same `TrustTier` ordering and policy semantics.
- Because the plugin crates are vendored in-repo, no side-by-side checkout
  is required and a single `cargo build` / `npm install` builds everything.
  A future split into published crates or git submodules is still possible
  but is no longer the operating assumption.

## Workspace Plugin Sources

In addition to plugins installed under `data_dir/plugins`, operators can
register **workspace plugin sources**: plugin directories that live outside
the data directory (for example, in-repo directories under `plugins/<name>/`
such as `plugins/demo` or `plugins/draw`).

Configuration is stored in `config.toml`:

```toml
[plugin_sources]
demo = "plugins/demo"
draw = "plugins/draw"

enabled_plugins = ["demo"]
```

- `plugin_sources` — a name → directory path mapping.
- `enabled_plugins` — names from `plugin_sources` that should be loaded at
  startup.

Both fields can also be set via environment variables:

- `KIRKFORGE_PLUGIN_SOURCES=name1=/path/one,name2=/path/two`
- `KIRKFORGE_ENABLED_PLUGINS=name1,name2`

The TUI provides slash commands for runtime management:

- `/plugins add <name> <path>` — register a workspace source.
- `/plugins remove <name>` — unregister a source and unload it.
- `/plugins toggle <name>` — enable or disable a source; persists to config.
- `/plugins sources` — list configured sources and their enabled/active state.
- `/plugins setup` — show a quick-start summary.
- `/plugins reload` — rescan all sources and apply the current config.

## Implementation Notes

- `KirkForge-Plugin` workspace members:
  - `crates/kirkforge-plugin` — SDK.
  - `crates/kirkforge-plugin-host` — runtime.
  - `apps/plugin-cli` — standalone binary.
- `KirkForge-Cli/Cargo.toml` depends on both crates via
  `path = "../KirkForge-Plugin/crates/..."`.
- `src/session/skills.rs` wraps `PluginRegistry` and surfaces plugin trust
  tiers in the TUI status bar.
- `src/session/plugin_tools.rs` loads workspace sources through
  `load_workspace_plugins` and merges them into the executor-facing registry.
- `src/tui/commands/plugins.rs` implements `/plugins toggle`, `add`, `remove`,
  `sources`, and `setup`, persisting mutations through
  `session::config::save_config`.
