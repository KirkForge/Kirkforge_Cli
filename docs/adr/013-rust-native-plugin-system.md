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

Introduce a Rust-native plugin system in a separate repository,
`KirkForge-Plugin`, consumed by `KirkForge-Cli` as path dependencies.

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

A standalone CLI, `kirkforge-plugin`, ships in `KirkForge-Plugin/apps/plugin-cli`:

- `kirkforge-plugin init <name>` — scaffold a new plugin directory.
- `kirkforge-plugin check <dir>` — validate a plugin manifest.
- `kirkforge-plugin list [--dir <dir>]` — list installed plugins and warnings.

## Consequences

- Plugin authors get a clear manifest format and a validation workflow
  without installing the full `KirkForge-Cli`.
- `KirkForge-Cli` keeps the skill compatibility loader, so existing
  `.claude/skills/` directories continue to work.
- The trust model is centralized in `KirkForge-Plugin`; both repos share
  the same `TrustTier` ordering and policy semantics.
- Path dependencies require both repos to be checked out side-by-side.
  For release we can switch to published crates or git submodules.

## Implementation Notes

- `KirkForge-Plugin` workspace members:
  - `crates/kirkforge-plugin` — SDK.
  - `crates/kirkforge-plugin-host` — runtime.
  - `apps/plugin-cli` — standalone binary.
- `KirkForge-Cli/Cargo.toml` depends on both crates via
  `path = "../KirkForge-Plugin/crates/..."`.
- `src/session/skills.rs` wraps `PluginRegistry` and surfaces plugin trust
  tiers in the TUI status bar.
