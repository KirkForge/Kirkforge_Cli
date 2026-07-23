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

## Implementation

- `crates/kirkforge-context-index/src/lib.rs`: `ContextIndex` struct with `index_file`, `index_dir`, `symbols`, `edges`, `call_edges`, `retrieve`. `Symbol` struct with `name`, `kind`, `file`, `line`, `end_line`. `SymbolKind` enum: `Function, Struct, Enum, Impl, Module, Use, Class, Interface, TypeAlias`. `ImportEdge` struct with `source_file`, `imported_symbol`, `resolved_file`, `line`. `CallEdge` struct with `caller_file`, `caller_name`, `caller_line`, `callee_name`, `callee_file`. `CallSite` struct with `caller_name`, `caller_file`, `line`. `RetrievalResult` struct with `symbol`, `imported_by`, `called_by`.
- Tree-sitter parsing for Rust (tree-sitter 0.25, tree-sitter-rust 0.24), TypeScript (tree-sitter-typescript 0.23), Python (tree-sitter-python 0.23), and Go (tree-sitter-go 0.23).
- `Language` enum (`Rust`, `TypeScript`, `Python`, `Go`) with `detect_language(path)` — dispatches `.rs` → Rust, `.ts`/`.tsx` → TypeScript, `.py` → Python, `.go` → Go.
- Import edge extraction: `extract_import_edges()` walks the AST for `use_declaration`/`import_statement`/`import_from_statement`/`import_declaration` nodes and extracts specifiers via `extract_import_specifier()`. `resolve_imports()` resolves specifiers to file paths.
- Call-graph edge extraction: `extract_call_edges()` walks the AST for call-expression nodes (Rust `call_expression`/`method_call_expression`, TS `call_expression`, Python `call`, Go `call_expression`). `extract_callee_name()` extracts the callee identifier (last identifier for method calls). `find_enclosing_function()` walks up the tree to find the enclosing function/method name. `resolve_call_edges()` resolves callee names to known symbol files.
- `retrieve()` returns `Vec<RetrievalResult>` (symbol + `imported_by` files + `called_by` call sites). `retrieve_symbols()` returns `Vec<Symbol>` for backward compatibility.
- Substring-match retrieval (ponytail: upgrade path is embeddings or graph-walk).
- Wired into `PromptBuilder` via `with_context_index()`. Index built at session start in `run_session()`. Relevant symbols section now includes "imported by" context.
- Disk caching: `CachedIndex` struct with `head` (git HEAD SHA) + `symbols` + `edges` + `call_edges`. `save()`, `load()`, `is_current()`. Cache at `.kirkforge/context-index/cache.json`. Rebuild on HEAD mismatch.

## Consequences

**Positive:**
- Accurate symbol extraction with proper line ranges (not just declaration line).
- Catches inline declarations that line-based heuristics miss.
- Model gets relevant symbols injected before every turn.
- 5 tests pass (3 original + 2 new: inline struct, end_line) → 10 tests pass (+ 5 new: save/load roundtrip, cache hit, cache miss, head differs, from_symbols) → 15 tests pass (+ 5 new: TS function, TS class, TS interface, dir walks TS files, detect_language) → 18 tests pass (+ 3 new: Python function, Python class, dir walks .py files) → 22 tests pass (+ 4 new: Go function, Go struct, Go method, dir walks .go files) → **27 tests pass (+ 5 new: import edge Rust use, import edge TS relative, import edge Python from, import edge unresolvable, retrieve includes importers) → 32 tests pass (+ 5 new: call edge Rust function call, call edge TS method call, call edge Python call, call edge unresolvable callee, retrieve includes callers).**

**Negative:**
- Tree-sitter adds ~2MB to the binary size (documented tradeoff).
- Rust + TypeScript + Python + Go — call-graph edges are implemented for all four languages.
- Import resolution is best-effort: bare specifiers (node_modules, PyPI packages, Go modules) are stored with `resolved_file: None`. Only relative and `crate::` imports are resolved.
- No disk caching — index is rebuilt on every session start → **Fixed in Phase 4: cache at `.kirkforge/context-index/cache.json` with git-HEAD invalidation.**
- Call-graph resolution is name-based (no type-aware dispatch). Method calls extract only the method name, not the receiver type.
- Call-graph edges not yet implemented — retrieval is substring + import-graph, not call-graph. → **Fixed in Phase 6: call-graph edges added for Rust/TS/Python/Go.**

**Neutral:**
- Status moved from Experimental to Accepted (tree-sitter integration proved feasible).
- The `retrieve()` API is stable; only the extraction internals changed.

