# KirkForge

A provider-agnostic, verification-first coding agent in Rust.

KirkForge routes model requests to any supported provider (Ollama, OpenAI-compatible,
Anthropic direct/Bedrock/Vertex, OpenCode-Zen), edits files, runs commands, and
verifies its own work with a build/test/lint/git/security correction loop. A
tree-sitter context index gives it graph-grounded code understanding. Token-budget
management and context compression keep costs bounded on long sessions.

Specialized runtimes for diagram rendering and instruction-driven video editing
ship as satellite binaries, orchestrated through the plugin system.

For the full architecture, see [ARCHITECTURE.md](ARCHITECTURE.md).

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
# Requires a running Ollama server (or set provider config for cloud models)
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

## What makes KirkForge different

**Verification-first.** After every file-modifying tool call, a verifier bus runs
build, test, clippy, rustfmt, git-state, and security checks. A correction loop
auto-applies formatter fixes (up to 3 iterations) and feeds unfixable errors back
to the model as tool results. The agent sees its own mistakes and corrects them
before you do.

**Provider-agnostic.** One `ModelAdapter` trait, six concrete providers. Route by
model name (`claude-*` → Anthropic, `glm*`/`deepseek*`/`gemini*`/`kimi*` → Ollama,
`opencode/` → OpenCode-Zen, else → OpenAI-compat) or override in config. No vendor
lock-in.

**Semantic code understanding.** A tree-sitter index builds symbol, import, and
call-graph edges for Rust, TypeScript, Python, and Go. The agent retrieves
graph-grounded context — who imports a symbol, who calls it — not just text
matches.

**Cost-aware.** Two complementary systems bound context cost: Stratum compresses
bloated tool outputs on the input side; Plugin3 tracks token spend against a
ceiling and slices or compacts oversized results on the output side.

**Deterministic execution.** Enforced plan mode (`/plan` then `/implement`),
per-result checkpointing mid-batch, execution replay, and conversation logging.

## Features

- **TUI chat** with conversation search, copy-to-clipboard, and model hot-swap (`/model`).
- **File tools** (`read_file`, `write_file`, `edit_file`) with approval gates, diff previews, and `/undo`.
- **Bash tool** and `!` passthrough, sandboxed to a configurable working directory.
- **Session management** — `/fork`, `/resume`, `/sessions`, `/save`, plus `--continue-session`, `--auto-resume`, and `--attach`.
- **Session daemon** — background process tracks the last 5 sessions; the TUI shows a startup picker unless you resume explicitly. Example systemd unit and launchd plist are in [`docs/`](docs/).
- **Config hot-reload** — edit `config.toml` and type `/reload` (or send `SIGHUP`) to update access control live.
- **Permission rules** — allow/ask/deny rules per command/path; `Deny` rules use prefix matching while `Allow`/`Ask` rules stay anchored.
- **Multimodal** — `read_image` for screenshots and images.
- **MCP tools** — optional external tool servers via `[[mcp_servers]]` in config.
- **Enforced plan mode** — `/plan` locks the executor to read-only tools until you type `/implement`.
- **Subagent personas** — `/explore`, `/plan`, and `/coder` run isolated fork sessions with restricted toolsets and merge a summary back.
- **Programmable workflows** — JSON-defined DAGs of persona-driven steps with built-in `bugfix`, `feature`, and `refactor` templates.
- **Safe git commit helper** — `/commit` shows status, runs pre-commit sanitation (large files, secrets, conflict markers, unstaged debris) and suggests a conventional-commit message; `/commit "message"` stages all changes and commits after sanitation; `/commit --push "message"` also pushes.
- **Runtime plugins** — drop a plugin folder into `~/.local/share/kirkforge/plugins/<name>/` or toggle a built-in workspace source without restarting.
- **Benchmark harness** — 10 coding tasks (easy/medium/hard) with deterministic verification, runnable via `kirkforge bench`.

## Plugins

Plugins are filesystem folders containing a `kirkforge.toml` manifest plus any
tool/hook/skill/verifier scripts it declares. The host loads them at startup and
caps each plugin to the `max_plugin_trust` tier in `config.toml` (read-only then
shell then network then unsafe). Optional minisign signature verification is
supported.

### Built-in plugins

Five plugins ship in this repo. Each plugin's logic lives in a Rust crate under
`crates/`; the `plugins/` directory contains thin shell wrappers that invoke the
compiled binary.

| Plugin | Skill | What it does |
|---|---|---|
| **Stratum** | `/stratum` | Context compression — classifies and compacts bloated tool outputs before they enter the context window. 5 tools, 2 hooks. |
| **Plugin3** | `/budget` | Token budget guard — tracks spend against a ceiling, slices or compacts oversized results. 7 tools, 4 hooks. |
| **Draw** | `/draw` | Terminal diagram editor — the model produces `.td.json`, `kfd` renders it to fenced markdown. 1 tool, 1 hook. |
| **Video** | `/video` | Instruction-driven video production — the text LLM directs, FFmpeg renders. 8 tools. |
| **Plugin SDK** | `/kirkforge` | Verification tooling backed by the Node SDK. 6 tools. |

They are registered as workspace plugin sources and **enabled by default** (when
their directories exist). Use `/plugins toggle <name>` to disable a bundled
plugin persistently.

### Runtime commands

- `/plugins list` — show active, blocked, available, and workspace plugin sources.
- `/plugins enable <name>` — load an available plugin directory.
- `/plugins disable <name>` — unload a plugin and remove its tools/skills.
- `/plugins toggle <name>` — enable or disable a built-in workspace source persistently.
- `/plugins reload` — full rescan of the plugins directory and workspace sources.
- `/plugins trust <name> <tier>` — session-only re-enable with a specific trust tier.
- `/plugins sources` — list configured workspace plugin sources.
- `/plugins add <name> <path>` / `/plugins remove <name>` — register or unregister a workspace source.
- `/plugins setup` — quick-start help for workspace sources.

## Config

Config lives at `~/.local/share/kirkforge/config.toml`. See
[`config.toml.example`](config.toml.example) for a fully documented sample,
including permission rules, MCP servers, plugin settings, and provider routing.

```toml
# Set these to the Ollama gateway that routes your chosen frontier model.
# Leaving them empty requires every invocation to pick a model with -m/--model.
default_model = ""
ollama_host = ""
auto_approve = false
bang_requires_approval = true
sandbox_dir = "."  # "." = current directory; "" = unsandboxed (escape hatch)
# routing_model_map = { complex = "kimi-2.7k-coder:cloud", medium = "glm-5.2:cloud", simple = "qwen3:32b:cloud" }
```

## Development

```bash
cargo test                          # unit tests
cargo clippy --all-targets -- -D warnings
./scripts/run-integration-tests.sh  # needs Ollama + qwen2.5:0.5b
cargo build --release               # ~5.4 MB binary
```

The Rust satellites build automatically with the workspace
(`cargo build --workspace --release`). The Node SDK under
`npm/kirkforge-plugin/` must be built separately:

```bash
cd npm/kirkforge-plugin
npm install
npm run build
```

This produces `apps/cli/dist/index.js`, which the `plugins/kirkforge-plugin/`
tool scripts invoke.

## Releases

KirkForge-Cli follows a two-week minor-release cadence while in the `v0.x`
series with patch releases as needed. Release binaries are built automatically
when a `v*.*.*` tag is pushed:

- Linux: `x86_64-unknown-linux-gnu`, `x86_64-unknown-linux-musl`, `aarch64-unknown-linux-musl`
- macOS: `x86_64-apple-darwin`, `aarch64-apple-darwin`
- Windows: `x86_64-pc-windows-msvc`

See the [releases page](https://github.com/KirkForge/Kirkforge_Cli/releases) or
use the install script above. See [`docs/RELEASE.md`](docs/RELEASE.md) for the
maintainer runbook.

## Platform notes

The core `kirkforge run` workflow works on Linux, macOS, and Windows. A few
Unix-only features have platform-specific behavior:

- **Session daemon** — supported on Unix. On Windows the daemon is unsupported;
  session discovery falls back to the file index.
- **Scheduled-job daemon (`kirkforge jobd`)** — Unix only.
- **Config hot-reload** — use `/reload` everywhere. On Unix you can also send
  `SIGHUP`; Windows has no `SIGHUP` equivalent.
- **Bash tool** — on Windows targets `bash` (Git for Windows / WSL). `cmd.exe`
  is not used.
- **Subprocess cleanup** — on Unix the full process group is killed. On Windows
  only the immediate child is killed.

## Daemon supervision

The session daemon is started on demand, but you can also run it under your init
system:

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

- [ARCHITECTURE.md](ARCHITECTURE.md) — full architecture: how the pieces fit together
- [`docs/adr/`](docs/adr/) — 62 architecture decision records pinning load-bearing decisions
- [`docs/workorders/`](docs/workorders/) — planned and in-progress work
- [`docs/ideas/`](docs/ideas/) — roadmap and design notes
- [`docs/runbooks/`](docs/runbooks/) — operational runbooks
- [AGENTS.md](AGENTS.md) — worker contract for AI agents in this repo
- [`state.md`](state.md) — current production-readiness state