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

All 10 milestones complete. 33 source files, ~5,100 lines of Rust. 69 unit tests, 7 integration tests (require Ollama, marked `#[ignore]`).

## Build

- **0 errors, 0 warnings** (clippy `-D warnings`)
- `cargo test` — 69 unit tests, all pass
- `cargo test --test integration_test -- --ignored` — 7 integration tests against live Ollama
- Release: 4.6 MB (gnu), 4.8 MB (musl static), LTO + panic=abort + strip

## Milestones

| # | Milestone | Status |
|---|-----------|--------|
| 1 | Ollama connection + streaming | ✅ |
| 2 | Tools (read/write/edit/bash/grep/glob) | ✅ |
| 3 | TUI (ratatui + crossterm) | ✅ |
| 4 | Model adapters (GLM/DeepSeek/Gemini/OpenAI-compat) | ✅ |
| 5 | Approval gates + tool dispatch | ✅ Always-approve persists to config |
| 6 | Session persistence (NDJSON logs) | ✅ |
| 7 | Cross-compile CI (x86_64/aarch64/armv7 musl) | ✅ |
| 8 | VFS prompt compression (minifier) | ✅ Wired into read_file + prompt builder |
| 9 | Event bus + verifier slots | ✅ 9 event kinds, 4 verifier slots, correction loop |
| 10 | Deny list + path safety | ✅ 6-layer path guard, read-before-edit, binary detection |

## Relevant paths

- `docs/adr/` — architecture decision records (7 documents)
- `tests/integration_test.rs` — 7 live Ollama integration tests
- `~/.local/share/kirkforge/` — runtime data directory
- `~/.ollama/` — Ollama server config (model storage, server settings)
- `src/session/access.rs` — DenyList, PathGuard, ReadGate (Phase 2)
- `src/session/event_bus.rs` — EventBus, EventHandler trait, 9 event kinds (Phase 3)
- `src/session/verifier/` — VerifierSlots, CorrectionLoop, lint/security/git verifiers (Phase 4)

## graphify

This project has a knowledge graph at graphify-out/ with god nodes, community structure, and cross-file relationships.

Rules:
- For codebase questions, first run `graphify query "<question>"` when graphify-out/graph.json exists. Use `graphify path "<A>" "<B>"` for relationships and `graphify explain "<concept>"` for focused concepts. These return a scoped subgraph, usually much smaller than GRAPH_REPORT.md or raw grep output.
- If graphify-out/wiki/index.md exists, use it for broad navigation instead of raw source browsing.
- Read graphify-out/GRAPH_REPORT.md only for broad architecture review or when query/path/explain do not surface enough context.
- After modifying code, run `graphify update .` to keep the graph current (AST-only, no API cost).

<!-- gitnexus:start -->
# GitNexus — Code Intelligence

This project is indexed by GitNexus as **CLI** (492 symbols, 866 relationships, 31 execution flows). Use the GitNexus MCP tools to understand code, assess impact, and navigate safely.

> If any GitNexus tool warns the index is stale, run `npx gitnexus analyze` in terminal first.

## Always Do

- **MUST run impact analysis before editing any symbol.** Before modifying a function, class, or method, run `gitnexus_impact({target: "symbolName", direction: "upstream"})` and report the blast radius (direct callers, affected processes, risk level) to the user.
- **MUST run `gitnexus_detect_changes()` before committing** to verify your changes only affect expected symbols and execution flows.
- **MUST warn the user** if impact analysis returns HIGH or CRITICAL risk before proceeding with edits.
- When exploring unfamiliar code, use `gitnexus_query({query: "concept"})` to find execution flows instead of grepping. It returns process-grouped results ranked by relevance.
- When you need full context on a specific symbol — callers, callees, which execution flows it participates in — use `gitnexus_context({name: "symbolName"})`.

## Never Do

- NEVER edit a function, class, or method without first running `gitnexus_impact` on it.
- NEVER ignore HIGH or CRITICAL risk warnings from impact analysis.
- NEVER rename symbols with find-and-replace — use `gitnexus_rename` which understands the call graph.
- NEVER commit changes without running `gitnexus_detect_changes()` to check affected scope.

## Resources

| Resource | Use for |
|----------|---------|
| `gitnexus://repo/CLI/context` | Codebase overview, check index freshness |
| `gitnexus://repo/CLI/clusters` | All functional areas |
| `gitnexus://repo/CLI/processes` | All execution flows |
| `gitnexus://repo/CLI/process/{name}` | Step-by-step execution trace |

## CLI

| Task | Read this skill file |
|------|---------------------|
| Understand architecture / "How does X work?" | `.claude/skills/gitnexus/gitnexus-exploring/SKILL.md` |
| Blast radius / "What breaks if I change X?" | `.claude/skills/gitnexus/gitnexus-impact-analysis/SKILL.md` |
| Trace bugs / "Why is X failing?" | `.claude/skills/gitnexus/gitnexus-debugging/SKILL.md` |
| Rename / extract / split / refactor | `.claude/skills/gitnexus/gitnexus-refactoring/SKILL.md` |
| Tools, resources, schema reference | `.claude/skills/gitnexus/gitnexus-guide/SKILL.md` |
| Index, status, clean, wiki CLI commands | `.claude/skills/gitnexus/gitnexus-cli/SKILL.md` |

<!-- gitnexus:end -->
