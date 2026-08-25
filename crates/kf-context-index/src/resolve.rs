//! Import + call-edge resolution and directory walking.
//!
//! After tree-sitter extraction populates raw edges (with
//! `resolved_file: None` / `callee_file: None`), `resolve_imports`
//! and `resolve_call_edges` fill in the target files by matching the
//! specifier/callee name against on-disk paths and known symbols.
//! `index_dir` walks a directory, indexes every indexable file, then
//! runs both resolvers.
//!
//! ponytail: module-level attribution (file, not symbol); a symbol-level
//! edge model is the upgrade path.

use crate::{ContextIndex, PathBuf};

impl ContextIndex {
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
