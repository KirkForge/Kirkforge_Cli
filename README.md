> `kirkforge` — native Ollama coding agent CLI

A terminal coding assistant that runs locally against Ollama (or any OpenAI-compatible endpoint). It edits files, runs commands, keeps a conversation log, and stays inside a sandbox.

## Quick start

### Install a release binary

```bash
curl -fsSL https://raw.githubusercontent.com/KirkForge/Kirkforge_Cli/main/scripts/install.sh | sh
```

Or build from source with [Rust](https://rustup.rs):

```bash
cargo install --git https://github.com/KirkForge/Kirkforge_Cli
```

### Run

```bash
# Requires a running Ollama server
kirkforge run

# Resume the most recent session via the daemon
kirkforge run --auto-resume

# Resume a specific session by id or prefix
kirkforge run --attach 2026-06-22-session-01

# Non-interactive, multi-turn
echo -e "fix the borrow check\n\n" | kirkforge run --non-interactive --max-turns 5

# Start the session daemon manually (it is auto-started on demand)
kirkforge daemon
```

## Main features

- **TUI chat** with conversation search, copy-to-clipboard, and model hot-swap (`/model`).
- **File tools** (`read_file`, `write_file`, `edit_file`) with approval gates, diff previews, and `/undo`.
- **Bash tool** and `!` passthrough, sandboxed to a configurable working directory.
- **Session management** — `/fork`, `/resume`, `/sessions`, `/save`, plus `--continue-session`, `--auto-resume`, and `--attach`.
- **Session daemon** — background process tracks the last 5 sessions; the TUI shows a startup picker unless you resume explicitly. Example systemd unit and launchd plist are in [`docs/`](docs/).
- **Config hot-reload** — edit `config.toml` and type `/reload` (or send `SIGHUP`) to update access control live.
- **Permission rules** — Claude-Code-style allow/ask/deny rules per command/path; `Deny` rules use prefix matching while `Allow`/`Ask` rules stay anchored.
- **Multimodal** — `read_image` for screenshots and images.
- **MCP tools** — optional external tool servers via `[[mcp_servers]]` in config.
- **Enforced plan mode** — `/plan` locks the executor to read-only tools until you type `/implement`.
- **Subagent personas** — `/explore`, `/plan`, and `/coder` run isolated fork sessions with restricted toolsets and merge a summary back.
- **Safe git commit helper** — `/commit` shows status, runs pre-commit sanitation (large files, secrets, conflict markers, unstaged debris) and suggests a conventional-commit message; `/commit "message"` stages all changes and commits after sanitation; `/commit --push "message"` also pushes.
- **Runtime plugins** — drop a plugin folder into `~/.local/share/kirkforge/plugins/<name>/` or toggle a built-in workspace source without restarting: `/plugins list`, `/plugins enable <name>`, `/plugins disable <name>`, `/plugins toggle <name>`, `/plugins reload`, `/plugins trust <name> <tier>`.

## Plugins

Plugins are filesystem folders containing a `kirkforge.toml` manifest plus any tool/hook/verifier scripts it declares. The host loads them at startup and caps each plugin to the `max_plugin_trust` tier in `config.toml` (read-only → shell → network → unsafe).

### Built-in workspace sources

This repo ships with five satellite plugins under `plugins/<name>/`. Each plugin’s source code also lives in this repo so everything builds together:

- `plugins/kirkforge-draw/` / `crates/kirkforge-draw*` — terminal diagram editor (`/draw`, `draw_render`, `draw_edit`).
- `plugins/kirkforge-video/` / `crates/kirkforge-video` — FFmpeg-native video pipeline (`/video`, `video_pipeline`, `video_render`, …).
- `plugins/stratum/` / `crates/kirkstratum*` — context compression pipeline (`/stratum`, `stratum_run`, …).
- `plugins/kirkforge-plugin3/` / `crates/plugin3*` — token-budget assistant (`/budget`, `plugin3_budget_*`, …).
- `plugins/kirkforge-plugin/` / `npm/kirkforge-plugin` — KirkForge-Plugin SDK verification CLI (`/kirkforge`, `plugin_verify`, …).

They are registered as workspace plugin sources by default but left **disabled** until you toggle them on. The plugin tool scripts prefer binaries built by this workspace (`target/release/<bin>` or `target/debug/<bin>`) and fall back to `PATH` for the Node SDK or any externally installed build.

### Runtime commands

Use the TUI slash commands to manage plugins without restarting:

- `/plugins list` — show active, blocked, available, and workspace plugin sources.
- `/plugins enable <name>` — load an available plugin directory from `~/.local/share/kirkforge/plugins/`.
- `/plugins disable <name>` — unload a plugin and remove its tools/skills.
- `/plugins toggle <name>` — enable or disable a built-in workspace source persistently.
- `/plugins reload` — full rescan of the plugins directory and workspace sources.
- `/plugins trust <name> <tier>` — session-only re-enable with a specific trust tier.
- `/plugins sources` — list configured workspace plugin sources.
- `/plugins add <name> <path>` / `/plugins remove <name>` — register or unregister a workspace source.
- `/plugins setup` — quick-start help for workspace sources.

## Config

Config lives at `~/.local/share/kirkforge/config.toml`. See [`config.toml.example`](config.toml.example) for a fully documented sample, including permission rules and MCP servers.

```toml
default_model = "qwen2.5:3b"
ollama_host = "http://localhost:11434"
auto_approve = false
bang_requires_approval = true
sandbox_dir = "."  # "." = current directory; "" = unsandboxed (escape hatch)
```

## Development

```bash
cargo test                          # unit tests
cargo clippy --all-targets -- -D warnings
./scripts/run-integration-tests.sh  # needs Ollama + qwen2.5:0.5b
cargo build --release               # ~5.4 MB binary
```

The Rust satellites build automatically with the workspace (`cargo build --workspace --release`). The Node SDK under `npm/kirkforge-plugin/` must be built separately:

```bash
cd npm/kirkforge-plugin
npm install
npm run build
```

This produces `apps/cli/dist/index.js`, which the `plugins/kirkforge-plugin/` tool scripts invoke.

## Releases

Release binaries for Linux (x86_64) and macOS (x86_64, Apple Silicon) are built automatically when a `v*.*.*` tag is pushed. See the [releases page](https://github.com/KirkForge/Kirkforge_Cli/releases) or use the install script above.

## Daemon supervision

The session daemon is started on demand, but you can also run it under your init system:

- **systemd (Linux):** copy [`docs/kirkforge-daemon.service`](docs/kirkforge-daemon.service) to `~/.config/systemd/user/`, adjust the `ExecStart` path if needed, then:
  ```bash
  systemctl --user daemon-reload
  systemctl --user enable --now kirkforge-daemon
  ```
- **launchd (macOS):** copy [`docs/com.kirkforge.daemon.plist`](docs/com.kirkforge.daemon.plist) to `~/Library/LaunchAgents/`, replace `USER` with your username, then:
  ```bash
  launchctl load ~/Library/LaunchAgents/com.kirkforge.daemon.plist
  launchctl start com.kirkforge.daemon
  ```

## Documentation

- [`review.md`](review.md) — current capabilities and known gaps.
- [`docs/adr/`](docs/adr/) — architecture decision records covering the daemon, session model, hot-reload, and more.
- [`docs/ideas/`](docs/ideas/) — roadmap and design notes.
