# ADR 001: Native Ollama CLI in Rust

## Status

Accepted

## Date

2026-06-01

## Context

We run GLM-5.1:Cloud, DeepSeek-v4-Pro, and Gemini 3.0 Flash 1M through Ollama on thin clients (8GB C-50 laptop, 16GB 2012 laptop, ARM Huawei P30). Existing coding agents either require AVX (Codex, OpenCode) or only speak Anthropic's API (Vix).

We tested Vix through a Node.js proxy to Ollama. It connected and streamed responses, but the proxy kept crashing, GLM's thinking tokens leaked through, tool calls didn't translate properly, and the whole thing was a fragile shim between two incompatible APIs. The experience proved that protocol translation is the wrong layer — we need a native Ollama client, not a translator.

## Decision

Build a TUI coding agent in Rust that talks directly to Ollama's `/api/chat` and `/v1/chat/completions` endpoints. No Anthropic layer, no translation, no Node.js.

One binary. Ollama native. Runs on potato hardware.

## Core requirements

- **Ollama native** — speaks `/api/chat` (NDJSON streaming) and `/v1/chat/completions` (OpenAI-compatible SSE) directly
- **Model-aware** — GLM-5.1 has a `thinking` field, DeepSeek has its own tool call format, Gemini has different streaming behavior. Handle each correctly, no regex hacks
- **Static binary** — cross-compile for x86_64, aarch64, armv7. No runtime deps. `scp` to any of the three machines and run
- **Low resource** — megabytes of RAM, sub-second startup. Must run on the 8GB C-50
- **TUI** — terminal user interface, like Codex. Chat, file reads, file edits, bash execution
- **Streaming first** — SSE/NDJSON streaming as the default path. Show tokens as they arrive
- **Tool use** — read files, write files, edit files, run bash, grep, glob. Same capability set as Vix/Codex

## Alternatives considered

| Alternative | Why rejected |
|---|---|
| Vix + Node.js proxy | Fragile, crashes, thinking tokens leak, tool calls don't translate, 200MB+ Node footprint |
| Patching Vix | Closed-source binary, can't modify |
| Codex | Requires AVX, won't run on C-50 or P30 |
| OpenCode | Requires AVX |
| Continue/vscode extension | GUI, not CLI; wrong form factor for these machines |
| Writing in Go | Vix is Go. We want something different — Rust gives us memory safety without GC, smaller binaries, and no runtime overhead on constrained hardware |

## Milestones

1. **MVP** — connect to Ollama, stream responses in terminal, handle GLM's thinking field
2. **Tools** — read_file, write_file, edit_file, bash, grep, glob
3. **VFS** — syntax-aware minification (the Vix insight: send less, pay less)
4. **Stem agent** — single agent, single conversation, cache reuse across phases
5. **Cross-compile** — CI producing binaries for x86_64, aarch64, armv7
