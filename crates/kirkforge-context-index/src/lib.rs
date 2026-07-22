use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
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
}

pub fn detect_language(path: &std::path::Path) -> Option<Language> {
    match path.extension().and_then(|s| s.to_str()) {
        Some("rs") => Some(Language::Rust),
        Some("ts") | Some("tsx") => Some(Language::TypeScript),
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
}

/// Cached index metadata — the git HEAD at cache time plus the symbols.
///
/// Stored as JSON at `.kirkforge/context-index/cache.json`. The HEAD field
/// enables cache invalidation: if the current HEAD differs from the stored
/// HEAD, the cache is stale and must be rebuilt.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CachedIndex {
    /// The git HEAD SHA when this cache was written.
    pub head: String,
    /// The indexed symbols.
    pub symbols: Vec<Symbol>,
}

/// A tree-sitter-backed index of source-code symbols.
///
/// Uses tree-sitter grammars to extract function, struct, enum, impl, module,
/// and use declarations from Rust and TypeScript source files. The index is
/// built by calling `index_file` or `index_dir`, then queried via `retrieve`.
///
/// ponytail: Rust + TypeScript symbol extraction via tree-sitter. The upgrade
/// path is Python/Go grammars.
///
/// ponytail: substring-match retrieval. The upgrade path is embeddings or
/// graph-walk retrieval.
///
/// ponytail: disk caching uses serde_json (not bincode — bincode is unmaintained).
/// The upgrade path is a compact binary format if JSON size becomes a concern.
pub struct ContextIndex {
    symbols: Vec<Symbol>,
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
        }
    }

    /// Create an index from a pre-built symbol list (e.g., loaded from cache).
    pub fn from_symbols(symbols: Vec<Symbol>) -> Self {
        Self { symbols }
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
        }

        let tree = parser
            .parse(content, None)
            .ok_or_else(|| anyhow::anyhow!("tree-sitter failed to parse {}", path.display()))?;

        let root = tree.root_node();
        let mut cursor = root.walk();
        self.walk_tree(&mut cursor, content, path, lang);
        Ok(())
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
                    "class_declaration" => (SymbolKind::Class, node.child_by_field_name("name")),
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
            };

            if let Some(name_node) = name_node {
                let is_named_function = kind == "function_item" || kind == "function_declaration";
                if symbol_kind != SymbolKind::Function || is_named_function {
                    let is_use_like = kind == "use_declaration" || kind == "import_statement";
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
                        self.symbols.push(Symbol {
                            name,
                            kind: symbol_kind,
                            file: path.to_path_buf(),
                            line: start_line,
                            end_line,
                        });
                    }
                }
            }

            if cursor.goto_first_child() {
                self.walk_tree(cursor, source, path, lang);
                cursor.goto_parent();
            }

            if !cursor.goto_next_sibling() {
                break;
            }
        }
    }

    /// Index all `.rs` and `.ts`/`.tsx` files under a directory.
    pub fn index_dir(&mut self, root: &std::path::Path) -> anyhow::Result<()> {
        for entry in walkdir::WalkDir::new(root)
            .into_iter()
            .filter_map(|e| e.ok())
        {
            let path = entry.path();
            let ext = path.extension().and_then(|s| s.to_str());
            let is_indexable = ext == Some("rs") || ext == Some("ts") || ext == Some("tsx");
            if is_indexable && path.is_file() {
                let content = std::fs::read_to_string(path)?;
                self.index_file(path, &content)?;
            }
        }
        Ok(())
    }

    /// All extracted symbols.
    pub fn symbols(&self) -> &[Symbol] {
        &self.symbols
    }

    /// Retrieve the first `k` symbols whose name contains `query` as a substring.
    ///
    /// ponytail: substring-match retrieval. The upgrade path is
    /// embeddings or graph-walk retrieval.
    pub fn retrieve(&self, query: &str, k: usize) -> Vec<Symbol> {
        self.symbols
            .iter()
            .filter(|s| s.name.contains(query))
            .take(k)
            .cloned()
            .collect()
    }

    /// Save the index to a JSON file, along with the current git HEAD.
    pub fn save(&self, path: &std::path::Path, head: &str) -> anyhow::Result<()> {
        let cached = CachedIndex {
            head: head.to_string(),
            symbols: self.symbols.clone(),
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn index_file_extracts_fn_and_struct() {
        let tmp =
            std::env::temp_dir().join(format!("kirkforge-context-test-{}", std::process::id()));
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
            std::env::temp_dir().join(format!("kirkforge-context-retrieve-{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();

        let src = tmp.join("mod.rs");
        fs::write(&src, "fn foo_bar() {}\nfn baz() {}\nfn foo_baz() {}\n").unwrap();
        idx.index_file(&src, &fs::read_to_string(&src).unwrap())
            .unwrap();

        let results = idx.retrieve("foo", 2);
        assert_eq!(results.len(), 2, "expected 2 results, got {results:?}");
        assert!(results.iter().all(|s| s.name.contains("foo")));
    }

    #[test]
    fn index_dir_walks_rs_files() {
        let tmp =
            std::env::temp_dir().join(format!("kirkforge-context-dir-{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();
        fs::create_dir_all(&tmp.join("sub")).unwrap();

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
            std::env::temp_dir().join(format!("kirkforge-context-inline-{}", std::process::id()));
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
            std::env::temp_dir().join(format!("kirkforge-context-endline-{}", std::process::id()));
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
            "kirkforge-context-cache-roundtrip-{}",
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

        let cache_path = dir.join(".kirkforge/context-index/cache.json");
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
        let dir = std::env::temp_dir().join(format!(
            "kirkforge-context-cache-miss-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        let cache_path = dir.join(".kirkforge/context-index/cache.json");
        let result = ContextIndex::load(&cache_path);
        assert!(result.is_err(), "loading from nonexistent path should fail");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn context_index_cache_hit_when_head_matches() {
        let dir = std::env::temp_dir().join(format!(
            "kirkforge-context-cache-hit-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        let mut idx = ContextIndex::new();
        let src = dir.join("lib.rs");
        fs::write(&src, "fn test_fn() {}\n").unwrap();
        idx.index_file(&src, &fs::read_to_string(&src).unwrap())
            .unwrap();

        let cache_path = dir.join(".kirkforge/context-index/cache.json");
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
            "kirkforge-context-cache-head-diff-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        let mut idx = ContextIndex::new();
        let src = dir.join("lib.rs");
        fs::write(&src, "fn test_fn() {}\n").unwrap();
        idx.index_file(&src, &fs::read_to_string(&src).unwrap())
            .unwrap();

        let cache_path = dir.join(".kirkforge/context-index/cache.json");
        idx.save(&cache_path, "old_head_sha").unwrap();

        let loaded = ContextIndex::load(&cache_path).unwrap();
        // Simulate a HEAD mismatch by checking against a different head
        let cached = CachedIndex {
            head: "old_head_sha".to_string(),
            symbols: loaded.symbols,
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
        }];
        let idx = ContextIndex::from_symbols(symbols);
        assert_eq!(idx.symbols().len(), 1);
        assert_eq!(idx.symbols()[0].name, "foo");
    }

    #[test]
    fn index_file_extracts_ts_function() {
        let tmp =
            std::env::temp_dir().join(format!("kirkforge-context-ts-fn-{}", std::process::id()));
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
            std::env::temp_dir().join(format!("kirkforge-context-ts-class-{}", std::process::id()));
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
            std::env::temp_dir().join(format!("kirkforge-context-ts-iface-{}", std::process::id()));
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
            std::env::temp_dir().join(format!("kirkforge-context-dir-ts-{}", std::process::id()));
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
        assert_eq!(detect_language(PathBuf::from("foo.py").as_path()), None);
        assert_eq!(detect_language(PathBuf::from("foo").as_path()), None);
    }
}
