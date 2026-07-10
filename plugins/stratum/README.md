# Stratum plugin for KirkForge-Cli

This directory packages the [Stratum](https://github.com/kirkstratum/stratum) compression/rules pipeline as a KirkForge-Cli filesystem plugin.

## Installation

1. Build and install the `stratum` binary:

   ```bash
   cargo install --path crates/kirkstratum-cli
   ```

   or, once published:

   ```bash
   cargo install stratum
   ```

2. Copy this directory to the KirkForge plugin path:

   ```bash
   mkdir -p ~/.local/share/kirkforge/plugins
   cp -r /path/to/KirkForge-Plugin2/plugin ~/.local/share/kirkforge/plugins/stratum
   ```

3. Restart KirkForge-Cli or reload plugins. The manifest declares `trust = "shell"`, so ensure your KirkForge configuration allows shell-tier plugins.

## Requirements

- `stratum` must be on `PATH`.
- `jq` is optional but recommended for robust argument parsing.

## Provided tools

| Tool | Description |
|------|-------------|
| `stratum_run` | Run the pipeline on stdin. Pass `mode`, `token_budget`, `json`, `dry_run`, `max_input_size`. |
| `stratum_apply` | Apply the pipeline to a file or stdin. Pass `file`, `content_type`, `mode`, `token_budget`, `json`, `dry_run`. |
| `stratum_rules` | Emit the ruleset for the active or requested mode. Pass `mode`, `json`. |
| `stratum_mode` | Show or set the active mode. Pass `value` (off/lite/full/ultra), `json`. |
| `stratum_config_validate` | Validate the effective configuration. Pass `json`. |

## Provided skill

- `/stratum` — a slash-command assistant that explains the Stratum tools and helps pick the right one for the user's request.

## Provided hooks

| Hook | Event | Behaviour |
|------|-------|-----------|
| `hooks/session-start.sh` | `session-start` | Emits the active mode's ruleset so the model knows the compression contract. |
| `hooks/pre-tool-bash.sh` | `pre-tool-bash` | Validates the effective Stratum config before any bash tool runs. |

## File layout

```
~/.local/share/kirkforge/plugins/stratum/
├── kirkforge.toml
├── README.md
├── tools/
│   ├── run.sh
│   ├── apply.sh
│   ├── rules.sh
│   ├── mode.sh
│   └── config_validate.sh
└── hooks/
    ├── session-start.sh
    └── pre-tool-bash.sh
```
