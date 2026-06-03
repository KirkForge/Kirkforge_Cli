# Project State

Last updated: 2026-06-03

## Milestone Progress

| # | Milestone | Status | Notes |
|---|-----------|--------|-------|
| 1 | Ollama connection + streaming | ✅ Done | 4 adapters: GLM, DeepSeek, Gemini, OpenAI-compat fallback |
| 2 | Tools (read/write/edit/bash/grep/glob) | ✅ Done | All 6 tools with real implementations |
| 3 | TUI — chat view + input + streaming output | ✅ Done | ratatui + crossterm, dirty-flag event loop |
| 4 | Model abstraction — StreamEvent + per-model adapters | ✅ Done | GLM thinking field, DeepSeek tool blocks, Gemini streaming |
| 5 | Approval gates + tool dispatch | ✅ Done | Tiered approval, diff display, auto-deny/approve, always-approve persists to config |
| 6 | Session persistence (NDJSON logs) | ✅ Done | Append-only, crash-safe, resume support |
| 7 | Cross-compile CI (x86_64, aarch64, armv7 musl) | ✅ Done | CI workflows + Cross.toml + static-pie verified (5.0 MB) |
| 8 | VFS prompt compression (minifier) | ✅ Done | Language-aware minification (Rust, Python, JS/TS, Go, Markdown) |
| 9 | Event bus + verifier slots | ✅ Done | 9 event kinds, 4 verifier slots, lint/security/git verifiers, correction loop |
| 10 | Deny list + path safety | ✅ Done | 6-layer path guard, read-before-edit, binary detection, symlink guards |
| 11 | Config bootstrap | ✅ Done | Layered resolution, env var overrides, partial merge |
| 12 | Skills system | ✅ Done | SKILL.md frontmatter parser, slash command registry, wired into TUI |
| 13 | Session forking + background bash | ✅ Done | ForkManager, BashJobRegistry, wired into TUI |
| 14 | VFS tree-sitter minification upgrade | ✅ Done | LazyLock cache, strip-test blocks, C++/Java/Ruby/Shell support |
| 15 | Workflow engine | ✅ Done | DAG steps, conditions, loops, variable interpolation |
| 16 | Prompt cache stem | ✅ Done | Cache-aware build_stem(), hit probability estimator |

## Compilation Status

- **Rust toolchain**: stable (2026-06-03)
- **Build**: ✅ Clean — 0 errors, 0 warnings (clippy `-D warnings`)
- **Tests**: 146 unit tests pass, 7 integration tests (require Ollama, marked `#[ignore]`)
- **Release binary (gnu)**: 4.6 MB stripped, ELF x86-64, LTO + panic=abort
- **Release binary (musl, static)**: 5.0 MB, static-pie linked, no dynamic deps — verified
- **CI**: `ci.yml` (fmt/clippy/test/release) + `cross-compile.yml` (x86_64/aarch64/armv7 musl matrix + release publish)
- **Source**: 41 files, ~10,545 lines of Rust

## Repo

Git log (14 commits):

```
491f082 Wire Skills system into TUI, Event Bus + Verifiers into Executor
27290cf Phase 10: Prompt cache stem agent
ce53148 Phase 9: Workflow engine — DAG steps, variable interpolation
2075ce6 Phase 8: VFS tree-sitter minification upgrade
b304de4 Phase 5-7: Config bootstrap + Skills + Session forking + Background bash
52125d3 Phase 2-4: Access control, event bus, verifier slots
c42985b Wire minifier, add executor tests, fix always-approve persistence
69831cb Compilation fixes: resolve all build errors and warnings
e822ccf Cleanup: remove unused imports, fix warning sources
cb447cc Scaffold complete Rust project — all layers implemented
53f5fb9 Add .gitignore — state.md excluded
1c43b0b Add CLAUDE.md with architecture overview
87aac09 ADR 001 accepted, ADRs 002-005 added
4644fe1 Cross-compile CI setup & state.md update
```

## Source Tree

```
src/
├── main.rs                              — CLI entry, config bootstrap, headless modes, --max-cost
├── adapters/
│   ├── mod.rs                           — trait + adapter_for() factory
│   ├── glm.rs                           — GLM-5.1 adapter (thinking field)
│   ├── deepseek.rs                      — DeepSeek-v4-Pro adapter (CoT + tool blocks)
│   ├── gemini.rs                        — Gemini 3.0 adapter (OpenAI compat)
│   └── openai_compat.rs                 — Fallback SSE adapter
├── session/
│   ├── mod.rs                           — data dir, config loader, session IDs
│   ├── access.rs                        — DenyList, PathGuard, ReadGate, 10-point write guard system
│   ├── bash_jobs.rs                     — Background bash job registry (global singleton)
│   ├── config.rs                        — TOML config read/write, layered resolution
│   ├── conversation.rs                  — NDJSON append-only log
│   ├── event_bus.rs                     — 9 event kinds, EventHandler trait, publish/subscribe
│   ├── executor.rs                      — Turn loop: stream → tool dispatch → verifier → repeat
│   ├── prompt.rs                        — Handlebars system prompt + cache stem + budget truncation
│   ├── session_fork.rs                  — ForkManager for branching sessions
│   ├── skills.rs                        — SkillRegistry with SKILL.md frontmatter loader
│   └── verifier/
│       ├── mod.rs                       — VerifierSlots, CorrectionLoop, Verdict type
│       ├── git.rs                       — Git-aware verifier (pre-commit checks)
│       ├── lint.rs                      — Source lint verifier
│       └── security.rs                  — Security pattern verifier
├── shared/
│   ├── mod.rs                           — Core types (Message, StreamEvent, Config, etc.)
│   └── minify.rs                        — Language-aware source minification (8 languages)
├── tools/
│   ├── mod.rs                           — Tool trait + all_tools(), deny list + path guard checks
│   ├── read_file.rs                     — Line-offset file reading
│   ├── write_file.rs                    — Full file write with parent dir creation
│   ├── edit_file.rs                     — String-match edit with fuzzy fallback + diff
│   ├── bash.rs                          — Shell command with timeout + read-only detection
│   ├── bash_cancel.rs                   — Cancel background bash job
│   ├── bash_status.rs                   — Check background job status/output
│   ├── grep.rs                          — Recursive pattern search with context
│   └── glob.rs                          — gitignore-aware glob matching
├── tui/
│   ├── mod.rs                           — Event loop + input handling + approval routing + skill dispatch
│   ├── app.rs                           — AppState, ConnectionState, ConversationEntry, fork UI
│   ├── rendering.rs                     — Syntax highlighting + markdown rendering
│   ├── components/
│   │   ├── mod.rs
│   │   └── approval.rs                  — Approval dialog overlay
│   └── widgets/
│       ├── mod.rs
│       ├── chat.rs                      — Chat panel with timestamps + role colors
│       ├── input.rs                     — Input bar with cursor + placeholder
│       └── status.rs                    — Status bar (model, tokens, elapsed, cost)
└── workflow/
    └── mod.rs                           — DAG engine: steps, conditions, loops, variable interpolation
```

## Key Config Files

- `.claude/settings.json` — Permission allowlist (cargo clippy/check, graphify, ollama read-only)
- `Cross.toml` — cross-rs Docker config for aarch64/armv7 musl cross-compilation
- `.github/workflows/ci.yml` — Native x86_64-gnu (fmt/clippy/test/release)
- `.github/workflows/cross-compile.yml` — Cross-compile matrix + release publish (3 archs)

## Binary Sizes

| Target | Size | Type | Notes |
|--------|------|------|-------|
| x86_64-unknown-linux-gnu | 4.6 MB | ELF, LTO + strip + panic=abort | dev workstation |
| x86_64-unknown-linux-musl | 5.0 MB | static-pie, no dynamic deps | Linux servers, containers |
| aarch64-unknown-linux-musl | — | static (via cross Docker) | ARM servers, Raspberry Pi 3+/4/5, Huawei P30 |
| armv7-unknown-linux-musleabihf | — | static (via cross Docker) | Raspberry Pi 0/1/2, ARM 32-bit SBCs |

## Ollama Integration Tests

7 tests in `tests/integration_test.rs`, run with:
```
cargo test --test integration_test -- --ignored --nocapture
```
Requires `qwen2.5:0.5b` model on local Ollama (cloud gateway routes to DeepSeek/GLM/Kimi).

## Done This Session

- **Graphify + GitNexus analysis**: Fresh graph built (1,071 nodes, 1,906 edges, 52 communities). GitNexus indexed 1,370 symbols, 118 execution flows. No dead code, no concurrency issues, no CRITICAL blast radius found.
- **Bug fix — verifiers never fired in production**: `Executor::with_log()` pre-registered an empty `VerifierHandler` on the event bus, then `init_default_verifiers()` tried to register a second one with the same ID — bus rejected it. Security/lint/git verifiers were silently never called during a real session. Fixed: removed the empty pre-registration, `with_log()` now immediately calls `init_default_verifiers()` which is the sole registration point.
- **Full audit**: 146/146 tests pass, clippy clean, build clean. Graph's "unwired minification" and "missing cost display" claims were false positives — both are correctly wired.