//! Language detection — sniff marker files to identify the project language(s).
//!
//! Each verifier (Rust or Python) self-gates on the result of
//! [`detect_project_languages`] so a mixed-language workspace runs only the
//! verifiers relevant to the edited file. A project can be multi-language
//! (e.g. a Cargo crate with a `pyproject.toml` for tooling), so the function
//! returns a `Vec`, not a single value.

use std::path::{Path, PathBuf};

/// A programming language detected from a workspace marker file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ProjectLanguage {
    Rust,
    Python,
    Node,
    Go,
}

/// Marker files that imply a language is present at the workspace root.
const RUST_MARKERS: &[&str] = &["Cargo.toml"];
const PYTHON_MARKERS: &[&str] = &["pyproject.toml", "setup.py", "conftest.py"];
const NODE_MARKERS: &[&str] = &["package.json"];
const GO_MARKERS: &[&str] = &["go.mod"];

fn has_any_marker(workspace: &Path, markers: &[&str]) -> bool {
    markers.iter().any(|m| workspace.join(m).is_file())
}

/// Detect every language rooted at `workspace`.
///
/// Scans the immediate `workspace` directory for the canonical marker files.
/// Returns the languages in a stable order (Rust, Python, Node, Go) so caller
/// assertions are deterministic.
pub fn detect_project_languages(workspace: &Path) -> Vec<ProjectLanguage> {
    let mut out = Vec::new();
    if has_any_marker(workspace, RUST_MARKERS) {
        out.push(ProjectLanguage::Rust);
    }
    if has_any_marker(workspace, PYTHON_MARKERS) {
        out.push(ProjectLanguage::Python);
    }
    if has_any_marker(workspace, NODE_MARKERS) {
        out.push(ProjectLanguage::Node);
    }
    if has_any_marker(workspace, GO_MARKERS) {
        out.push(ProjectLanguage::Go);
    }
    out
}

/// Walk up from `path` to the nearest ancestor (inclusive) that looks like a
/// Python project root. Mirrors [`super::helpers::find_cargo_root`] for the
/// Python side.
pub fn find_python_root(path: &Path) -> Option<PathBuf> {
    find_root_with_markers(path, PYTHON_MARKERS)
}

/// Walk up from `path` to the nearest Node project root (has `package.json`).
/// Mirrors [`find_python_root`].
pub fn find_node_root(path: &Path) -> Option<PathBuf> {
    find_root_with_markers(path, NODE_MARKERS)
}

/// Walk up from `path` to the nearest Go project root (has `go.mod`).
/// Mirrors [`find_python_root`].
pub fn find_go_root(path: &Path) -> Option<PathBuf> {
    find_root_with_markers(path, GO_MARKERS)
}

/// Shared walk-up used by every `find_<lang>_root`. Returns the nearest
/// ancestor (inclusive) of `path` containing any of `markers`.
fn find_root_with_markers(path: &Path, markers: &[&str]) -> Option<PathBuf> {
    let mut dir = path.parent()?;
    loop {
        if has_any_marker(dir, markers) {
            return Some(dir.to_path_buf());
        }
        match dir.parent() {
            Some(parent) => dir = parent,
            None => return None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_rust_from_cargo_toml() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("Cargo.toml"), "").unwrap();
        let langs = detect_project_languages(tmp.path());
        assert_eq!(langs, vec![ProjectLanguage::Rust]);
    }

    #[test]
    fn detects_python_from_any_marker() {
        for marker in PYTHON_MARKERS {
            let tmp = tempfile::tempdir().unwrap();
            std::fs::write(tmp.path().join(marker), "").unwrap();
            let langs = detect_project_languages(tmp.path());
            assert_eq!(
                langs,
                vec![ProjectLanguage::Python],
                "marker {marker} should detect Python"
            );
        }
    }

    #[test]
    fn detects_node_from_package_json() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("package.json"), "{}").unwrap();
        let langs = detect_project_languages(tmp.path());
        assert_eq!(langs, vec![ProjectLanguage::Node]);
    }

    #[test]
    fn detects_go_from_go_mod() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("go.mod"), "module x\n").unwrap();
        let langs = detect_project_languages(tmp.path());
        assert_eq!(langs, vec![ProjectLanguage::Go]);
    }

    #[test]
    fn detects_multiple_languages_in_order() {
        // ponytail: stable ordering (Rust, Python, Node, Go) keeps caller
        // assertions deterministic — multi-language detection is real
        // (a Cargo crate with a pyproject.toml for tooling).
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("Cargo.toml"), "").unwrap();
        std::fs::write(tmp.path().join("pyproject.toml"), "").unwrap();
        std::fs::write(tmp.path().join("go.mod"), "").unwrap();
        let langs = detect_project_languages(tmp.path());
        assert_eq!(
            langs,
            vec![
                ProjectLanguage::Rust,
                ProjectLanguage::Python,
                ProjectLanguage::Go
            ]
        );
    }

    #[test]
    fn returns_empty_when_no_markers() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(detect_project_languages(tmp.path()).is_empty());
    }

    #[test]
    fn ignores_directories_with_marker_names() {
        // A directory named `Cargo.toml` must not count as a marker.
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir(tmp.path().join("Cargo.toml")).unwrap();
        assert!(detect_project_languages(tmp.path()).is_empty());
    }

    #[test]
    fn find_python_root_walks_up_to_nearest_marker() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("pyproject.toml"), "").unwrap();
        let deep = tmp.path().join("src/pkg/mod.py");
        std::fs::create_dir_all(deep.parent().unwrap()).unwrap();
        assert_eq!(find_python_root(&deep), Some(tmp.path().to_path_buf()));
    }

    #[test]
    fn find_python_root_returns_none_without_marker() {
        let tmp = tempfile::tempdir().unwrap();
        let f = tmp.path().join("orphan.py");
        assert!(find_python_root(&f).is_none());
    }

    #[test]
    fn find_python_root_picks_pyproject_over_parent_setup() {
        // Inner pyproject wins; outer setup.py is not the answer.
        let outer = tempfile::tempdir().unwrap();
        std::fs::write(outer.path().join("setup.py"), "").unwrap();
        let inner = outer.path().join("sub");
        std::fs::create_dir_all(&inner).unwrap();
        std::fs::write(inner.join("pyproject.toml"), "").unwrap();
        let f = inner.join("mod.py");
        assert_eq!(find_python_root(&f), Some(inner.to_path_buf()));
    }

    #[test]
    fn find_node_root_walks_up_to_package_json() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("package.json"), "{}").unwrap();
        let deep = tmp.path().join("src/lib/index.ts");
        std::fs::create_dir_all(deep.parent().unwrap()).unwrap();
        assert_eq!(find_node_root(&deep), Some(tmp.path().to_path_buf()));
    }

    #[test]
    fn find_node_root_returns_none_without_marker() {
        let tmp = tempfile::tempdir().unwrap();
        let f = tmp.path().join("orphan.ts");
        assert!(find_node_root(&f).is_none());
    }

    #[test]
    fn find_go_root_walks_up_to_go_mod() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("go.mod"), "module x\n").unwrap();
        let deep = tmp.path().join("pkg/foo/foo.go");
        std::fs::create_dir_all(deep.parent().unwrap()).unwrap();
        assert_eq!(find_go_root(&deep), Some(tmp.path().to_path_buf()));
    }

    #[test]
    fn find_go_root_returns_none_without_marker() {
        let tmp = tempfile::tempdir().unwrap();
        let f = tmp.path().join("orphan.go");
        assert!(find_go_root(&f).is_none());
    }
}
