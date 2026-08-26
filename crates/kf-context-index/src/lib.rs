use std::path::PathBuf;

pub mod cache;
pub mod embeddings;
pub mod graph_walk;
pub mod parser;
pub mod resolve;

pub use cache::current_head;
pub use embeddings::{
    build_embeddings, build_vocabulary, cosine_similarity, embed_query, embed_symbol, SparseVec,
    SymbolEmbedding, Vocabulary,
};
pub use graph_walk::graph_walk as graph_walk_fn;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum SymbolKind {
    Function,
    Struct,
    Enum,
    Impl,
    Module,
    Use,
    Class,
    Interface,
    TypeAlias,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Language {
    Rust,
    TypeScript,
    Python,
    Go,
}

pub fn detect_language(path: &std::path::Path) -> Option<Language> {
    match path.extension().and_then(|s| s.to_str()) {
        Some("rs") => Some(Language::Rust),
        Some("ts") | Some("tsx") => Some(Language::TypeScript),
        Some("py") => Some(Language::Python),
        Some("go") => Some(Language::Go),
        _ => None,
    }
}

/// A single symbol extracted from source code.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Symbol {
    pub name: String,
    pub kind: SymbolKind,
    pub file: PathBuf,
    pub line: u32,
    pub end_line: u32,
    /// Doc comment text extracted from `///` or `/** */` above the symbol.
    /// `None` when no doc comment is present.
    #[serde(default)]
    pub doc: Option<String>,
}

/// An import edge: file A imports symbol/module from file B (or an external package).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ImportEdge {
    /// The file that contains the import statement.
    pub source_file: PathBuf,
    /// The raw import specifier (e.g., `std::collections::HashMap`, `./utils`, `from foo import bar`).
    pub imported_symbol: String,
    /// The resolved target file, if we could resolve it. None for external/unresolvable imports.
    pub resolved_file: Option<PathBuf>,
    /// Line number of the import statement.
    pub line: u32,
}

/// A call-graph edge: caller calls callee.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CallEdge {
    pub caller_file: PathBuf,
    pub caller_name: String,
    pub caller_line: u32,
    pub callee_name: String,
    pub callee_file: Option<PathBuf>,
}

/// A call site: who calls a given function.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CallSite {
    pub caller_name: String,
    pub caller_file: PathBuf,
    pub line: u32,
}

/// A retrieval result: a symbol plus the files that import it and call sites.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct RetrievalResult {
    pub symbol: Symbol,
    /// Files that import the file containing this symbol.
    pub imported_by: Vec<PathBuf>,
    /// Call sites that invoke this symbol.
    pub called_by: Vec<CallSite>,
}

/// Cached index metadata — the git HEAD at cache time plus the symbols and edges.
///
/// Stored as JSON at `.kf-code/context-index/cache.json`. The HEAD field
/// enables cache invalidation: if the current HEAD differs from the stored
/// HEAD, the cache is stale and must be rebuilt. The `format_version` field
/// (WO 43.21) invalidates the cache when the on-disk format changes — a
/// mismatch causes `load` to return `Err`, which callers treat as "rebuild".
pub const CURRENT_FORMAT_VERSION: u32 = 1;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CachedIndex {
    /// Format version stamp. A mismatch with [`CURRENT_FORMAT_VERSION`]
    /// causes `load` to return `Err` (rebuild signal) so an old cache never
    /// silently feeds a new code path. `#[serde(default)]` so caches written
    /// before this field existed (version 0) load and then trigger a rebuild.
    #[serde(default)]
    pub format_version: u32,
    /// The git HEAD SHA when this cache was written.
    pub head: String,
    /// The indexed symbols.
    pub symbols: Vec<Symbol>,
    /// The import edges.
    pub edges: Vec<ImportEdge>,
    /// The call-graph edges.
    pub call_edges: Vec<CallEdge>,
    /// Sparse TF-IDF embeddings per symbol (Phase 7). Persisted so
    /// the index does not recompute IDF on every load. May be empty
    /// for caches written before Phase 7 (serde default).
    #[serde(default)]
    pub embeddings: Vec<SymbolEmbedding>,
    /// File modification times (seconds since epoch) at index time.
    #[serde(default)]
    pub file_mtimes: std::collections::HashMap<String, u64>,
}

/// A tree-sitter-backed index of source-code symbols and import edges.
///
/// Uses tree-sitter grammars to extract function, struct, enum, impl, module,
/// and use declarations from Rust, TypeScript, Python, and Go source files.
/// Also extracts import edges showing which files import which modules.
/// The index is built by calling `index_file` or `index_dir`, then queried via
/// `retrieve`.
///
/// ponytail: Rust + TypeScript + Python + Go symbol extraction via tree-sitter.
/// Phase 6 complete. Import + call-graph edges for Rust/TS/Python/Go. The upgrade
/// path is embeddings/graph-walk retrieval (Phase 7).
///
/// ponytail: substring-match retrieval. The upgrade path is embeddings or
/// graph-walk retrieval.
///
/// ponytail: disk caching uses serde_json (not bincode — bincode is unmaintained).
/// The upgrade path is a compact binary format if JSON size becomes a concern.
pub struct ContextIndex {
    symbols: Vec<Symbol>,
    edges: Vec<ImportEdge>,
    call_edges: Vec<CallEdge>,
    /// Pre-computed TF-IDF embeddings loaded from `CachedIndex`. When
    /// non-empty and matching `symbols` by index, `retrieve_hybrid`
    /// uses them directly instead of rebuilding the vocabulary and
    /// re-embedding every symbol per query (WO 38.9 item 5).
    embeddings: Vec<SymbolEmbedding>,
}

impl Default for ContextIndex {
    fn default() -> Self {
        Self::new()
    }
}

impl ContextIndex {
    pub fn new() -> Self {
        Self {
            symbols: Vec::new(),
            edges: Vec::new(),
            call_edges: Vec::new(),
            embeddings: Vec::new(),
        }
    }

    /// Create an index from a pre-built symbol list (e.g., loaded from cache).
    pub fn from_symbols(symbols: Vec<Symbol>) -> Self {
        Self {
            symbols,
            edges: Vec::new(),
            call_edges: Vec::new(),
            embeddings: Vec::new(),
        }
    }

    /// Create an index from a pre-built symbol list and edge list.
    pub fn from_symbols_and_edges(symbols: Vec<Symbol>, edges: Vec<ImportEdge>) -> Self {
        Self {
            symbols,
            edges,
            call_edges: Vec::new(),
            embeddings: Vec::new(),
        }
    }

    /// Create an index from pre-built symbols, import edges, and call edges.
    pub fn from_symbols_and_edges_and_calls(
        symbols: Vec<Symbol>,
        edges: Vec<ImportEdge>,
        call_edges: Vec<CallEdge>,
    ) -> Self {
        Self {
            symbols,
            edges,
            call_edges,
            embeddings: Vec::new(),
        }
    }

    /// Create an index from a `CachedIndex`, including pre-computed
    /// embeddings so the query path does not rebuild them per query
    /// (WO 38.9 item 5). Embeddings whose `symbol_idx` is out of bounds
    /// are dropped to keep the invariant `embeddings` matches `symbols`.
    pub fn from_cached(cached: CachedIndex) -> Self {
        let len = cached.symbols.len();
        let embeddings = cached
            .embeddings
            .into_iter()
            .filter(|e| e.symbol_idx < len)
            .collect();
        Self {
            symbols: cached.symbols,
            edges: cached.edges,
            call_edges: cached.call_edges,
            embeddings,
        }
    }

    /// All extracted symbols.
    pub fn symbols(&self) -> &[Symbol] {
        &self.symbols
    }

    /// All extracted import edges.
    pub fn edges(&self) -> &[ImportEdge] {
        &self.edges
    }

    /// All extracted call edges.
    pub fn call_edges(&self) -> &[CallEdge] {
        &self.call_edges
    }

    pub fn call_edges_mut(&mut self) -> &mut [CallEdge] {
        &mut self.call_edges
    }

    /// Retrieve the first `k` symbols whose name contains `query` as a substring,
    /// along with the files that import the matched symbols' files.
    ///
    /// ponytail: substring-match retrieval. The upgrade path is
    /// embeddings or graph-walk retrieval.
    pub fn retrieve(&self, query: &str, k: usize) -> Vec<RetrievalResult> {
        self.symbols
            .iter()
            .filter(|s| s.name.contains(query))
            .take(k)
            .map(|sym| {
                let imported_by = self
                    .edges
                    .iter()
                    .filter(|e| e.resolved_file.as_ref() == Some(&sym.file))
                    .map(|e| e.source_file.clone())
                    .collect();
                let called_by = self
                    .call_edges
                    .iter()
                    .filter(|e| e.callee_name == sym.name)
                    .map(|e| CallSite {
                        caller_name: e.caller_name.clone(),
                        caller_file: e.caller_file.clone(),
                        line: e.caller_line,
                    })
                    .collect();
                RetrievalResult {
                    symbol: sym.clone(),
                    imported_by,
                    called_by,
                }
            })
            .collect()
    }

    /// Retrieve the first `k` symbols whose name contains `query` as a substring.
    /// Simplified API that returns just the symbols without import context.
    pub fn retrieve_symbols(&self, query: &str, k: usize) -> Vec<Symbol> {
        self.symbols
            .iter()
            .filter(|s| s.name.contains(query))
            .take(k)
            .cloned()
            .collect()
    }

    /// Hybrid retrieval: graph-walk for exact-name queries, embedding
    /// similarity for free-text queries, substring match as a fallback.
    ///
    /// ponytail: three retrieval strategies dispatched by query shape.
    /// The upgrade path is a unified ranker that fuses graph-walk hops
    /// with embedding similarity into a single score.
    pub fn retrieve_hybrid(&self, query: &str, k: usize) -> Vec<RetrievalResult> {
        if let Some(start) = self.symbols.iter().find(|s| s.name == query) {
            let walked = graph_walk::graph_walk(start, self, 2);
            return walked
                .into_iter()
                .take(k)
                .map(|(sym, _hops)| self.to_retrieval_result(&sym))
                .collect();
        }

        // WO 38.9 item 5: use pre-computed embeddings from the cached
        // index when available, avoiding per-query re-embedding of all
        // symbols. We still build the vocabulary (cheap — tokenize only)
        // to embed the query, but skip N embed_symbol calls.
        let vocab = build_vocabulary(&self.symbols);
        if !vocab.is_empty() {
            let qvec = embed_query(query, &vocab);
            if !qvec.is_empty() {
                let mut scored: Vec<(f32, &Symbol)> = if self.embeddings.len() == self.symbols.len()
                    && self
                        .embeddings
                        .iter()
                        .all(|e| e.symbol_idx < self.symbols.len())
                {
                    self.embeddings
                        .iter()
                        .map(|e| {
                            (
                                cosine_similarity(&qvec, &e.vector),
                                &self.symbols[e.symbol_idx],
                            )
                        })
                        .collect()
                } else {
                    self.symbols
                        .iter()
                        .map(|s| {
                            let v = embed_symbol(s, &vocab, s.doc.as_deref());
                            (cosine_similarity(&qvec, &v), s)
                        })
                        .collect()
                };
                scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
                let top: Vec<RetrievalResult> = scored
                    .into_iter()
                    .take(k)
                    .map(|(_, s)| self.to_retrieval_result(s))
                    .collect();
                if !top.is_empty() {
                    return top;
                }
            }
        }

        self.retrieve(query, k)
    }

    /// Build a `RetrievalResult` for a single symbol (shared by the
    /// graph-walk and embedding paths).
    fn to_retrieval_result(&self, sym: &Symbol) -> RetrievalResult {
        // Only edges that RESOLVED to this symbol's file count as
        // importers. Unresolved edges (resolved_file: None — external /
        // stdlib imports) belong to no symbol; the old
        // `is_none_or(rf == sym.file)` filter smeared every unresolved
        // edge in the index into every result, which on a large repo
        // produced multi-MB system prompts (7MB / >1M tokens observed).
        // ponytail: module-level attribution (file, not symbol); a
        // symbol-level edge model is the upgrade path.
        let mut seen_files = std::collections::HashSet::new();
        let imported_by = self
            .edges
            .iter()
            .filter(|e| e.resolved_file.as_ref() == Some(&sym.file))
            .filter(|e| seen_files.insert(e.source_file.clone()))
            .map(|e| e.source_file.clone())
            .collect();
        let called_by = self
            .call_edges
            .iter()
            .filter(|e| e.callee_name == sym.name)
            .map(|e| CallSite {
                caller_name: e.caller_name.clone(),
                caller_file: e.caller_file.clone(),
                line: e.caller_line,
            })
            .collect();
        RetrievalResult {
            symbol: sym.clone(),
            imported_by,
            called_by,
        }
    }
}

/// Free-function form of hybrid retrieval (Phase 7).
///
/// Dispatches by query shape:
/// - exact symbol-name match → BFS graph walk from that symbol,
///   ranked by hop distance.
/// - free text → TF-IDF embedding cosine similarity, top-N.
/// - substring → falls back to `ContextIndex::retrieve`.
///
/// `max_results` caps the returned list. The graph-walk hop cap is
/// fixed at 2 (the default from ADR-037 Phase 7); callers needing a
/// deeper walk should call `graph_walk::graph_walk` directly.
pub fn retrieve_hybrid(
    query: &str,
    index: &ContextIndex,
    max_results: usize,
) -> Vec<RetrievalResult> {
    index.retrieve_hybrid(query, max_results)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn index_file_extracts_fn_and_struct() {
        let tmp = std::env::temp_dir().join(format!("kf-code-context-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();

        let src = tmp.join("lib.rs");
        fs::write(
            &src,
            "fn foo() {}\nstruct Bar { x: i32 }\nfn baz() -> bool { true }\n",
        )
        .unwrap();

        let mut idx = ContextIndex::new();
        idx.index_file(&src, &fs::read_to_string(&src).unwrap())
            .unwrap();

        let syms = idx.symbols();
        assert_eq!(syms.len(), 3, "expected 3 symbols, got {syms:?}");

        assert_eq!(syms[0].name, "foo");
        assert_eq!(syms[0].kind, SymbolKind::Function);

        assert_eq!(syms[1].name, "Bar");
        assert_eq!(syms[1].kind, SymbolKind::Struct);

        assert_eq!(syms[2].name, "baz");
        assert_eq!(syms[2].kind, SymbolKind::Function);
    }

    #[test]
    fn retrieve_returns_matching_symbols() {
        let mut idx = ContextIndex::new();
        let tmp =
            std::env::temp_dir().join(format!("kf-code-context-retrieve-{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();

        let src = tmp.join("mod.rs");
        fs::write(&src, "fn foo_bar() {}\nfn baz() {}\nfn foo_baz() {}\n").unwrap();
        idx.index_file(&src, &fs::read_to_string(&src).unwrap())
            .unwrap();

        let results = idx.retrieve("foo", 2);
        assert_eq!(results.len(), 2, "expected 2 results, got {results:?}");
        assert!(results.iter().all(|s| s.symbol.name.contains("foo")));
    }

    #[test]
    fn index_dir_walks_rs_files() {
        let tmp = std::env::temp_dir().join(format!("kf-code-context-dir-{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();
        fs::create_dir_all(tmp.join("sub")).unwrap();

        fs::write(tmp.join("a.rs"), "fn a() {}\nstruct A;\n").unwrap();
        fs::write(tmp.join("sub").join("b.rs"), "fn b() {}\n").unwrap();
        // Non-.rs file should be skipped
        fs::write(tmp.join("c.txt"), "not code").unwrap();

        let mut idx = ContextIndex::new();
        idx.index_dir(&tmp).unwrap();

        let syms = idx.symbols();
        assert_eq!(syms.len(), 3, "expected 3 symbols, got {syms:?}");
        let names: Vec<&str> = syms.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"a"));
        assert!(names.contains(&"A"));
        assert!(names.contains(&"b"));
    }

    #[test]
    fn index_file_extracts_inline_struct() {
        let tmp =
            std::env::temp_dir().join(format!("kf-code-context-inline-{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();

        let src = tmp.join("lib.rs");
        fs::write(&src, "fn foo() { struct Bar; }\n").unwrap();

        let mut idx = ContextIndex::new();
        idx.index_file(&src, &fs::read_to_string(&src).unwrap())
            .unwrap();

        let syms = idx.symbols();
        assert_eq!(
            syms.len(),
            2,
            "expected 2 symbols (fn + struct), got {syms:?}"
        );
        assert_eq!(syms[0].name, "foo");
        assert_eq!(syms[0].kind, SymbolKind::Function);
        assert_eq!(syms[1].name, "Bar");
        assert_eq!(syms[1].kind, SymbolKind::Struct);
    }

    #[test]
    fn index_file_extracts_end_line() {
        let tmp =
            std::env::temp_dir().join(format!("kf-code-context-endline-{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();

        let src = tmp.join("lib.rs");
        fs::write(&src, "fn foo() {\n    let x = 1;\n    let y = 2;\n}\n").unwrap();

        let mut idx = ContextIndex::new();
        idx.index_file(&src, &fs::read_to_string(&src).unwrap())
            .unwrap();

        let syms = idx.symbols();
        assert_eq!(syms.len(), 1, "expected 1 symbol, got {syms:?}");
        assert_eq!(syms[0].name, "foo");
        assert_eq!(syms[0].line, 1);
        assert!(
            syms[0].end_line > syms[0].line,
            "end_line ({}) should be > line ({}) for multi-line function",
            syms[0].end_line,
            syms[0].line
        );
    }

    #[test]
    fn context_index_save_and_load_roundtrip() {
        let dir = std::env::temp_dir().join(format!(
            "kf-code-context-cache-roundtrip-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        let src = dir.join("lib.rs");
        fs::write(&src, "fn hello() {}\nstruct World;\n").unwrap();

        let mut idx = ContextIndex::new();
        idx.index_file(&src, &fs::read_to_string(&src).unwrap())
            .unwrap();
        let original_count = idx.symbols().len();
        assert!(original_count > 0, "index should have symbols");

        let cache_path = dir.join(".kf-code/context-index/cache.json");
        idx.save(&cache_path, "abc123").unwrap();

        let loaded = ContextIndex::load(&cache_path).unwrap();
        assert_eq!(loaded.head, "abc123");
        assert_eq!(loaded.symbols.len(), original_count);

        let idx2 = ContextIndex::from_symbols(loaded.symbols);
        assert_eq!(idx2.symbols().len(), original_count);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn context_index_cache_miss_when_no_file() {
        let dir =
            std::env::temp_dir().join(format!("kf-code-context-cache-miss-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        let cache_path = dir.join(".kf-code/context-index/cache.json");
        let result = ContextIndex::load(&cache_path);
        assert!(result.is_err(), "loading from nonexistent path should fail");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn context_index_cache_hit_when_head_matches() {
        let dir =
            std::env::temp_dir().join(format!("kf-code-context-cache-hit-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        let mut idx = ContextIndex::new();
        let src = dir.join("lib.rs");
        fs::write(&src, "fn test_fn() {}\n").unwrap();
        idx.index_file(&src, &fs::read_to_string(&src).unwrap())
            .unwrap();

        let cache_path = dir.join(".kf-code/context-index/cache.json");
        let head = "fake_head_sha_1234";
        idx.save(&cache_path, head).unwrap();

        let loaded = ContextIndex::load(&cache_path).unwrap();
        // is_current with a matching head string should return true
        // (we can't easily test against real git HEAD in a unit test,
        // but we can test the comparison logic directly)
        assert_eq!(loaded.head, head);
        assert!(!loaded.symbols.is_empty());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn context_index_cache_miss_when_head_differs() {
        let dir = std::env::temp_dir().join(format!(
            "kf-code-context-cache-head-diff-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        let mut idx = ContextIndex::new();
        let src = dir.join("lib.rs");
        fs::write(&src, "fn test_fn() {}\n").unwrap();
        idx.index_file(&src, &fs::read_to_string(&src).unwrap())
            .unwrap();

        let cache_path = dir.join(".kf-code/context-index/cache.json");
        idx.save(&cache_path, "old_head_sha").unwrap();

        let loaded = ContextIndex::load(&cache_path).unwrap();
        // Simulate a HEAD mismatch by checking against a different head
        let cached = CachedIndex {
            format_version: CURRENT_FORMAT_VERSION,
            head: "old_head_sha".to_string(),
            symbols: loaded.symbols,
            edges: loaded.edges,
            call_edges: loaded.call_edges,
            embeddings: loaded.embeddings,
            file_mtimes: loaded.file_mtimes,
        };
        // is_current checks real git HEAD, which won't match "old_head_sha"
        // in a temp dir (not a git repo) → returns false
        assert!(!ContextIndex::is_current(&cached, &dir));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn from_symbols_constructs_index() {
        let symbols = vec![Symbol {
            name: "foo".to_string(),
            kind: SymbolKind::Function,
            file: PathBuf::from("src/lib.rs"),
            line: 1,
            end_line: 5,
            doc: None,
        }];
        let idx = ContextIndex::from_symbols(symbols);
        assert_eq!(idx.symbols().len(), 1);
        assert_eq!(idx.symbols()[0].name, "foo");
    }

    #[test]
    fn doc_comments_extracted_from_rust_function() {
        let tmp =
            std::env::temp_dir().join(format!("kf-code-context-doc-rs-{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();

        let src = tmp.join("lib.rs");
        fs::write(
            &src,
            "/// Authenticates a user by token.\nfn auth(token: &str) -> bool {\n    true\n}\nstruct NoDoc;\n",
        )
        .unwrap();

        let mut idx = ContextIndex::new();
        idx.index_file(&src, &fs::read_to_string(&src).unwrap())
            .unwrap();

        let syms = idx.symbols();
        let auth = syms.iter().find(|s| s.name == "auth").unwrap();
        assert_eq!(
            auth.doc.as_deref(),
            Some("Authenticates a user by token."),
            "expected doc comment on auth, got {:?}",
            auth.doc
        );
        let no_doc = syms.iter().find(|s| s.name == "NoDoc").unwrap();
        assert!(no_doc.doc.is_none(), "NoDoc should have no doc comment");

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn block_doc_comments_extracted() {
        let tmp =
            std::env::temp_dir().join(format!("kf-code-context-doc-block-{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();

        let src = tmp.join("lib.rs");
        fs::write(
            &src,
            "/**\n * A configuration holder.\n * Stores all settings.\n */\nstruct Config {\n    x: i32,\n}\n",
        )
        .unwrap();

        let mut idx = ContextIndex::new();
        idx.index_file(&src, &fs::read_to_string(&src).unwrap())
            .unwrap();

        let syms = idx.symbols();
        let config = syms.iter().find(|s| s.name == "Config").unwrap();
        assert!(
            config
                .doc
                .as_ref()
                .is_some_and(|d| d.contains("configuration holder")),
            "expected block doc on Config, got {:?}",
            config.doc
        );

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn index_file_extracts_ts_function() {
        let tmp =
            std::env::temp_dir().join(format!("kf-code-context-ts-fn-{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();

        let src = tmp.join("app.ts");
        fs::write(&src, "function foo(a: number): string { return \"hi\"; }").unwrap();

        let mut idx = ContextIndex::new();
        idx.index_file(&src, &fs::read_to_string(&src).unwrap())
            .unwrap();

        let syms = idx.symbols();
        assert!(
            syms.iter()
                .any(|s| s.name == "foo" && s.kind == SymbolKind::Function),
            "expected foo as Function, got {syms:?}"
        );

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn index_file_extracts_ts_class() {
        let tmp =
            std::env::temp_dir().join(format!("kf-code-context-ts-class-{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();

        let src = tmp.join("app.ts");
        fs::write(&src, "class Bar { constructor() {} method() {} }").unwrap();

        let mut idx = ContextIndex::new();
        idx.index_file(&src, &fs::read_to_string(&src).unwrap())
            .unwrap();

        let syms = idx.symbols();
        assert!(
            syms.iter()
                .any(|s| s.name == "Bar" && s.kind == SymbolKind::Class),
            "expected Bar as Class, got {syms:?}"
        );

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn index_file_extracts_ts_interface() {
        let tmp =
            std::env::temp_dir().join(format!("kf-code-context-ts-iface-{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();

        let src = tmp.join("app.ts");
        fs::write(&src, "interface Baz { name: string; }").unwrap();

        let mut idx = ContextIndex::new();
        idx.index_file(&src, &fs::read_to_string(&src).unwrap())
            .unwrap();

        let syms = idx.symbols();
        assert!(
            syms.iter()
                .any(|s| s.name == "Baz" && s.kind == SymbolKind::Interface),
            "expected Baz as Interface, got {syms:?}"
        );

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn index_dir_walks_ts_files() {
        let tmp =
            std::env::temp_dir().join(format!("kf-code-context-dir-ts-{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();

        fs::write(tmp.join("a.rs"), "fn a() {}\nstruct A;\n").unwrap();
        fs::write(
            tmp.join("b.ts"),
            "function b() {}\ninterface IB { x: number; }\n",
        )
        .unwrap();
        // Non-indexable extension should be skipped
        fs::write(tmp.join("c.txt"), "not code").unwrap();

        let mut idx = ContextIndex::new();
        idx.index_dir(&tmp).unwrap();

        let syms = idx.symbols();
        let names: Vec<&str> = syms.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"a"), "expected a, got {names:?}");
        assert!(names.contains(&"A"), "expected A, got {names:?}");
        assert!(names.contains(&"b"), "expected b, got {names:?}");
        assert!(names.contains(&"IB"), "expected IB, got {names:?}");

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn detect_language_by_extension() {
        assert_eq!(
            detect_language(PathBuf::from("foo.rs").as_path()),
            Some(Language::Rust)
        );
        assert_eq!(
            detect_language(PathBuf::from("foo.ts").as_path()),
            Some(Language::TypeScript)
        );
        assert_eq!(
            detect_language(PathBuf::from("foo.tsx").as_path()),
            Some(Language::TypeScript)
        );
        assert_eq!(
            detect_language(PathBuf::from("foo.py").as_path()),
            Some(Language::Python)
        );
        assert_eq!(
            detect_language(PathBuf::from("foo.go").as_path()),
            Some(Language::Go)
        );
        assert_eq!(detect_language(PathBuf::from("foo").as_path()), None);
    }

    #[test]
    fn index_file_extracts_python_function() {
        let tmp =
            std::env::temp_dir().join(format!("kf-code-context-py-fn-{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();

        let src = tmp.join("app.py");
        fs::write(&src, "def foo(a: int) -> str:\n    return \"hi\"").unwrap();

        let mut idx = ContextIndex::new();
        idx.index_file(&src, &fs::read_to_string(&src).unwrap())
            .unwrap();

        let syms = idx.symbols();
        assert!(
            syms.iter()
                .any(|s| s.name == "foo" && s.kind == SymbolKind::Function),
            "expected foo as Function, got {syms:?}"
        );

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn index_file_extracts_python_class() {
        let tmp =
            std::env::temp_dir().join(format!("kf-code-context-py-class-{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();

        let src = tmp.join("app.py");
        fs::write(&src, "class Bar:\n    def method(self):\n        pass").unwrap();

        let mut idx = ContextIndex::new();
        idx.index_file(&src, &fs::read_to_string(&src).unwrap())
            .unwrap();

        let syms = idx.symbols();
        assert!(
            syms.iter()
                .any(|s| s.name == "Bar" && s.kind == SymbolKind::Class),
            "expected Bar as Class, got {syms:?}"
        );

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn index_dir_walks_py_files() {
        let tmp =
            std::env::temp_dir().join(format!("kf-code-context-dir-py-{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();

        fs::write(tmp.join("a.rs"), "fn a() {}\nstruct A;\n").unwrap();
        fs::write(
            tmp.join("b.ts"),
            "function b() {}\ninterface IB { x: number; }\n",
        )
        .unwrap();
        fs::write(tmp.join("c.py"), "def c(): pass\nclass C: pass\n").unwrap();

        let mut idx = ContextIndex::new();
        idx.index_dir(&tmp).unwrap();

        let syms = idx.symbols();
        let names: Vec<&str> = syms.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"a"), "expected a, got {names:?}");
        assert!(names.contains(&"A"), "expected A, got {names:?}");
        assert!(names.contains(&"b"), "expected b, got {names:?}");
        assert!(names.contains(&"IB"), "expected IB, got {names:?}");
        assert!(names.contains(&"c"), "expected c, got {names:?}");
        assert!(names.contains(&"C"), "expected C, got {names:?}");

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn index_file_extracts_go_function() {
        let tmp =
            std::env::temp_dir().join(format!("kf-code-context-go-fn-{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();

        let src = tmp.join("main.go");
        fs::write(
            &src,
            "package main\n\nfunc foo(a int) string {\n\treturn \"hi\"\n}",
        )
        .unwrap();

        let mut idx = ContextIndex::new();
        idx.index_file(&src, &fs::read_to_string(&src).unwrap())
            .unwrap();

        let syms = idx.symbols();
        assert!(
            syms.iter()
                .any(|s| s.name == "foo" && s.kind == SymbolKind::Function),
            "expected foo as Function, got {syms:?}"
        );

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn index_file_extracts_go_struct() {
        let tmp =
            std::env::temp_dir().join(format!("kf-code-context-go-struct-{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();

        let src = tmp.join("main.go");
        fs::write(&src, "package main\n\ntype Bar struct {\n\tX int\n}").unwrap();

        let mut idx = ContextIndex::new();
        idx.index_file(&src, &fs::read_to_string(&src).unwrap())
            .unwrap();

        let syms = idx.symbols();
        assert!(
            syms.iter()
                .any(|s| s.name == "Bar" && s.kind == SymbolKind::Struct),
            "expected Bar as Struct, got {syms:?}"
        );

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn index_file_extracts_go_method() {
        let tmp =
            std::env::temp_dir().join(format!("kf-code-context-go-method-{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();

        let src = tmp.join("main.go");
        fs::write(
            &src,
            "package main\n\ntype Bar struct { X int }\nfunc (b Bar) method() {}",
        )
        .unwrap();

        let mut idx = ContextIndex::new();
        idx.index_file(&src, &fs::read_to_string(&src).unwrap())
            .unwrap();

        let syms = idx.symbols();
        // WO 8.9: Go method receivers are now included in the symbol
        // name so the receiver type is preserved. `func (b Bar) method()`
        // is recorded as "Bar.method" instead of bare "method".
        assert!(
            syms.iter()
                .any(|s| s.name == "Bar.method" && s.kind == SymbolKind::Function),
            "expected `Bar.method` (with receiver type) as Function, got {syms:?}"
        );

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn index_dir_walks_go_files() {
        let tmp =
            std::env::temp_dir().join(format!("kf-code-context-dir-go-{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();

        fs::write(tmp.join("a.rs"), "fn a() {}\nstruct A;\n").unwrap();
        fs::write(
            tmp.join("b.ts"),
            "function b() {}\ninterface IB { x: number; }\n",
        )
        .unwrap();
        fs::write(tmp.join("c.py"), "def c(): pass\nclass C: pass\n").unwrap();
        fs::write(
            tmp.join("d.go"),
            "package main\n\nfunc d() {}\ntype D struct { x int }",
        )
        .unwrap();

        let mut idx = ContextIndex::new();
        idx.index_dir(&tmp).unwrap();

        let syms = idx.symbols();
        let names: Vec<&str> = syms.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"a"), "expected a, got {names:?}");
        assert!(names.contains(&"A"), "expected A, got {names:?}");
        assert!(names.contains(&"b"), "expected b, got {names:?}");
        assert!(names.contains(&"IB"), "expected IB, got {names:?}");
        assert!(names.contains(&"c"), "expected c, got {names:?}");
        assert!(names.contains(&"C"), "expected C, got {names:?}");
        assert!(names.contains(&"d"), "expected d, got {names:?}");
        assert!(names.contains(&"D"), "expected D, got {names:?}");

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn import_edge_rust_use_creates_edge() {
        let tmp =
            std::env::temp_dir().join(format!("kf-code-context-import-rs-{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();

        let src = tmp.join("lib.rs");
        fs::write(&src, "use std::collections::HashMap;\nfn foo() {}\n").unwrap();

        let mut idx = ContextIndex::new();
        idx.index_file(&src, &fs::read_to_string(&src).unwrap())
            .unwrap();

        let edges = idx.edges();
        assert!(
            edges
                .iter()
                .any(|e| e.imported_symbol.contains("std::collections")),
            "expected Rust use import edge, got {edges:?}"
        );
        assert!(
            edges.iter().any(
                |e| e.imported_symbol.contains("std::collections") && e.resolved_file.is_none()
            ),
            "external Rust use should have resolved_file=None, got {edges:?}"
        );

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn import_edge_ts_relative_import() {
        let tmp =
            std::env::temp_dir().join(format!("kf-code-context-import-ts-{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();

        let src = tmp.join("app.ts");
        fs::write(
            &src,
            "import { foo } from \"./utils\";\nfunction bar() {}\n",
        )
        .unwrap();
        let utils = tmp.join("utils.ts");
        fs::write(&utils, "function foo() {}\n").unwrap();

        let mut idx = ContextIndex::new();
        idx.index_dir(&tmp).unwrap();

        let edges = idx.edges();
        assert!(
            edges.iter().any(|e| e.imported_symbol == "./utils"),
            "expected TS import edge with specifier './utils', got {edges:?}"
        );

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn import_edge_python_from_import() {
        let tmp =
            std::env::temp_dir().join(format!("kf-code-context-import-py-{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();

        let src = tmp.join("main.py");
        fs::write(&src, "from foo import bar\n\ndef main(): pass\n").unwrap();

        let mut idx = ContextIndex::new();
        idx.index_file(&src, &fs::read_to_string(&src).unwrap())
            .unwrap();

        let edges = idx.edges();
        assert!(
            edges.iter().any(|e| e.imported_symbol == "foo"),
            "expected Python from-import edge with specifier 'foo', got {edges:?}"
        );

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn import_edge_unresolvable_stored_with_none() {
        let tmp = std::env::temp_dir().join(format!(
            "kf-code-context-import-none-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();

        let src = tmp.join("main.rs");
        fs::write(&src, "use serde::Deserialize;\nfn foo() {}\n").unwrap();

        let mut idx = ContextIndex::new();
        idx.index_file(&src, &fs::read_to_string(&src).unwrap())
            .unwrap();
        idx.resolve_imports(&tmp);

        let edges = idx.edges();
        assert!(
            edges
                .iter()
                .any(|e| e.imported_symbol.contains("serde") && e.resolved_file.is_none()),
            "expected unresolvable external import with resolved_file=None, got {edges:?}"
        );

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn retrieve_includes_importers() {
        let tmp = std::env::temp_dir().join(format!(
            "kf-code-context-retrieve-imp-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();

        let auth = tmp.join("auth.rs");
        fs::write(&auth, "fn auth() {}\n").unwrap();
        let main = tmp.join("main.rs");
        fs::write(&main, "use crate::auth;\nfn run() { auth(); }\n").unwrap();

        let mut idx = ContextIndex::new();
        idx.index_dir(&tmp).unwrap();

        let results = idx.retrieve("auth", 10);
        assert!(
            !results.is_empty(),
            "expected at least one result for 'auth'"
        );
        let auth_result = results.iter().find(|r| r.symbol.name == "auth");
        assert!(auth_result.is_some(), "expected 'auth' symbol in results");

        let _ = fs::remove_dir_all(&tmp);
    }

    /// WO 43.34: `retrieve()` must NOT smear unresolved import edges
    /// (resolved_file == None — stdlib/external imports) into every
    /// matched symbol's `imported_by`. Only edges that RESOLVED to the
    /// symbol's file count as importers. The old
    /// `is_none_or(rf == sym.file)` filter attributed every unresolved
    /// edge in the index to every result, re-inflating `imported_by`
    /// to near-whole-index size on a large repo (7MB / >1M tokens
    /// observed). `other.rs` has only an unresolved stdlib import and
    /// no relation to `auth.rs`; it must not appear in `auth`'s
    /// `imported_by`.
    #[test]
    fn retrieve_does_not_smear_unresolved_edges() {
        let tmp = std::env::temp_dir().join(format!(
            "kf-code-context-retrieve-smear-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&tmp);
        let src = tmp.join("src");
        fs::create_dir_all(&src).unwrap();

        // auth.rs lives at src/auth.rs so `use crate::auth;` resolves to it.
        let auth = src.join("auth.rs");
        fs::write(&auth, "fn auth() {}\n").unwrap();
        // main.rs imports auth (resolves to src/auth.rs) AND std::fs
        // (unresolved — resolved_file == None). Under the old smear,
        // the std::fs edge would also add main.rs to auth's imported_by.
        let main = src.join("main.rs");
        fs::write(
            &main,
            "use crate::auth;\nuse std::fs;\nfn run() { auth(); }\n",
        )
        .unwrap();
        // other.rs has only an unresolved stdlib import and no relation
        // to auth.rs. Under the old smear, its unresolved edge would
        // add other.rs to auth's imported_by — the regression.
        let other = src.join("other.rs");
        fs::write(&other, "use std::collections::HashMap;\nfn helper() {}\n").unwrap();

        let mut idx = ContextIndex::new();
        idx.index_dir(&tmp).unwrap();

        let results = idx.retrieve("auth", 10);
        let auth_result = results
            .iter()
            .find(|r| r.symbol.name == "auth")
            .expect("expected 'auth' symbol in results");

        // The resolved importer (main.rs via `use crate::auth;`) is present.
        let imported_by: Vec<String> = auth_result
            .imported_by
            .iter()
            .map(|p| p.to_string_lossy().to_string())
            .collect();
        assert!(
            imported_by.iter().any(|p| p.contains("main.rs")),
            "resolved importer main.rs must be in imported_by, got {imported_by:?}"
        );
        // The unrelated file with only an unresolved stdlib import must
        // NOT be smeared into auth's imported_by.
        assert!(
            !imported_by.iter().any(|p| p.contains("other.rs")),
            "unresolved edge from other.rs must NOT smear into auth's imported_by, got {imported_by:?}"
        );

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn call_edge_rust_function_call() {
        let tmp =
            std::env::temp_dir().join(format!("kf-code-context-call-rs-{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();

        let src = tmp.join("lib.rs");
        fs::write(&src, "fn foo() { bar(); }\nfn bar() {}\n").unwrap();

        let mut idx = ContextIndex::new();
        idx.index_file(&src, &fs::read_to_string(&src).unwrap())
            .unwrap();
        idx.resolve_call_edges();

        let call_edges = idx.call_edges();
        let foo_calls_bar = call_edges
            .iter()
            .find(|e| e.caller_name == "foo" && e.callee_name == "bar");
        assert!(
            foo_calls_bar.is_some(),
            "expected CallEdge foo→bar, got {call_edges:?}"
        );
        let edge = foo_calls_bar.unwrap();
        assert!(
            edge.callee_file.is_some(),
            "expected callee_file to be resolved for 'bar', got {:?}",
            edge.callee_file
        );

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn call_edge_ts_method_call() {
        let tmp =
            std::env::temp_dir().join(format!("kf-code-context-call-ts-{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();

        let src = tmp.join("app.ts");
        fs::write(&src, "function foo() { obj.bar(); }\n").unwrap();

        let mut idx = ContextIndex::new();
        idx.index_file(&src, &fs::read_to_string(&src).unwrap())
            .unwrap();

        let call_edges = idx.call_edges();
        assert!(
            call_edges.iter().any(|e| e.callee_name == "bar"),
            "expected CallEdge with callee=bar, got {call_edges:?}"
        );

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn call_edge_python_call() {
        let tmp =
            std::env::temp_dir().join(format!("kf-code-context-call-py-{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();

        let src = tmp.join("app.py");
        fs::write(&src, "def foo():\n    bar()\n").unwrap();

        let mut idx = ContextIndex::new();
        idx.index_file(&src, &fs::read_to_string(&src).unwrap())
            .unwrap();

        let call_edges = idx.call_edges();
        assert!(
            call_edges
                .iter()
                .any(|e| e.callee_name == "bar" && e.caller_name == "foo"),
            "expected CallEdge foo→bar, got {call_edges:?}"
        );

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn call_edge_unresolvable_callee() {
        let tmp = std::env::temp_dir().join(format!(
            "kf-code-context-call-unresolvable-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();

        let src = tmp.join("lib.rs");
        fs::write(&src, "fn foo() { external_lib(); }\n").unwrap();

        let mut idx = ContextIndex::new();
        idx.index_file(&src, &fs::read_to_string(&src).unwrap())
            .unwrap();
        idx.resolve_call_edges();

        let call_edges = idx.call_edges();
        let edge = call_edges.iter().find(|e| e.callee_name == "external_lib");
        assert!(
            edge.is_some(),
            "expected CallEdge to external_lib, got {call_edges:?}"
        );
        assert!(
            edge.unwrap().callee_file.is_none(),
            "expected callee_file=None for unresolvable callee, got {:?}",
            edge.unwrap().callee_file
        );

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn retrieve_includes_callers() {
        let tmp = std::env::temp_dir().join(format!(
            "kf-code-context-retrieve-callers-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();

        let auth = tmp.join("auth.rs");
        fs::write(&auth, "fn auth() {}\n").unwrap();
        let main = tmp.join("main.rs");
        fs::write(&main, "fn login() { auth(); }\n").unwrap();

        let mut idx = ContextIndex::new();
        idx.index_dir(&tmp).unwrap();

        let results = idx.retrieve("auth", 10);
        assert!(
            !results.is_empty(),
            "expected at least one result for 'auth'"
        );
        let auth_result = results.iter().find(|r| r.symbol.name == "auth");
        assert!(auth_result.is_some(), "expected 'auth' symbol in results");

        let called_by = &auth_result.unwrap().called_by;
        assert!(
            called_by.iter().any(|cs| cs.caller_name == "login"),
            "expected auth to be called by login, got {called_by:?}"
        );

        let _ = fs::remove_dir_all(&tmp);
    }

    /// WO 38.9 item 5: from_cached preserves embeddings, and
    /// retrieve_hybrid uses them instead of rebuilding. The result
    /// should be identical whether using cached embeddings or
    /// rebuilding from scratch.
    #[test]
    fn from_cached_preserves_embeddings_and_retrieve_uses_them() {
        let dir =
            std::env::temp_dir().join(format!("kf-code-context-cached-emb-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        let src = dir.join("lib.rs");
        fs::write(&src, "fn authenticate_user() {}\nstruct User {}").unwrap();

        let mut idx = ContextIndex::new();
        idx.index_file(&src, &fs::read_to_string(&src).unwrap())
            .unwrap();

        let cache_path = dir.join(".kf-code/context-index/cache.json");
        idx.save(&cache_path, "test_head").unwrap();

        let cached = ContextIndex::load(&cache_path).unwrap();
        assert!(
            !cached.embeddings.is_empty(),
            "saved cache should have embeddings"
        );

        let idx_from_cached = ContextIndex::from_cached(cached);
        assert!(
            !idx_from_cached.embeddings.is_empty(),
            "from_cached should preserve embeddings"
        );

        // retrieve_hybrid with cached embeddings should find the same
        // symbol as the substring fallback.
        let results = idx_from_cached.retrieve_hybrid("authenticate", 5);
        assert!(
            results.iter().any(|r| r.symbol.name == "authenticate_user"),
            "retrieve_hybrid with cached embeddings should find authenticate_user, got {results:?}"
        );

        let _ = fs::remove_dir_all(&dir);
    }

    /// WO 38.9 item 5: from_cached drops embeddings whose symbol_idx is
    /// out of bounds (stale cache with fewer symbols than embeddings).
    #[test]
    fn from_cached_drops_out_of_bounds_embeddings() {
        let cached = CachedIndex {
            format_version: CURRENT_FORMAT_VERSION,
            head: "test".to_string(),
            symbols: vec![Symbol {
                name: "foo".to_string(),
                kind: SymbolKind::Function,
                file: PathBuf::from("src/lib.rs"),
                line: 1,
                end_line: 1,
                doc: None,
            }],
            edges: vec![],
            call_edges: vec![],
            embeddings: vec![
                SymbolEmbedding {
                    symbol_idx: 0,
                    vector: vec![(0, 1.0)],
                },
                SymbolEmbedding {
                    symbol_idx: 5, // out of bounds
                    vector: vec![(0, 2.0)],
                },
            ],
            file_mtimes: std::collections::HashMap::new(),
        };
        let idx = ContextIndex::from_cached(cached);
        assert_eq!(idx.embeddings.len(), 1, "out-of-bounds embedding dropped");
        assert_eq!(idx.embeddings[0].symbol_idx, 0);
    }

    // ── WO 43.21: atomic write + format_version ──────────────────────────

    #[test]
    fn cache_atomic_write_partial_leaves_old_intact() {
        // save() writes to a temp file then renames. If the write dies after
        // creating the temp but before rename, the old cache.json is intact.
        let dir = std::env::temp_dir().join(format!(
            "kf-code-context-atomic-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        let mut idx = ContextIndex::new();
        let src = dir.join("lib.rs");
        fs::write(&src, "fn old_fn() {}\n").unwrap();
        idx.index_file(&src, &fs::read_to_string(&src).unwrap())
            .unwrap();

        let cache_path = dir.join(".kf-code/context-index/cache.json");
        idx.save(&cache_path, "head_v1").unwrap();
        let old_loaded = ContextIndex::load(&cache_path).unwrap();
        assert_eq!(old_loaded.head, "head_v1");

        // Simulate a crash mid-save: write a partial temp file but DON'T rename.
        let tmp = cache_path.with_extension("json.tmp");
        fs::write(&tmp, b"{\"truncated").unwrap();
        // The old cache.json must still be intact and loadable.
        let still_intact = ContextIndex::load(&cache_path).unwrap();
        assert_eq!(
            still_intact.head, "head_v1",
            "old cache must survive a partial temp write"
        );
        assert_eq!(still_intact.symbols.len(), old_loaded.symbols.len());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn cache_format_version_mismatch_triggers_rebuild() {
        // A cache with format_version 0 (pre-versioning) or a future version
        // must cause load() to return Err so the caller rebuilds.
        let dir = std::env::temp_dir().join(format!(
            "kf-code-context-fv-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        let cache_path = dir.join(".kf-code/context-index/cache.json");
        std::fs::create_dir_all(cache_path.parent().unwrap()).unwrap();
        // Write a cache with format_version = 0 (old format).
        let old_json = r#"{"format_version":0,"head":"x","symbols":[],"edges":[],"call_edges":[]}"#;
        fs::write(&cache_path, old_json).unwrap();
        let result = ContextIndex::load(&cache_path);
        assert!(
            result.is_err(),
            "format_version mismatch must return Err (rebuild signal)"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    /// WO 46.27: `mtime_rebuild` must preserve cached embeddings for
    /// symbols whose files did NOT change (WO 38.9 item 5 invariant).
    /// Before the fix, both rebuild paths constructed via
    /// `from_symbols_and_edges_and_calls`, which hard-codes
    /// `embeddings: Vec::new()` — silently dropping every cached
    /// embedding on every rebuild. This test asserts the surviving
    /// symbol's embedding is preserved AND re-indexed to its new
    /// position (the dropped file's symbols were ahead of it in the
    /// original ordering, so its `symbol_idx` must shift down).
    #[test]
    fn mtime_rebuild_preserves_surviving_embeddings() {
        let dir = std::env::temp_dir().join(format!(
            "kf-code-context-rebuild-emb-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        let a_path = dir.join("a.rs");
        let b_path = dir.join("b.rs");
        fs::write(&a_path, "fn alpha() {}\n").unwrap();
        fs::write(&b_path, "fn beta() {}\n").unwrap();

        let mut idx = ContextIndex::new();
        idx.index_file(&a_path, &fs::read_to_string(&a_path).unwrap())
            .unwrap();
        idx.index_file(&b_path, &fs::read_to_string(&b_path).unwrap())
            .unwrap();
        // Symbol order: alpha@0, beta@1.
        assert_eq!(idx.symbols.len(), 2);

        let cache_path = dir.join(".kf-code/context-index/cache.json");
        idx.save(&cache_path, "head_v1").unwrap();
        let cached = ContextIndex::load(&cache_path).unwrap();
        assert_eq!(cached.embeddings.len(), 2, "cache should have 2 embeddings");
        // beta's cached embedding points at index 1 (its original position).
        let beta_cached = cached
            .embeddings
            .iter()
            .find(|e| e.symbol_idx == 1)
            .expect("beta embedding at idx 1");
        let beta_vector = beta_cached.vector.clone();

        // Change a.rs content + mtime. b.rs untouched.
        fs::write(&a_path, "fn alpha_v2() {}\nfn new_func() {}\n").unwrap();
        let later = std::time::SystemTime::now() + std::time::Duration::from_secs(2);
        let file = std::fs::OpenOptions::new()
            .write(true)
            .open(&a_path)
            .unwrap();
        file.set_modified(later).unwrap();
        drop(file);

        let (rebuilt, changed) = ContextIndex::mtime_rebuild(cached, &dir);
        assert_eq!(changed, 1, "only a.rs changed");

        // beta survived and must keep its cached embedding, re-indexed
        // to its new position. After drop_changed, kept_symbols = [beta]
        // (new idx 0); index_file then appends alpha_v2 + new_func at 1, 2.
        let beta_new_idx = rebuilt
            .symbols
            .iter()
            .position(|s| s.name == "beta")
            .expect("beta must survive rebuild");
        let beta_emb = rebuilt
            .embeddings
            .iter()
            .find(|e| e.symbol_idx == beta_new_idx)
            .expect("beta must have a preserved embedding at its new idx");
        assert_eq!(
            beta_emb.vector, beta_vector,
            "beta's preserved embedding vector must match the cached one"
        );

        let _ = fs::remove_dir_all(&dir);
    }
}
