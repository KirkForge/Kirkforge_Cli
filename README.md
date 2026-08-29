# KirkForge

![Rust](https://img.shields.io/badge/rust-1.88%2B-orange)
![CI](https://img.shields.io/github/actions/workflow/status/KirkForge/Kirkforge_Cli/.github/workflows/ci-merge.yml?branch=dev)
![License](https://img.shields.io/badge/license-MIT-blue)
![Version](https://img.shields.io/badge/version-0.3.10-green)

AI coding assistant that runs in your terminal. Open source, local-first, works with any LLM.

## Quick install

**Linux / macOS:**

```bash
curl -fsSL https://raw.githubusercontent.com/KirkForge/Kirkforge_Cli/main/scripts/install.sh | bash
```

**Windows (PowerShell):**

```powershell
irm https://raw.githubusercontent.com/KirkForge/Kirkforge_Cli/main/scripts/install.ps1 | iex
```

## Quick start

```
kf-code run       # start the TUI
/model claude     # pick a model
Type a message    # start coding
```

## What it does

- Reads, writes, and edits your code
- Runs tests, commits, and manages git
- Works with Claude, GPT, Ollama, and any OpenAI-compatible model

Benchmarked on 30 coding tasks — see [docs/TECHNICAL.md](docs/TECHNICAL.md#benchmarks-kirk-bench).

## Links

- [docs/TECHNICAL.md](docs/TECHNICAL.md) — architecture and internals
- [docs/adr/](docs/adr/) — architecture decision records
- [CHANGELOG.md](CHANGELOG.md) — what changed
- [Releases](https://github.com/KirkForge/Kirkforge_Cli/releases)