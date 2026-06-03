# Scavenged Ideas for KirkForge

These documents capture ideas scavenged from three reference projects:

| Source | Language | Focus |
|--------|----------|-------|
| [claude-code-rust](https://github.com/lorryjovens-hub/claude-code-rust) | Rust | Claude Code CLI reimplementation — architecture patterns, tool registry, cost tracking |
| [vix](https://github.com/kirby88/vix) | Go | Ollama-compatible CLI — workflow engine, skills system, VFS minification, sandbox |
| [KirkForge-Plugin](../plugins/) | TypeScript | Event bus for deterministic actions — lint/type-check/git-state without LLM tool calls |

Each document describes one idea, why it matters for KirkForge (token savings, safety, UX), and a sketch of how to integrate it into the existing Rust codebase.

## Index

1. [Event Bus — Deterministic Actions](event-bus.md) — Run lint, type-check, git diff, import graph as local deterministic listeners, not LLM tool calls
2. [Verifier Slot System & Correction Loop](verifier-correction-loop.md) — Canonical verifier slots (lint/types/security/graph) with truth-model precedence table — derived from the event bus
3. [Cost Tracking & Pricing Tables](cost-tracking.md) — Per-model pricing tables with cache-aware calculation, per-turn cost display, budget caps
4. [Workflow Engine](workflow-engine.md) — DAG of steps (agent/tool/bash) with fork_from, variable interpolation, Plan/Explore/Execute/Review phases
5. [Skills System](skills-system.md) — User-definable slash commands via SKILL.md files with YAML frontmatter, tool allowlists, per-skill model overrides
6. [Deny List & Access Control](deny-list.md) — Path deny list, symlink safety, automatic directory access mode, tool reason fields, read-before-edit gate
7. [Background Bash Jobs](background-bash.md) — Spawn/detach/poll long-running commands without blocking the agent loop
8. [Headless JSON Output](headless-json.md) — Machine-parseable structured output mode for CI/CD integration
9. [VFS & Tree-Sitter Minification](vfs-minification.md) — Upgrade from regex-based minifier to tree-sitter AST-aware minification with per-language formatters
10. [Prompt Cache & Stem Agent Pattern](prompt-cache-stem.md) — Share system prompt across workflow phases for prompt cache hits
11. [Path Safety — Atomic Writes & Guards](path-safety.md) — 10-point write guard system: sandbox containment, symlink checks, size limits, binary detection, atomic temp+rename
12. [Config Bootstrap & Layered Config](config-bootstrap.md) — First-run default generation, layered ~/.kirkforge + ./.kirkforge resolution, env var overrides
13. [Session Forking](session-forking.md) — Fork conversation history at turn boundary for branching workflows