# ADR-037: Repo-Graph Context Retrieval

**Status:** Accepted (2026-07-21)

## Context

The 6th-pass review named context-management as C+ — the single biggest gap vs. Vix. `grep -rn 'symbol_graph|call_graph|dependency_graph|import_graph|tree_sitter|tree-sitter' src/` → 0 hits. No repo graph, no symbol graph, no call graph, no import graph used for *context retrieval*. The model gets whatever the user pointed at + whatever it grep/globs itself.

Vix's differentiator is stem-agent cache reuse + tree-sitter virtual filesystem for token efficiency. Without a repo-graph index, the prompt builder has no way to inject relevant symbols/files/lines before every turn.

## Decision

Build `crates/kirkforge-context-index/` — a tree-sitter-backed symbol/import/call-graph index with a `retrieve(query, k)` API that the prompt builder calls every turn.

**Phase 1 (scaffold):** Line-based heuristic symbol extraction. Validated the API shape. **Done.**

**Phase 2 (tree-sitter):** Tree-sitter parsing for Rust. Extracts `function_item`, `struct_item`, `enum_item`, `impl_item`, `mod_item`, `use_declaration` nodes with accurate line ranges. **Done (Rust).**

**Phase 5 (multi-language):** TypeScript grammar added. `detect_language()` dispatches `.rs` → Rust, `.ts`/`.tsx` → TypeScript. `SymbolKind` extended with `Class`, `Interface`, `TypeAlias` for TS-specific declarations. `index_dir` walks both `.rs` and `.ts`/`.tsx` files. Python grammar added. `detect_language()` dispatches `.py` → Python. Extracts `function_definition`, `class_definition`, `import_statement`, `import_from_statement`, `decorated_definition`. `index_dir` walks `.py` files. Go grammar added. `detect_language()` dispatches `.go` → Go. Extracts `function_declaration`, `method_declaration`, `type_declaration` (with `type_spec` dispatch for `Struct`/`Interface`/`TypeAlias`), `import_declaration`. `index_dir` walks `.go` files. **Done (Rust + TypeScript + Python + Go).**

**Phase 3 (wire-in):** `retrieve()` called from the prompt builder before every turn. Injects up to 10 relevant symbols as a "Relevant symbols:" section. **Done.**

**Phase 4 (disk caching):** Cache at `.kirkforge/context-index/cache.json` with git-HEAD-based invalidation. On session start, if cache exists and HEAD matches, load from disk (instant). Otherwise rebuild and save. **Done.**

**Phase 4+ (future):** Call-graph edges (tree-sitter queries for call sites: `fn`/`def`/`function` calls). Embeddings or graph-walk retrieval (replace substring match).

**Phase 6 (import-graph edges):** `ImportEdge` struct with `source_file`, `imported_symbol`, `resolved_file`, `line`. `resolve_import()` resolves relative imports (TS `./utils` → `./utils.ts`), Rust `crate::` imports, and Python relative imports to file paths. External/bare imports stored with `resolved_file: None`. `retrieve()` returns `RetrievalResult` (symbol + `imported_by` files). `CachedIndex` now includes edges. `index_dir` calls `resolve_imports()` after indexing all files. **Done (import edges for Rust/TS/Python/Go).**

**Phase 6+ (call-graph edges):** `CallEdge` struct with `caller_file`, `caller_name`, `caller_line`, `callee_name`, `callee_file`. `CallSite` struct with `caller_name`, `caller_file`, `line`. `extract_call_edges()` walks the AST for call expressions and extracts callee name + enclosing function name. `resolve_call_edges()` matches callee names to known symbols. `retrieve()` returns `called_by: Vec<CallSite>`. Supports Rust (`call_expression`, `method_call_expression`), TypeScript (`call_expression`), Python (`call`), Go (`call_expression`). For method calls like `obj.method()`, extracts just `method` as the callee name. **Done (import + call-graph edges for Rust/TS/Python/Go).**

**Phase 7 (embeddings + graph-walk retrieval):** Implemented. Three retrieval strategies dispatched by query shape, replacing the substring-only `retrieve()` at the prompt-builder call site:

- **TF-IDF embeddings** (`crates/kirkforge-context-index/src/embeddings.rs`): pure-Rust sparse vectors (`Vec<(usize, f32)>`), no ML runtime. Tokenizes each symbol's name (snake_case + camelCase splitting) and kind (`fn`/`struct`/…), builds a vocabulary across all symbols, computes a TF-IDF sparse vector per symbol, and exposes a cosine-similarity function (hand-written dot product + L2 norms over sorted sparse vectors). `SymbolEmbedding` records are persisted in `CachedIndex` (serde `#[serde(default)]` for back-compat with pre-Phase-7 caches) so IDF is not recomputed on every load. A free-text query is embedded with the same tokenizer (no kind token added) and ranked by cosine similarity against every symbol.
- **Graph-walk BFS** (`crates/kirkforge-context-index/src/graph_walk.rs`): `graph_walk(start, index, max_hops)` BFS over both directions of the import + call-graph edges — `imported_by`/`called_by` (who depends on this symbol) and `imports`/`calls` (what this symbol depends on). Deduplicates by `(file, name)` keeping the minimum hop distance; caps at `max_hops` (default 2).
- **Hybrid retrieval** (`retrieve_hybrid`): exact symbol-name match → graph walk from that symbol, ranked by hop distance; free text → embedding cosine similarity, top-N; substring → falls back to the original `retrieve()`. Exposed as both a `ContextIndex::retrieve_hybrid` method and a free function `retrieve_hybrid(query, index, max_results)`.

`PromptBuilder` now calls `retrieve_hybrid` instead of `retrieve`. Binary-size impact: zero new deps (the embeddings module is pure Rust over the existing `serde`/`tree-sitter`/`walkdir` set).

**Phase 7.1 (embedding quality):** WO 8.4. Improved the TF-IDF tokenizer and graph-walk ranking so retrieval quality is measurable:

- **Tokenizer** (`embeddings.rs::split_identifier`): now strips code-specific punctuation (`<`, `>`, `&`, `*`, `'`, `!`) as separators so generics (`Vec<T>` → `vec`, `t`), lifetimes (`foo<'a>` → `foo`, `a`), macro invocations (`println!` → `println`), references (`&str` → `str`), and pointers (`*const T` → `const`, `t`) tokenize into semantically meaningful tokens instead of leaving stray bracket/amp punctuation in the bag. Path qualifiers (`std::collections::HashMap`) already split on `::`. `tokenize_symbol` emits doc-comment tokens twice (weight 2x) — `///` doc text is more semantically meaningful than code identifiers, so a free-text query matching a doc comment ranks the owning symbol higher.
- **Edge weighting** (`graph_walk.rs`): call-graph edges use weight 1.0 (calling is a strong relationship); import edges use weight 0.5 (importing is weaker); same-file symbols get a `+0.3` bonus. Ranking score is `edge_weight / hop_distance + same_file_bonus`, sorted descending — so a callee at hop 1 outranks an importer at hop 1, and both outrank a hop-2 symbol. (The workorder's literal formula `1.0/(hop*weight)` was inverted relative to its stated intent — strong edges would score lower — so the impl uses `weight/hop` to honor "call ranks higher than import".)
- **Retrieval quality tests**: 11 new tests across `embeddings.rs` and `graph_walk.rs` — tokenizer handles generics/lifetimes/macros/paths/refs, identical symbols have similarity > 0.8, unrelated symbols < 0.3, doc-comment-weighted retrieval finds a struct by its doc text, graph walk finds a function by exact name, closer symbols rank higher, call edges rank higher than import edges at the same hop, same-file symbols rank higher than cross-file.

## Implementation

- `crates/kirkforge-context-index/src/lib.rs`: `ContextIndex` struct with `index_file`, `index_dir`, `symbols`, `edges`, `call_edges`, `retrieve`. `Symbol` struct with `name`, `kind`, `file`, `line`, `end_line`. `SymbolKind` enum: `Function, Struct, Enum, Impl, Module, Use, Class, Interface, TypeAlias`. `ImportEdge` struct with `source_file`, `imported_symbol`, `resolved_file`, `line`. `CallEdge` struct with `caller_file`, `caller_name`, `caller_line`, `callee_name`, `callee_file`. `CallSite` struct with `caller_name`, `caller_file`, `line`. `RetrievalResult` struct with `symbol`, `imported_by`, `called_by`.
- Tree-sitter parsing for Rust (tree-sitter 0.25, tree-sitter-rust 0.24), TypeScript (tree-sitter-typescript 0.23), Python (tree-sitter-python 0.23), and Go (tree-sitter-go 0.23).
- `Language` enum (`Rust`, `TypeScript`, `Python`, `Go`) with `detect_language(path)` — dispatches `.rs` → Rust, `.ts`/`.tsx` → TypeScript, `.py` → Python, `.go` → Go.
- Import edge extraction: `extract_import_edges()` walks the AST for `use_declaration`/`import_statement`/`import_from_statement`/`import_declaration` nodes and extracts specifiers via `extract_import_specifier()`. `resolve_imports()` resolves specifiers to file paths.
- Call-graph edge extraction: `extract_call_edges()` walks the AST for call-expression nodes (Rust `call_expression`/`method_call_expression`, TS `call_expression`, Python `call`, Go `call_expression`). `extract_callee_name()` extracts the callee identifier (last identifier for method calls). `find_enclosing_function()` walks up the tree to find the enclosing function/method name. `resolve_call_edges()` resolves callee names to known symbol files.
- `retrieve()` returns `Vec<RetrievalResult>` (symbol + `imported_by` files + `called_by` call sites). `retrieve_symbols()` returns `Vec<Symbol>` for backward compatibility.
- Substring-match retrieval (ponytail: upgrade path is embeddings or graph-walk).
- Phase 7 embeddings: `embeddings.rs` exposes `Vocabulary`, `SparseVec`, `SymbolEmbedding`, `tokenize_symbol`, `build_vocabulary`, `embed_symbol`, `embed_query`, `build_embeddings`, `cosine_similarity`, `dot_product`, `norm`. Sparse vectors are `Vec<(usize, f32)>` sorted by dimension; cosine is a linear-merge dot product over sorted vectors.
- Phase 7 graph-walk: `graph_walk.rs` exposes `graph_walk(start, index, max_hops) -> Vec<(Symbol, usize)>`. BFS over forward + reverse import/call edges; dedup by `(file, name)` keeping min hop; cap at `max_hops`.
- Phase 7 hybrid retrieval: `ContextIndex::retrieve_hybrid(query, k)` and free function `retrieve_hybrid(query, index, max_results)`. Exact-name → graph walk ranked by hop; free text → embedding cosine top-N; substring → `retrieve()`. `PromptBuilder` calls `retrieve_hybrid`.
- Wired into `PromptBuilder` via `with_context_index()`. Index built at session start in `run_session()`. Relevant symbols section now includes "imported by" context.
- Disk caching: `CachedIndex` struct with `head` (git HEAD SHA) + `symbols` + `edges` + `call_edges`. `save()`, `load()`, `is_current()`. Cache at `.kirkforge/context-index/cache.json`. Rebuild on HEAD mismatch.

## Consequences

**Positive:**
- Accurate symbol extraction with proper line ranges (not just declaration line).
- Catches inline declarations that line-based heuristics miss.
- Model gets relevant symbols injected before every turn.
- 5 tests pass (3 original + 2 new: inline struct, end_line) → 10 tests pass (+ 5 new: save/load roundtrip, cache hit, cache miss, head differs, from_symbols) → 15 tests pass (+ 5 new: TS function, TS class, TS interface, dir walks TS files, detect_language) → 18 tests pass (+ 3 new: Python function, Python class, dir walks .py files) → 22 tests pass (+ 4 new: Go function, Go struct, Go method, dir walks .go files) → 27 tests pass (+ 5 new: import edge Rust use, import edge TS relative, import edge Python from, import edge unresolvable, retrieve includes importers) → 32 tests pass (+ 5 new: call edge Rust function call, call edge TS method call, call edge Python call, call edge unresolvable callee, retrieve includes callers) → **40 tests pass (+ 8 new in Phase 7: identical-names similarity, unrelated-symbols similarity, empty-index embeddings, snake_case splits, camel_case splits, kind token distinguishes fn/struct, query matches related symbol, graph-walk no-edges returns only itself) → 46 tests pass (+ 6 new graph-walk: importer within 1 hop, callee within 1 hop, max_hops limits traversal, dedup keeps min hop, missing start symbol returns only start). → 57 tests pass (+ 11 new in Phase 7.1: tokenizer handles generics, lifetimes, macros, paths, refs; identical-symbol similarity > 0.8, unrelated < 0.3, doc-comment-weighted retrieval, graph walk finds function by name, closer symbols rank higher, call edge outranks import edge, same-file outranks cross-file).**

**Negative:**
- Tree-sitter adds ~2MB to the binary size (documented tradeoff).
- Rust + TypeScript + Python + Go — call-graph edges are implemented for all four languages.
- Import resolution is best-effort: bare specifiers (node_modules, PyPI packages, Go modules) are stored with `resolved_file: None`. Only relative and `crate::` imports are resolved.
- No disk caching — index is rebuilt on every session start → **Fixed in Phase 4: cache at `.kirkforge/context-index/cache.json` with git-HEAD invalidation.**
- Call-graph resolution is name-based (no type-aware dispatch). Method calls extract only the method name, not the receiver type.
- Call-graph edges not yet implemented — retrieval is substring + import-graph, not call-graph. → **Fixed in Phase 6: call-graph edges added for Rust/TS/Python/Go.**
- Embeddings or graph-walk retrieval not yet implemented — retrieval is substring + import/call-graph only. → **Fixed in Phase 7: TF-IDF embeddings + graph-walk BFS + hybrid retrieval implemented.**
- TF-IDF tokenizer did not handle code-specific syntax (lifetimes, generics, macros, doc comments) and graph-walk treated all edges equally → **Fixed in Phase 7.1: tokenizer strips code punctuation and weights doc comments 2x; graph-walk weights call edges above import edges and same-file above cross-file.**

**Neutral:**
- Status moved from Experimental to Accepted (tree-sitter integration proved feasible).
- The `retrieve()` API is stable; only the extraction internals changed.

