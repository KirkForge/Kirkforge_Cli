> `kirkforge` — native Ollama coding agent CLI

A terminal coding assistant that runs locally against Ollama (or any OpenAI-compatible endpoint). It edits files, runs commands, keeps a conversation log, and stays inside a sandbox.

## Quick start

```bash
# Requires a running Ollama server
cargo run -- run

# Resume the most recent session via the daemon
cargo run -- run --auto-resume

# Resume a specific session by id or prefix
cargo run -- run --attach 2026-06-22-session-01

# Non-interactive, multi-turn
echo -e "fix the borrow check\n\n" | cargo run -- run --non-interactive --max-turns 5

# Start the session daemon manually (it is auto-started on demand)
cargo run -- daemon
```

## Main features

- **TUI chat** with conversation search, copy-to-clipboard, and model hot-swap (`/model`).
- **File tools** (`read_file`, `write_file`, `edit_file`) with approval gates, diff previews, and `/undo`.
- **Bash tool** and `!` passthrough, sandboxed to a configurable working directory.
- **Session management** — `/fork`, `/resume`, `/sessions`, plus `--continue-session`, `--auto-resume`, and `--attach`.
- **Session daemon** — background process tracks the last 5 sessions; the TUI shows a startup picker unless you resume explicitly.
- **Config hot-reload** — edit `config.toml` and type `/reload` (or send `SIGHUP`) to update access control live.
- **Permission rules** — Claude-Code-style allow/ask/deny rules per command/path; see the config example.
- **Multimodal** — `read_image` for screenshots and images.
- **MCP tools** — optional external tool servers via `[[mcp_servers]]` in config.
- **Enforced plan mode** — `/plan` locks the executor to read-only tools until you type `/implement`.
- **Subagent personas** — `/explore`, `/plan`, and `/coder` run isolated fork sessions with restricted toolsets and merge a summary back.

## Config

Config lives at `~/.local/share/kirkforge/config.toml`. See [`config.toml.example`](config.toml.example) for a fully documented sample, including permission rules and MCP servers.

```toml
default_model = "qwen2.5:3b"
ollama_host = "http://localhost:11434"
auto_approve = false
bang_requires_approval = true
sandbox_dir = ""  # empty = current directory
```

## Development

```bash
cargo test                          # unit tests
cargo clippy --all-targets -- -D warnings
./scripts/run-integration-tests.sh  # needs Ollama + qwen2.5:0.5b
cargo build --release               # ~5.4 MB binary
```

## Documentation

- [`review.md`](review.md) — current capabilities and known gaps.
- [`docs/adr/`](docs/adr/) — architecture decision records covering the daemon, session model, hot-reload, and more.
- [`docs/ideas/`](docs/ideas/) — roadmap and design notes.
