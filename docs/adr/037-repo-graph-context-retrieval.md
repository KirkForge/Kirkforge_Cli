# ADR-037: Repo-Graph Context Retrieval (Prototype)

**Status:** Experimental (2026-07-21)

## Context

The 6th-pass review named context-management as C+ — the single biggest gap vs. Vix. `grep -rn 'symbol_graph|call_graph|dependency_graph|import_graph|tree_sitter|tree-sitter' src/` → 0 hits. No repo graph, no symbol graph, no call graph, no import graph used for *context retrieval*. The model gets whatever the user pointed at + whatever it grep/globs itself.

Vix's differentiator is stem-agent cache reuse + tree-sitter virtual filesystem for token efficiency. Without a repo-graph index, the prompt builder has no way to inject relevant symbols/files/lines before every turn.

## Decision

Build `crates/kirkforge-context-index/` — a tree-sitter-backed symbol/import/call-graph index with a `retrieve(query, k)` API that the prompt builder calls every turn.

**Phase 1 (this ADR):** Scaffold the crate with line-based heuristic symbol extraction (no tree-sitter dep yet). Validate the API shape. Status: **Experimental** — not a load-bearing decision yet.

**Phase 2 (future):** Add tree-sitter grammars for Rust/TS/Python/Go. Add import-graph edges (reuse `tool-graphify`'s extension-resolution logic, ported to Rust). Add call-graph edges (tree-sitter queries for `fn`/`def`/`function` + call sites). Add git-history relevance graph.

**Phase 3 (future):** Wire `retrieve()` into the prompt builder (`src/session/prompt/mod.rs`). Cache the index on disk (`.kirkforge/context-index/`). Rebuild on git HEAD change.

## Implementation

- `crates/kirkforge-context-index/src/lib.rs`: `ContextIndex` struct with `index_file`, `index_dir`, `symbols`, `retrieve`. `Symbol` struct with `name`, `kind`, `file`, `line`, `end_line`. `SymbolKind` enum: `Function, Struct, Enum, Impl, Module, Use`.
- Line-based heuristic extraction (ponytail: upgrade path is tree-sitter).
- Substring-match retrieval (ponytail: upgrade path is embeddings or graph-walk).

## Consequences

**Positive:**
- Validates the API shape before adding the tree-sitter dep (~2MB binary size).
- Enables the prompt-builder integration in Phase 3.
- 3 tests pass.

**Negative:**
- Line-based heuristics miss inline declarations, macros, and non-standard syntax.
- No import/call-graph edges yet — retrieval is substring-only.
- No disk caching — index is rebuilt on every session start.

**Neutral:**
- Status is Experimental, not Accepted. The crate may be replaced or removed if tree-sitter integration proves infeasible.
