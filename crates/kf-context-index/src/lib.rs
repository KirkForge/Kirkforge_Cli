use std::path::PathBuf;

pub mod embeddings;
pub mod graph_walk;

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
/// HEAD, the cache is stale and must be rebuilt.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CachedIndex {
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

    /// Index a single source file using tree-sitter parsing.
    pub fn index_file(&mut self, path: &std::path::Path, content: &str) -> anyhow::Result<()> {
        let lang = detect_language(path)
            .ok_or_else(|| anyhow::anyhow!("unsupported file type: {}", path.display()))?;

        let mut parser = tree_sitter::Parser::new();
        match lang {
            Language::Rust => {
                parser
                    .set_language(&tree_sitter_rust::LANGUAGE.into())
                    .map_err(|e| anyhow::anyhow!("failed to set tree-sitter Rust language: {e}"))?;
            }
            Language::TypeScript => {
                parser
                    .set_language(&tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into())
                    .map_err(|e| {
                        anyhow::anyhow!("failed to set tree-sitter TypeScript language: {e}")
                    })?;
            }
            Language::Python => {
                parser
                    .set_language(&tree_sitter_python::LANGUAGE.into())
                    .map_err(|e| {
                        anyhow::anyhow!("failed to set tree-sitter Python language: {e}")
                    })?;
            }
            Language::Go => {
                parser
                    .set_language(&tree_sitter_go::LANGUAGE.into())
                    .map_err(|e| anyhow::anyhow!("failed to set tree-sitter Go language: {e}"))?;
            }
        }

        let tree = parser
            .parse(content, None)
            .ok_or_else(|| anyhow::anyhow!("tree-sitter failed to parse {}", path.display()))?;

        let root = tree.root_node();
        let mut cursor = root.walk();
        self.walk_tree(&mut cursor, content, path, lang);
        self.extract_import_edges(&root, content, path, lang);
        self.extract_call_edges(&root, content, path, lang);
        Ok(())
    }

    /// Extract import edges from the tree-sitter AST.
    fn extract_import_edges(
        &mut self,
        root: &tree_sitter::Node,
        source: &str,
        path: &std::path::Path,
        lang: Language,
    ) {
        let import_kinds: &[&str] = match lang {
            Language::Rust => &["use_declaration"],
            Language::TypeScript => &["import_statement"],
            Language::Python => &["import_statement", "import_from_statement"],
            Language::Go => &["import_declaration"],
        };

        let mut stack = vec![*root];
        while let Some(node) = stack.pop() {
            if import_kinds.contains(&node.kind()) {
                let text = node.utf8_text(source.as_bytes()).unwrap_or("");
                let specifier = Self::extract_import_specifier(text, lang);
                if !specifier.is_empty() {
                    let line = node.start_position().row as u32 + 1;
                    self.edges.push(ImportEdge {
                        source_file: path.to_path_buf(),
                        imported_symbol: specifier,
                        resolved_file: None,
                        line,
                    });
                }
            }
            let mut child_cursor = node.walk();
            for ch in child_cursor.node().children(&mut child_cursor) {
                stack.push(ch);
            }
        }
    }

    /// Extract call-graph edges from the tree-sitter AST.
    fn extract_call_edges(
        &mut self,
        root: &tree_sitter::Node,
        source: &str,
        path: &std::path::Path,
        lang: Language,
    ) {
        let call_kinds: &[&str] = match lang {
            Language::Rust => &["call_expression", "method_call_expression"],
            Language::TypeScript => &["call_expression"],
            Language::Python => &["call"],
            Language::Go => &["call_expression"],
        };

        let mut stack = vec![*root];
        while let Some(node) = stack.pop() {
            if call_kinds.contains(&node.kind()) {
                let callee_name = Self::extract_callee_name(&node, source, lang);
                if let Some(callee) = callee_name {
                    let caller_name = Self::find_enclosing_function(&node, source, lang)
                        .unwrap_or_else(|| "<top_level>".to_string());
                    let line = node.start_position().row as u32 + 1;
                    self.call_edges.push(CallEdge {
                        caller_file: path.to_path_buf(),
                        caller_name,
                        caller_line: line,
                        callee_name: callee,
                        callee_file: None,
                    });
                }
            }
            let mut child_cursor = node.walk();
            for ch in child_cursor.node().children(&mut child_cursor) {
                stack.push(ch);
            }
        }
    }

    /// Extract the callee name from a call expression node.
    fn extract_callee_name(
        node: &tree_sitter::Node,
        source: &str,
        lang: Language,
    ) -> Option<String> {
        match lang {
            Language::Rust => {
                // Rust: method_call_expression has a "method" field.
                // call_expression has a "function" field.
                if node.kind() == "method_call_expression" {
                    if let Some(method_node) = node.child_by_field_name("method") {
                        return Some(method_node.utf8_text(source.as_bytes()).ok()?.to_string());
                    }
                }
                // call_expression: "function" field
                if let Some(func_node) = node.child_by_field_name("function") {
                    let text = func_node.utf8_text(source.as_bytes()).ok()?.to_string();
                    return Some(Self::last_identifier(&text));
                }
                None
            }
            Language::TypeScript => {
                // call_expression: "function" field may be identifier or member_expression
                if let Some(func_node) = node.child_by_field_name("function") {
                    let text = func_node.utf8_text(source.as_bytes()).ok()?.to_string();
                    return Some(Self::last_identifier(&text));
                }
                None
            }
            Language::Python => {
                // call: "function" field may be identifier or attribute
                if let Some(func_node) = node.child_by_field_name("function") {
                    let text = func_node.utf8_text(source.as_bytes()).ok()?.to_string();
                    return Some(Self::last_identifier(&text));
                }
                None
            }
            Language::Go => {
                // call_expression: "function" field may be identifier or selector_expression
                if let Some(func_node) = node.child_by_field_name("function") {
                    if func_node.kind() == "selector_expression" {
                        // selector_expression: operand.field — extract the field
                        if let Some(field_node) = func_node.child_by_field_name("field") {
                            return Some(field_node.utf8_text(source.as_bytes()).ok()?.to_string());
                        }
                    }
                    let text = func_node.utf8_text(source.as_bytes()).ok()?.to_string();
                    return Some(Self::last_identifier(&text));
                }
                None
            }
        }
    }

    /// Extract the last identifier from a dotted expression like `obj.method`.
    fn last_identifier(text: &str) -> String {
        text.rsplit('.').next().unwrap_or(text).to_string()
    }

    /// Walk up the tree to find the enclosing function/method name.
    fn find_enclosing_function(
        node: &tree_sitter::Node,
        source: &str,
        lang: Language,
    ) -> Option<String> {
        let enclosing_kinds: &[&str] = match lang {
            Language::Rust => &["function_item"],
            Language::TypeScript => &[
                "function_declaration",
                "method_definition",
                "arrow_function",
            ],
            Language::Python => &["function_definition"],
            Language::Go => &["function_declaration", "method_declaration"],
        };

        let mut current = node.parent();
        while let Some(parent) = current {
            if enclosing_kinds.contains(&parent.kind()) {
                if let Some(name_node) = parent.child_by_field_name("name") {
                    return Some(name_node.utf8_text(source.as_bytes()).ok()?.to_string());
                }
                // arrow_function has no name field
                return Some("<anonymous>".to_string());
            }
            current = parent.parent();
        }
        None
    }

    /// Recursively walk the tree-sitter tree and extract declarations.
    fn walk_tree(
        &mut self,
        cursor: &mut tree_sitter::TreeCursor,
        source: &str,
        path: &std::path::Path,
        lang: Language,
    ) {
        loop {
            let node = cursor.node();
            let kind = node.kind();

            // Pre-pass: handle language-specific patterns whose symbol name
            // is not the conventional "name" field on the matched node:
            //   - TS: `export const foo = () => {}` — extract from the
            //     lexical_declaration's variable_declarator; the LHS
            //     identifier is the symbol name.
            //   - Python: `if __name__ == "__main__":` — skip the body.
            //   - Go: `func (s *Server) Start()` — prefix the receiver
            //     type to the method name.
            // Returns Some(symbol) to indicate "this node produced a
            // symbol, do not also fall through to the default match arm".
            let pre_extracted: Option<Symbol> = match lang {
                Language::TypeScript => Self::try_extract_ts_arrow(node, source, path),
                Language::Python => {
                    if Self::is_python_dunder_main_guard(node, source) {
                        // Skip the body of the `if __name__` guard entirely.
                        // The function/class definitions live at module
                        // level and are captured when we recurse past the
                        // if_statement. The body's expression_statements
                        // (function calls) are not symbols anyway.
                        if cursor.goto_next_sibling() {
                            continue;
                        } else {
                            break;
                        }
                    }
                    None
                }
                Language::Go => Self::try_extract_go_method(node, source, path),
                _ => None,
            };

            if let Some(sym) = pre_extracted {
                self.symbols.push(sym);
            } else {
                let (symbol_kind, name_node) = match lang {
                    Language::Rust => match kind {
                        "function_item" => (SymbolKind::Function, node.child_by_field_name("name")),
                        "struct_item" => (SymbolKind::Struct, node.child_by_field_name("name")),
                        "enum_item" => (SymbolKind::Enum, node.child_by_field_name("name")),
                        "impl_item" => {
                            let type_node = node.child_by_field_name("type");
                            (SymbolKind::Impl, type_node)
                        }
                        "mod_item" => {
                            if node.child_by_field_name("body").is_some() {
                                (SymbolKind::Module, node.child_by_field_name("name"))
                            } else {
                                (SymbolKind::Module, None)
                            }
                        }
                        "use_declaration" => (SymbolKind::Use, Some(node)),
                        _ => (SymbolKind::Function, None),
                    },
                    Language::TypeScript => match kind {
                        "function_declaration" => {
                            (SymbolKind::Function, node.child_by_field_name("name"))
                        }
                        "class_declaration" => {
                            (SymbolKind::Class, node.child_by_field_name("name"))
                        }
                        "interface_declaration" => {
                            (SymbolKind::Interface, node.child_by_field_name("name"))
                        }
                        "enum_declaration" => (SymbolKind::Enum, node.child_by_field_name("name")),
                        "type_alias_declaration" => {
                            (SymbolKind::TypeAlias, node.child_by_field_name("name"))
                        }
                        "import_statement" => (SymbolKind::Use, Some(node)),
                        _ => (SymbolKind::Function, None),
                    },
                    Language::Python => match kind {
                        "function_definition" => {
                            (SymbolKind::Function, node.child_by_field_name("name"))
                        }
                        "class_definition" => (SymbolKind::Class, node.child_by_field_name("name")),
                        "import_statement" => (SymbolKind::Use, Some(node)),
                        "import_from_statement" => (SymbolKind::Use, Some(node)),
                        "decorated_definition" => {
                            let mut child_kind = None;
                            let mut child_cursor = node.walk();
                            for ch in child_cursor.node().children(&mut child_cursor) {
                                match ch.kind() {
                                    "function_definition" => {
                                        child_kind = Some((
                                            SymbolKind::Function,
                                            ch.child_by_field_name("name"),
                                        ));
                                        break;
                                    }
                                    "class_definition" => {
                                        child_kind = Some((
                                            SymbolKind::Class,
                                            ch.child_by_field_name("name"),
                                        ));
                                        break;
                                    }
                                    _ => {}
                                }
                            }
                            match child_kind {
                                Some((sk, nn)) => (sk, nn),
                                None => (SymbolKind::Function, None),
                            }
                        }
                        _ => (SymbolKind::Function, None),
                    },
                    Language::Go => match kind {
                        "function_declaration" => {
                            (SymbolKind::Function, node.child_by_field_name("name"))
                        }
                        "method_declaration" => {
                            // The default arm without pre-extract is
                            // unreachable for method_declaration because
                            // try_extract_go_method always returns Some
                            // for that node. Defensive default here.
                            (SymbolKind::Function, node.child_by_field_name("name"))
                        }
                        "type_declaration" => {
                            let mut type_spec_kind = None;
                            let mut child_cursor = node.walk();
                            for ch in child_cursor.node().children(&mut child_cursor) {
                                if ch.kind() == "type_spec" {
                                    let name_node = ch.child_by_field_name("name");
                                    let value = ch.child_by_field_name("type");
                                    let value_kind = value.as_ref().map(|v| v.kind());
                                    let sym_kind = match value_kind {
                                        Some("struct_type") => SymbolKind::Struct,
                                        Some("interface_type") => SymbolKind::Interface,
                                        _ => SymbolKind::TypeAlias,
                                    };
                                    type_spec_kind = Some((sym_kind, name_node));
                                    break;
                                }
                            }
                            match type_spec_kind {
                                Some((sk, nn)) => (sk, nn),
                                None => (SymbolKind::Struct, None),
                            }
                        }
                        "import_declaration" => (SymbolKind::Use, Some(node)),
                        _ => (SymbolKind::Function, None),
                    },
                };

                if let Some(name_node) = name_node {
                    let is_named_function = kind == "function_item"
                        || kind == "function_declaration"
                        || kind == "function_definition"
                        || kind == "decorated_definition"
                        || kind == "method_declaration";
                    if symbol_kind != SymbolKind::Function || is_named_function {
                        let is_use_like = kind == "use_declaration"
                            || kind == "import_statement"
                            || kind == "import_from_statement"
                            || kind == "import_declaration";
                        let name = if is_use_like {
                            node.utf8_text(source.as_bytes()).unwrap_or("").to_string()
                        } else {
                            name_node
                                .utf8_text(source.as_bytes())
                                .unwrap_or("")
                                .to_string()
                        };
                        if !name.is_empty() {
                            let start_line = node.start_position().row as u32 + 1;
                            let end_line = node.end_position().row as u32 + 1;
                            let doc = Self::extract_doc_comment(source, start_line, lang);
                            self.symbols.push(Symbol {
                                name,
                                kind: symbol_kind,
                                file: path.to_path_buf(),
                                line: start_line,
                                end_line,
                                doc,
                            });
                        }
                    }
                }
            }

            let skip_children = kind == "decorated_definition";
            if !skip_children && cursor.goto_first_child() {
                self.walk_tree(cursor, source, path, lang);
                cursor.goto_parent();
            }

            if !cursor.goto_next_sibling() {
                break;
            }
        }
    }

    /// Detect a TypeScript `const foo = () => {}` (or `let` / `var`)
    /// pattern, optionally wrapped in `export ...`. Returns the LHS
    /// identifier as a Function symbol name. The TS grammar produces
    /// `lexical_declaration` for `const`/`let` and `variable_statement`
    /// for `var`; both contain a `variable_declarator` child.
    fn try_extract_ts_arrow(
        node: tree_sitter::Node,
        source: &str,
        path: &std::path::Path,
    ) -> Option<Symbol> {
        let kind = node.kind();
        if kind != "lexical_declaration" && kind != "variable_statement" {
            return None;
        }
        let mut decl_name: Option<String> = None;
        let mut is_arrow = false;
        let mut child_cursor = node.walk();
        for ch in child_cursor.node().children(&mut child_cursor) {
            if ch.kind() == "variable_declarator" {
                if let Some(name_node) = ch.child_by_field_name("name") {
                    decl_name = Some(
                        name_node
                            .utf8_text(source.as_bytes())
                            .unwrap_or("")
                            .to_string(),
                    );
                }
                if let Some(value_node) = ch.child_by_field_name("value") {
                    let value_kind = value_node.kind();
                    if value_kind == "arrow_function" || value_kind == "function_expression" {
                        is_arrow = true;
                    }
                }
            }
        }
        if !is_arrow {
            return None;
        }
        let name = decl_name?;
        if name.is_empty() {
            return None;
        }
        let start_line = node.start_position().row as u32 + 1;
        let end_line = node.end_position().row as u32 + 1;
        Some(Symbol {
            name,
            kind: SymbolKind::Function,
            file: path.to_path_buf(),
            line: start_line,
            end_line,
            doc: Self::extract_doc_comment(source, start_line, Language::TypeScript),
        })
    }

    /// True if `node` is an `if_statement` whose test is
    /// `__name__ == "__main__"` (the standard Python entry-point guard).
    /// When true, the walker's caller skips the body entirely; module-level
    /// definitions outside the guard are still captured when recursion
    /// walks past the if_statement.
    fn is_python_dunder_main_guard(node: tree_sitter::Node, source: &str) -> bool {
        if node.kind() != "if_statement" {
            return false;
        }
        // `if_statement` → first named child is the test.
        let Some(test) = node.child_by_field_name("condition") else {
            return false;
        };
        if test.kind() != "comparison_operator" {
            return false;
        }
        // comparison_operator: lhs <op> rhs — we want "__name__" on one
        // side and a string literal "__main__" on the other.
        let mut saw_dunder_name = false;
        let mut saw_main_string = false;
        let mut c = test.walk();
        for ch in test.children(&mut c) {
            if ch.kind() == "identifier" {
                if let Ok(text) = ch.utf8_text(source.as_bytes()) {
                    if text == "__name__" {
                        saw_dunder_name = true;
                    }
                }
            } else if ch.kind() == "string" || ch.kind() == "string_literal" {
                if let Ok(text) = ch.utf8_text(source.as_bytes()) {
                    // Strip surrounding quotes from a string literal.
                    let trimmed = text.trim_matches(|c| c == '"' || c == '\'');
                    if trimmed == "__main__" {
                        saw_main_string = true;
                    }
                }
            }
        }
        saw_dunder_name && saw_main_string
    }

    /// Detect a Go `func (s *Server) Start()` (or `func (r Server) Stop()`)
    /// and produce a `Server.Start` symbol whose name includes the receiver
    /// type. Pointer and value receivers are normalized to the base type
    /// (e.g. `*Server` → `Server`).
    fn try_extract_go_method(
        node: tree_sitter::Node,
        source: &str,
        path: &std::path::Path,
    ) -> Option<Symbol> {
        if node.kind() != "method_declaration" {
            return None;
        }
        let name_node = node.child_by_field_name("name")?;
        let method_name = name_node
            .utf8_text(source.as_bytes())
            .unwrap_or("")
            .to_string();
        if method_name.is_empty() {
            return None;
        }
        // The first parameter_list in a method_declaration is the receiver.
        let receiver_type = Self::find_go_receiver_type(&node, source);
        let final_name = match receiver_type {
            Some(t) if !t.is_empty() => format!("{t}.{method_name}"),
            _ => method_name,
        };
        let start_line = node.start_position().row as u32 + 1;
        let end_line = node.end_position().row as u32 + 1;
        Some(Symbol {
            name: final_name,
            kind: SymbolKind::Function,
            file: path.to_path_buf(),
            line: start_line,
            end_line,
            doc: Self::extract_doc_comment(source, start_line, Language::Go),
        })
    }

    /// Extract the receiver type name from a Go method_declaration.
    /// Returns "Server" for both `func (s *Server) M()` and
    /// `func (r Server) M()`.
    fn find_go_receiver_type(node: &tree_sitter::Node, source: &str) -> Option<String> {
        let mut c = node.walk();
        for ch in node.children(&mut c) {
            if ch.kind() == "parameter_list" {
                // First parameter_declaration inside the receiver's
                // parameter_list.
                let mut pc = ch.walk();
                for param in ch.children(&mut pc) {
                    if param.kind() == "parameter_declaration" {
                        // The type may be a type_identifier ("Server")
                        // or a pointer_type wrapping one ("*Server").
                        if let Some(type_node) = param.child_by_field_name("type") {
                            if type_node.kind() == "type_identifier" {
                                return Some(
                                    type_node
                                        .utf8_text(source.as_bytes())
                                        .unwrap_or("")
                                        .to_string(),
                                );
                            }
                            if type_node.kind() == "pointer_type" {
                                let mut pt_c = type_node.walk();
                                for inner in type_node.children(&mut pt_c) {
                                    if inner.kind() == "type_identifier" {
                                        return Some(
                                            inner
                                                .utf8_text(source.as_bytes())
                                                .unwrap_or("")
                                                .to_string(),
                                        );
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        None
    }

    /// Extract doc comments preceding `start_line` from `source`.
    fn extract_doc_comment(source: &str, start_line: u32, _lang: Language) -> Option<String> {
        let lines: Vec<&str> = source.lines().collect();
        if start_line < 2 {
            return None;
        }
        let mut doc_lines: Vec<String> = Vec::new();
        let mut i = (start_line as usize).saturating_sub(2);
        let mut found_doc = false;

        while let Some(line) = lines.get(i) {
            let trimmed = line.trim();
            if let Some(rest) = trimmed.strip_prefix("///") {
                doc_lines.push(rest.trim().to_string());
                found_doc = true;
            } else if let Some(rest) = trimmed.strip_prefix("/**") {
                let rest = rest.trim();
                if let Some(pos) = rest.find("*/") {
                    doc_lines.push(rest[..pos].trim().to_string());
                } else {
                    doc_lines.push(rest.to_string());
                }
                found_doc = true;
                break;
            } else if trimmed == "*/" {
                found_doc = true;
            } else if found_doc && trimmed.starts_with('*') {
                let c = trimmed[1..].trim();
                if !c.is_empty() {
                    doc_lines.push(c.to_string());
                }
            } else if found_doc && !trimmed.is_empty() && !trimmed.starts_with("*/") {
                break;
            } else if trimmed.starts_with("#")
                && trimmed.len() > 1
                && trimmed.chars().nth(1) == Some(' ')
            {
                let py_line = i + 1;
                if py_line < lines.len()
                    && (lines[py_line].trim().starts_with("def ")
                        || lines[py_line].trim().starts_with("class "))
                {
                    let c = trimmed[2..].trim();
                    doc_lines.push(c.to_string());
                    found_doc = true;
                } else {
                    break;
                }
            } else if !trimmed.is_empty() {
                break;
            }
            if i == 0 {
                break;
            }
            i -= 1;
        }
        if !found_doc {
            return None;
        }
        doc_lines.reverse();
        let joined = doc_lines.join(" ").trim().to_string();
        if joined.is_empty() {
            None
        } else {
            Some(joined)
        }
    }

    /// Remove duplicate `Interface` symbols that share the same
    /// `(name, file)` key. Used to deduplicate TypeScript interface
    /// merging, where the same interface name in one file produces
    /// multiple `interface_declaration` nodes by design.
    pub fn dedup_interfaces(&mut self) {
        let mut seen: std::collections::HashSet<(String, PathBuf)> =
            std::collections::HashSet::new();
        self.symbols.retain(|s| {
            if s.kind != SymbolKind::Interface {
                return true;
            }
            seen.insert((s.name.clone(), s.file.clone()))
        });
    }

    /// Extract the import specifier from an import statement's text.
    /// Returns the module path, not the full statement text.
    fn extract_import_specifier(text: &str, lang: Language) -> String {
        match lang {
            Language::Rust => {
                // `use crate::foo::bar;` or `use std::collections::HashMap;`
                let trimmed = text.trim().strip_prefix("use").unwrap_or(text).trim();
                let trimmed = trimmed.strip_suffix(';').unwrap_or(trimmed).trim();
                // Remove `{ ... }` grouped imports: `use crate::foo::{bar, baz}` → `crate::foo`
                if let Some(pos) = trimmed.find("::{") {
                    trimmed[..pos].to_string()
                } else {
                    trimmed.to_string()
                }
            }
            Language::TypeScript => {
                // `import { foo } from "./utils"` → `./utils`
                // `import "./utils"` → `./utils`
                // `import * as foo from "./utils"` → `./utils`
                // `import type { Foo } from "./utils"` → `./utils`
                let from_pos = text.rfind("from");
                if let Some(pos) = from_pos {
                    let after_from = &text[pos + 4..].trim();
                    extract_quoted_string(after_from).unwrap_or_else(|| after_from.to_string())
                } else {
                    // Side-effect import: `import "./styles.css"`
                    extract_quoted_string(text).unwrap_or_default()
                }
            }
            Language::Python => {
                // `import foo.bar` → `foo.bar`
                // `from foo.bar import baz` → `foo.bar`
                let trimmed = text.trim();
                if let Some(rest) = trimmed.strip_prefix("from") {
                    // `from foo.bar import baz` → `foo.bar`
                    let rest = rest.trim();
                    if let Some(pos) = rest.find("import") {
                        rest[..pos].trim().to_string()
                    } else {
                        rest.to_string()
                    }
                } else if let Some(rest) = trimmed.strip_prefix("import") {
                    // `import foo.bar` → `foo.bar`
                    rest.trim().to_string()
                } else {
                    trimmed.to_string()
                }
            }
            Language::Go => {
                // `import "fmt"` → `fmt`
                // `import ( "fmt"; "os" )` → first import only (handled per-node by tree-sitter)
                let trimmed = text.trim();
                if let Some(s) = extract_quoted_string(trimmed) {
                    s
                } else {
                    trimmed.to_string()
                }
            }
        }
    }

    /// Try to resolve an import specifier to a file path within the indexed project.
    pub fn resolve_imports(&mut self, root: &std::path::Path) {
        let edges = std::mem::take(&mut self.edges);
        for mut edge in edges {
            edge.resolved_file = resolve_import(&edge.imported_symbol, &edge.source_file, root);
            self.edges.push(edge);
        }
    }

    /// Resolve call edges: match callee_name to a known symbol's file.
    pub fn resolve_call_edges(&mut self) {
        let call_edges = std::mem::take(&mut self.call_edges);
        for mut edge in call_edges {
            edge.callee_file = self
                .symbols
                .iter()
                .find(|s| s.name == edge.callee_name)
                .map(|s| s.file.clone());
            self.call_edges.push(edge);
        }
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

    // ponytail: skip build/vendored/VCS dirs so a huge `target/` or `.git`
    // does not make the index-walk spin for minutes. The upgrade path is a
    // proper .gitignore-aware walker.
    fn is_ignored_dir(name: &std::ffi::OsStr) -> bool {
        matches!(
            name.to_str(),
            Some(
                "target"
                    | ".git"
                    | "node_modules"
                    | ".venv"
                    | "venv"
                    | "dist"
                    | "build"
                    | ".claude"
                    | ".opencode"
            )
        )
    }

    /// Index all `.rs`, `.ts`/`.tsx`, `.py`, and `.go` files under a directory.
    pub fn index_dir(&mut self, root: &std::path::Path) -> anyhow::Result<()> {
        for entry in walkdir::WalkDir::new(root)
            .into_iter()
            .filter_entry(|e| !Self::is_ignored_dir(e.file_name()))
            .filter_map(|e| e.ok())
        {
            let path = entry.path();
            let ext = path.extension().and_then(|s| s.to_str());
            let is_indexable = ext == Some("rs")
                || ext == Some("ts")
                || ext == Some("tsx")
                || ext == Some("py")
                || ext == Some("go");
            if is_indexable && path.is_file() {
                let content = std::fs::read_to_string(path)?;
                self.index_file(path, &content)?;
            }
        }
        // After indexing all files, resolve import edges and call edges.
        self.resolve_imports(root);
        self.resolve_call_edges();
        Ok(())
    }

    /// All extracted symbols.
    pub fn symbols(&self) -> &[Symbol] {
        &self.symbols
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
                    .filter(|e| e.resolved_file.as_ref().is_none_or(|rf| rf == &sym.file))
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

    /// Save the index to a JSON file, along with the current git HEAD
    /// and file modification times.
    pub fn save(&self, path: &std::path::Path, head: &str) -> anyhow::Result<()> {
        let embeddings = build_embeddings(self);
        let mut file_mtimes = std::collections::HashMap::new();
        for sym in &self.symbols {
            if let Ok(meta) = std::fs::metadata(&sym.file) {
                if let Ok(mtime) = meta.modified() {
                    let key = sym.file.to_string_lossy().to_string();
                    let ts = mtime
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs();
                    file_mtimes.entry(key).or_insert(ts);
                }
            }
        }
        let cached = CachedIndex {
            head: head.to_string(),
            symbols: self.symbols.clone(),
            edges: self.edges.clone(),
            call_edges: self.call_edges.clone(),
            embeddings,
            file_mtimes,
        };
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_string(&cached)?;
        std::fs::write(path, json)?;
        Ok(())
    }

    /// Load a cached index from a JSON file. Returns the cached index
    /// if the file exists and is valid JSON.
    pub fn load(path: &std::path::Path) -> anyhow::Result<CachedIndex> {
        let json = std::fs::read_to_string(path)?;
        let cached: CachedIndex = serde_json::from_str(&json)?;
        Ok(cached)
    }

    /// Check whether the cached index is current by comparing the
    /// stored git HEAD with the current HEAD in `repo_root`.
    pub fn is_current(cached: &CachedIndex, repo_root: &std::path::Path) -> bool {
        match current_head(repo_root) {
            Some(head) => head == cached.head,
            None => false,
        }
    }

    /// Incremental rebuild: diff changed files since the cached HEAD,
    /// remove stale symbols/edges for those files, and re-index only
    /// the changed files.
    pub fn incremental_rebuild(cached: CachedIndex, repo_root: &std::path::Path) -> (Self, usize) {
        let changed_files = git_diff_files(&cached.head, repo_root);
        let changed_count = changed_files.len();
        if changed_files.is_empty() {
            let idx = Self::from_cached(cached);
            return (idx, 0);
        }

        let changed_set: std::collections::HashSet<String> = changed_files
            .iter()
            .map(|p| p.to_string_lossy().to_string())
            .collect();

        let mut idx =
            Self::from_symbols_and_edges_and_calls(cached.symbols, cached.edges, cached.call_edges);

        idx.symbols
            .retain(|s| !changed_set.contains(&s.file.to_string_lossy().to_string()));
        idx.edges.retain(|e| {
            !changed_set.contains(&e.source_file.to_string_lossy().to_string())
                && e.resolved_file
                    .as_ref()
                    .is_none_or(|rf| !changed_set.contains(&rf.to_string_lossy().to_string()))
        });
        idx.call_edges.retain(|e| {
            !changed_set.contains(&e.caller_file.to_string_lossy().to_string())
                && e.callee_file
                    .as_ref()
                    .is_none_or(|cf| !changed_set.contains(&cf.to_string_lossy().to_string()))
        });

        for path in &changed_files {
            if path.is_file() {
                if let Ok(content) = std::fs::read_to_string(path) {
                    let _ = idx.index_file(path, &content);
                }
            }
        }

        idx.resolve_imports(repo_root);
        idx.resolve_call_edges();
        (idx, changed_count)
    }

    /// Mtime-based incremental rebuild: compares current file mtimes
    /// against the cached mtime map. Only re-indexes files whose mtime
    /// changed. Falls back to git-diff-based rebuild if no mtime data.
    ///
    /// Returns `(index, changed_file_count)`.
    pub fn mtime_rebuild(cached: CachedIndex, repo_root: &std::path::Path) -> (Self, usize) {
        if cached.file_mtimes.is_empty() {
            let changed = git_diff_files(&cached.head, repo_root);
            let count = changed.len();
            let idx = if changed.is_empty() {
                Self::from_cached(cached)
            } else {
                let (idx, c) = Self::incremental_rebuild(cached, repo_root);
                debug_assert_eq!(c, count);
                idx
            };
            return (idx, count);
        }

        let mut changed_paths: Vec<std::path::PathBuf> = Vec::new();
        for (file_str, cached_mtime) in &cached.file_mtimes {
            let path = std::path::PathBuf::from(file_str);
            if let Ok(meta) = std::fs::metadata(&path) {
                if let Ok(mtime) = meta.modified() {
                    let current_ts = mtime
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs();
                    if current_ts != *cached_mtime {
                        changed_paths.push(path);
                    }
                } else {
                    changed_paths.push(path);
                }
            } else {
                changed_paths.push(path);
            }
        }

        if changed_paths.is_empty() {
            return (Self::from_cached(cached), 0);
        }

        let changed_set: std::collections::HashSet<String> = changed_paths
            .iter()
            .map(|p| p.to_string_lossy().to_string())
            .collect();

        let mut idx =
            Self::from_symbols_and_edges_and_calls(cached.symbols, cached.edges, cached.call_edges);

        idx.symbols
            .retain(|s| !changed_set.contains(&s.file.to_string_lossy().to_string()));
        idx.edges.retain(|e| {
            !changed_set.contains(&e.source_file.to_string_lossy().to_string())
                && e.resolved_file
                    .as_ref()
                    .is_none_or(|rf| !changed_set.contains(&rf.to_string_lossy().to_string()))
        });
        idx.call_edges.retain(|e| {
            !changed_set.contains(&e.caller_file.to_string_lossy().to_string())
                && e.callee_file
                    .as_ref()
                    .is_none_or(|cf| !changed_set.contains(&cf.to_string_lossy().to_string()))
        });

        for path in &changed_paths {
            if path.is_file() {
                if let Ok(content) = std::fs::read_to_string(path) {
                    let _ = idx.index_file(path, &content);
                }
            }
        }

        idx.resolve_imports(repo_root);
        idx.resolve_call_edges();
        (idx, changed_paths.len())
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

/// Get the current git HEAD SHA for a repository root.
pub fn current_head(repo_root: &std::path::Path) -> Option<String> {
    std::process::Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(repo_root)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
}

/// Get files changed between `old_head` and current HEAD that are
/// indexable source files.
fn git_diff_files(old_head: &str, repo_root: &std::path::Path) -> Vec<std::path::PathBuf> {
    let output = std::process::Command::new("git")
        .args(["diff", "--name-only", old_head, "HEAD"])
        .current_dir(repo_root)
        .output();
    let output = match output {
        Ok(o) if o.status.success() => o,
        _ => return Vec::new(),
    };
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|l| !l.is_empty())
        .filter_map(|l| {
            let p = repo_root.join(l);
            if p.is_file() && detect_language(&p).is_some() {
                Some(p)
            } else {
                None
            }
        })
        .collect()
}

/// Extract a quoted string from text, handling both single and double quotes.
fn extract_quoted_string(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'"' || bytes[i] == b'\'' {
            let quote = bytes[i];
            let start = i + 1;
            let end = bytes[start..]
                .iter()
                .position(|&b| b == quote)
                .map(|p| start + p)?;
            return Some(String::from_utf8_lossy(&bytes[start..end]).to_string());
        }
        i += 1;
    }
    None
}

/// Try to resolve an import specifier to a file path within the project root.
fn resolve_import(
    specifier: &str,
    source_file: &std::path::Path,
    root: &std::path::Path,
) -> Option<PathBuf> {
    // Rust: `use crate::foo::bar` → `src/foo/bar.rs` or `src/foo/bar/mod.rs`
    if specifier.starts_with("crate::") {
        let module_path = specifier
            .strip_prefix("crate::")
            .unwrap()
            .replace("::", "/");
        let candidates = [
            root.join("src").join(format!("{module_path}.rs")),
            root.join("src").join(&module_path).join("mod.rs"),
        ];
        for candidate in candidates {
            if candidate.exists() {
                return Some(candidate);
            }
        }
        return None;
    }

    // Rust: `use std::...` etc — standard library, unresolvable locally
    if specifier.contains("::") && !specifier.starts_with('.') {
        // Bare module path like `std::collections`, `serde::Deserialize`
        return None;
    }

    // Relative imports (TS/JS): `./utils` → `./utils.ts` etc.
    if specifier.starts_with('.') {
        let source_dir = source_file.parent().unwrap_or(std::path::Path::new("."));
        let base = std::path::Path::new(specifier);
        let resolved = if base.is_absolute() {
            root.join(base.strip_prefix("/").unwrap_or(base))
        } else {
            source_dir.join(base)
        };

        let extensions = [".ts", ".tsx", ".js", ".jsx", ".mjs", ".mts"];
        for ext in extensions {
            let candidate = resolved.with_extension(ext.trim_start_matches('.'));
            if candidate.exists() {
                return Some(candidate);
            }
        }
        // Directory index resolution
        for index in ["index.ts", "index.tsx", "index.js"] {
            let candidate = resolved.join(index);
            if candidate.exists() {
                return Some(candidate);
            }
        }
        return None;
    }

    // Python: `from foo.bar import baz` or `import foo.bar`
    // Try `foo/bar.py` and `foo/bar/__init__.py`
    if !specifier.starts_with('.') && !specifier.contains('/') && !specifier.contains('\\') {
        let module_path = specifier.replace('.', std::path::MAIN_SEPARATOR_STR);
        let candidates = [
            root.join(format!("{module_path}.py")),
            root.join(&module_path).join("__init__.py"),
        ];
        for candidate in candidates {
            if candidate.exists() {
                return Some(candidate);
            }
        }
    }

    // Python relative: `from . import foo` or `from ..bar import baz`
    if specifier.starts_with('.') {
        let source_dir = source_file.parent().unwrap_or(std::path::Path::new("."));
        let mut dir = source_dir.to_path_buf();
        let stripped = specifier.trim_start_matches('.');
        let dot_count = specifier.len() - stripped.len();
        for _ in 0..dot_count.saturating_sub(1) {
            dir = dir
                .parent()
                .unwrap_or(std::path::Path::new("."))
                .to_path_buf();
        }
        if stripped.is_empty() {
            return None;
        }
        let module_path = stripped.replace('.', std::path::MAIN_SEPARATOR_STR);
        let candidates = [
            dir.join(format!("{module_path}.py")),
            dir.join(&module_path).join("__init__.py"),
        ];
        for candidate in candidates {
            if candidate.exists() {
                return Some(candidate);
            }
        }
        return None;
    }

    // Go: `"github.com/foo/bar"` — external package, unresolvable locally
    // Bare specifiers that don't match any of the above patterns
    None
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
}
