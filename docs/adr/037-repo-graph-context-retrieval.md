# ADR-037: Repo-Graph Context Retrieval

**Status:** Accepted (2026-07-21)

## Context

The 6th-pass review named context-management as C+ — the single biggest gap vs. Vix. `grep -rn 'symbol_graph|call_graph|dependency_graph|import_graph|tree_sitter|tree-sitter' src/` → 0 hits. No repo graph, no symbol graph, no call graph, no import graph used for *context retrieval*. The model gets whatever the user pointed at + whatever it grep/globs itself.

Vix's differentiator is stem-agent cache reuse + tree-sitter virtual filesystem for token efficiency. Without a repo-graph index, the prompt builder has no way to inject relevant symbols/files/lines before every turn.

## Decision

Build `crates/kirkforge-context-index/` — a tree-sitter-backed symbol/import/call-graph index with a `retrieve(query, k)` API that the prompt builder calls every turn.

**Phase 1 (scaffold):** Line-based heuristic symbol extraction. Validated the API shape. **Done.**

**Phase 2 (tree-sitter):** Tree-sitter parsing for Rust. Extracts `function_item`, `struct_item`, `enum_item`, `impl_item`, `mod_item`, `use_declaration` nodes with accurate line ranges. **Done (Rust).**

**Phase 5 (multi-language):** TypeScript grammar added. `detect_language()` dispatches `.rs` → Rust, `.ts`/`.tsx` → TypeScript. `SymbolKind` extended with `Class`, `Interface`, `TypeAlias` for TS-specific declarations. `index_dir` walks both `.rs` and `.ts`/`.tsx` files. **In progress (Rust + TypeScript).** Future: Python/Go grammars.

**Phase 3 (wire-in):** `retrieve()` called from the prompt builder before every turn. Injects up to 10 relevant symbols as a "Relevant symbols:" section. **Done.**

**Phase 4 (disk caching):** Cache at `.kirkforge/context-index/cache.json` with git-HEAD-based invalidation. On session start, if cache exists and HEAD matches, load from disk (instant). Otherwise rebuild and save. **Done.**

**Phase 4+ (future):** Import-graph edges (reuse `tool-graphify`'s logic). Call-graph edges (tree-sitter queries for call sites). Embeddings or graph-walk retrieval (replace substring match). Python/Go grammars (Phase 5 continuation).

## Implementation

- `crates/kirkforge-context-index/src/lib.rs`: `ContextIndex` struct with `index_file`, `index_dir`, `symbols`, `retrieve`. `Symbol` struct with `name`, `kind`, `file`, `line`, `end_line`. `SymbolKind` enum: `Function, Struct, Enum, Impl, Module, Use, Class, Interface, TypeAlias`.
- Tree-sitter parsing for Rust (tree-sitter 0.25, tree-sitter-rust 0.24) and TypeScript (tree-sitter-typescript 0.23).
- `Language` enum (`Rust`, `TypeScript`) with `detect_language(path)` — dispatches `.rs` → Rust, `.ts`/`.tsx` → TypeScript.
- Substring-match retrieval (ponytail: upgrade path is embeddings or graph-walk).
- Wired into `PromptBuilder` via `with_context_index()`. Index built at session start in `run_session()`.
- Disk caching: `CachedIndex` struct with `head` (git HEAD SHA) + `symbols`. `save()`, `load()`, `is_current()`. Cache at `.kirkforge/context-index/cache.json`. Rebuild on HEAD mismatch.

## Consequences

**Positive:**
- Accurate symbol extraction with proper line ranges (not just declaration line).
- Catches inline declarations that line-based heuristics miss.
- Model gets relevant symbols injected before every turn.
- 5 tests pass (3 original + 2 new: inline struct, end_line) → 10 tests pass (+ 5 new: save/load roundtrip, cache hit, cache miss, head differs, from_symbols) → **15 tests pass (+ 5 new: TS function, TS class, TS interface, dir walks TS files, detect_language).**

**Negative:**
- Tree-sitter adds ~2MB to the binary size (documented tradeoff).
- Rust + TypeScript only — Python/Go grammars are future work.
- No disk caching — index is rebuilt on every session start → **Fixed in Phase 4: cache at `.kirkforge/context-index/cache.json` with git-HEAD invalidation.**
- No import/call-graph edges yet — retrieval is substring-only.

**Neutral:**
- Status moved from Experimental to Accepted (tree-sitter integration proved feasible).
- The `retrieve()` API is stable; only the extraction internals changed.

