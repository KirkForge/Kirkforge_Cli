use std::path::PathBuf;

/// The kind of a source-code symbol.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SymbolKind {
    Function,
    Struct,
    Enum,
    Impl,
    Module,
    Use,
}

/// A single symbol extracted from source code.
#[derive(Debug, Clone)]
pub struct Symbol {
    pub name: String,
    pub kind: SymbolKind,
    pub file: PathBuf,
    pub line: u32,
    pub end_line: u32,
}

/// A tree-sitter-backed index of source-code symbols.
///
/// Currently a stub: symbols are extracted via simple line-based heuristics
/// (not tree-sitter yet). The real implementation will use tree-sitter
/// grammars for Rust/TS/Python/Go to build symbol, import, and call graphs.
///
/// ponytail: line-based heuristic extraction. The upgrade path is
/// tree-sitter grammar parsing. This stub exists to validate the API
/// shape and the `retrieve` contract before adding the tree-sitter dep.
pub struct ContextIndex {
    symbols: Vec<Symbol>,
}

impl ContextIndex {
    pub fn new() -> Self {
        Self {
            symbols: Vec::new(),
        }
    }

    /// Index a single file by scanning for declaration keywords.
    pub fn index_file(&mut self, path: &std::path::Path, content: &str) -> anyhow::Result<()> {
        for (i, line) in content.lines().enumerate() {
            let trimmed = line.trim();
            let line_no = (i + 1) as u32;

            if let Some(name) = Self::parse_decl(trimmed, "fn ") {
                self.symbols.push(Symbol {
                    name,
                    kind: SymbolKind::Function,
                    file: path.to_path_buf(),
                    line: line_no,
                    end_line: line_no,
                });
            } else if let Some(name) = Self::parse_decl(trimmed, "struct ") {
                self.symbols.push(Symbol {
                    name,
                    kind: SymbolKind::Struct,
                    file: path.to_path_buf(),
                    line: line_no,
                    end_line: line_no,
                });
            } else if let Some(name) = Self::parse_decl(trimmed, "enum ") {
                self.symbols.push(Symbol {
                    name,
                    kind: SymbolKind::Enum,
                    file: path.to_path_buf(),
                    line: line_no,
                    end_line: line_no,
                });
            } else if let Some(name) = Self::parse_decl(trimmed, "impl ") {
                self.symbols.push(Symbol {
                    name,
                    kind: SymbolKind::Impl,
                    file: path.to_path_buf(),
                    line: line_no,
                    end_line: line_no,
                });
            } else if let Some(name) = Self::parse_decl(trimmed, "mod ") {
                self.symbols.push(Symbol {
                    name,
                    kind: SymbolKind::Module,
                    file: path.to_path_buf(),
                    line: line_no,
                    end_line: line_no,
                });
            } else if let Some(name) = Self::parse_decl(trimmed, "use ") {
                self.symbols.push(Symbol {
                    name,
                    kind: SymbolKind::Use,
                    file: path.to_path_buf(),
                    line: line_no,
                    end_line: line_no,
                });
            }
        }
        Ok(())
    }

    /// Index all `.rs` files under a directory.
    pub fn index_dir(&mut self, root: &std::path::Path) -> anyhow::Result<()> {
        for entry in walkdir::WalkDir::new(root)
            .into_iter()
            .filter_map(|e| e.ok())
        {
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) == Some("rs") && path.is_file() {
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

    /// Parse a declaration like `fn foo(...)` or `struct Bar` from a trimmed line.
    fn parse_decl(trimmed: &str, keyword: &str) -> Option<String> {
        if !trimmed.starts_with(keyword) {
            return None;
        }
        let rest = &trimmed[keyword.len()..];
        // Take up to the first non-identifier character: (, <, {, ;, whitespace, etc.
        let name: String = rest
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_')
            .collect();
        if name.is_empty() {
            None
        } else {
            Some(name)
        }
    }
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
}
