# KirkForge

A provider-agnostic, verification-first coding agent in Rust.

It routes to any model provider (Ollama, OpenAI-compatible, Anthropic
direct/Bedrock/Vertex, OpenCode-Zen), edits files, runs commands, and
verifies its own work — build, test, lint, git, and security checks run
after every file-modifying tool call. A tree-sitter context index gives
it graph-grounded code understanding. Token-budget management and context
compression keep costs bounded on long sessions.

## Quick start

```bash
curl -fsSL https://raw.githubusercontent.com/KirkForge/Kirkforge_Cli/main/scripts/install.sh | sh
kirkforge run
```

Or build from source:

```bash
cargo install --git https://github.com/KirkForge/Kirkforge_Cli
```

Requires a running Ollama server (or set provider config for cloud models).

## Why KirkForge

- **Verification-first** — a correction loop catches build/test/lint errors
  and feeds them back to the model before you see them.
- **Provider-agnostic** — six providers behind one trait. No vendor lock-in.
- **Semantic code understanding** — tree-sitter symbol/import/call-graph index
  for Rust, TypeScript, Python, and Go.
- **Cost-aware** — Stratum compresses bloated inputs; Plugin3 guards token
  spend on outputs.
- **Plugin system** — Stratum, Plugin3, Draw, and Video are compiled-in
  behind feature flags (or shell fallbacks). External plugins via
  `kirkforge.toml` manifests with trust tiers and signature verification.
- **Benchmarked** — 31 coding tasks organized against the
  [KIRK-BENCH](KIRK-BENCH.md) spec, including the signature Token Budget
  Challenge that showcases the tree-sitter + Stratum + budget architecture.

## Documentation

- [docs/TECHNICAL.md](docs/TECHNICAL.md) — full technical manual
- [docs/adr/](docs/adr/) — architecture decision records
- [docs/workorders/](docs/workorders/) — planned and in-progress work
- [config.toml.example](config.toml.example) — fully documented config sample
- [AGENTS.md](AGENTS.md) — worker contract for AI agents in this repo
- [state.md](state.md) — current production-readiness state

## Development

```bash
cargo test --workspace
cargo clippy --all-targets -- -D warnings
./scripts/run-integration-tests.sh  # needs Ollama + qwen2.5:0.5b
cargo build --release               # ~5.4 MB binary
```

## Releases

Two-week minor cadence in the `v0.x` series. Binaries built automatically on
tag push for Linux (gnu/musl), macOS, and Windows. See the
[releases page](https://github.com/KirkForge/Kirkforge_Cli/releases).