# KirkForgeSeries CLI

Native Ollama CLI coding agent in Rust. Static binary, TUI, potato hardware (8GB C-50, 16GB 2012 laptop, ARM Huawei P30). Speaks `/api/chat` and `/v1/chat/completions` directly — no proxy, no Node.js, no Anthropic layer.

## Architecture (see docs/adr/)

| ADR | Decision |
|-----|----------|
| 001 | Native Ollama CLI in Rust (accepted) |
| 002 | ratatui + crossterm TUI, immediate-mode rendering |
| 003 | Single StreamEvent enum, per-model adapters (GLM/DeepSeek/Gemini) |
| 004 | Client-side tool dispatch with tiered approval gates |
| 005 | NDJSON session logs, token-budgeted prompt construction |

## Key constraints

- **Static binary** — musl target, cross-compile for x86_64/aarch64/armv7
- **No expensive runtime** — no libc deps, no kernel-module mocking
- **Thread safety** — TUI renderer on main thread, Ollama I/O on worker thread, tool execution on dedicated tasks. State protected by `Arc<RwLock<>>`.
- **No AVX** — C-50 and P30 don't have it. All SIMD gated behind runtime feature-detection
- **Model-type awareness** — GLM has `thinking` field, DeepSeek batches tool calls, Gemini streams differently. Handled by adapters, never raw JSON at the session level.

## Project status

Early stage. Repository initialized with ADRs 001-005. No code yet.

## Relevant paths

- `docs/adr/` — architecture decision records
- `~/.local/share/ollama-cli/` — runtime data directory
- `~/.ollama/` — Ollama server config (model storage, server settings)