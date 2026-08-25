//! Tree-sitter parsing + symbol/import/call extraction.
//!
//! `index_file` is the entry point: it selects a tree-sitter grammar by
//! language, parses the source, then walks the AST to extract symbols
//! (`walk_tree`), import edges (`extract_import_edges`), and call-graph
//! edges (`extract_call_edges`). Per-language helpers handle the
//! non-conventional symbol name sources (TS arrow consts, Go method
//! receivers, Python `if __name__` guard).
//!
//! ponytail: Rust + TypeScript + Python + Go symbol extraction via tree-sitter.
//! Phase 6 complete. Import + call-graph edges for Rust/TS/Python/Go. The upgrade
//! path is embeddings/graph-walk retrieval (Phase 7).

use crate::{detect_language, Language, SymbolKind};
use crate::{CallEdge, ContextIndex, ImportEdge, Symbol};

impl ContextIndex {
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
                let specifier = Self::extract_import_specifiers(text, lang);
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
        let mut seen: std::collections::HashSet<(String, std::path::PathBuf)> =
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
    fn extract_import_specifiers(text: &str, lang: Language) -> String {
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
