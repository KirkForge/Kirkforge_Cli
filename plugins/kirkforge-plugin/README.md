# KirkForge-Plugin filesystem plugin

This directory packages the KirkForge-Plugin SDK as a KirkForge-Cli filesystem plugin, exposing the TypeScript CLI commands as KirkForge tools. The Node SDK source lives in `npm/kirkforge-plugin` in this workspace.

## Installation

Copy this directory to the KirkForge plugins folder:

```bash
mkdir -p ~/.local/share/kirkforge/plugins/
cp -r plugins/kirkforge-plugin ~/.local/share/kirkforge/plugins/kirkforge-plugin/
```

The tool scripts locate the CLI entry point in the following order:

1. `npm/kirkforge-plugin/apps/cli/dist/index.js` — when running inside this workspace.
2. `~/.local/share/kirkforge/plugins/kirkforge-plugin/apps/cli/dist/index.js` — when the whole plugin directory is copied.
3. `kirkforge` on `PATH` — if the CLI binary/executable is installed globally.

After restarting KirkForge-Cli, the plugin's tools and skill become available.

## Prerequisites

- **Node.js >= 20.0.0** (required to run the bundled CLI).
- The CLI is invoked from `apps/cli/dist/index.js`. Rebuild with `npm run build` from `npm/kirkforge-plugin/`.
- **Optional:** `tsx` is only needed if you want to run the source directly (e.g. `npm run cli -- verify`). The plugin scripts use the prebuilt `dist` output, so `tsx` is not required at runtime.

## Available tools

| Tool | CLI command | Purpose |
|------|-------------|---------|
| `plugin_verify` | `verify` | Run deterministic verification emitters without calling a model. Reports lint, type, security, graph, and overall status. |
| `plugin_verify_workspace` | `verify-workspace` | Run deterministic verification on a workspace directory and emit a `ReducedStatePacket`. |
| `plugin_audit_verify` | `audit-verify` | Verify the integrity of a KirkForge audit JSONL chain. |
| `plugin_doctor` | `doctor` | Probe local verification tools (ESLint, tsc, Ruff, Pyright, Bandit, SecDev) and report capabilities. |
| `plugin_health` | `health` | Show orchestrator health and SLO status. |
| `plugin_tools` | `tools` | List registered verification tools and lint engines. |

Each tool script reads its arguments from the `KIRKFORGE_TOOL_ARGS_JSON` environment variable, translates them into command-line flags, and runs the CLI from the plugin root.

## Available skill

- `/kirkforge` — KirkForge-Plugin assistant that selects the right verification or diagnostic tool based on the user's request.

## Trust tier

This plugin requests `trust = "shell"` because it invokes Node.js and shell commands to run the bundled CLI.
