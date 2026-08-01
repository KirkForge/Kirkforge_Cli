use std::path::{Path, PathBuf};

pub(super) fn find_cargo_root(path: &Path) -> Option<PathBuf> {
    let mut dir = path.parent()?;
    loop {
        if dir.join("Cargo.toml").exists() {
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
    use super::find_cargo_root;

    #[test]
    fn find_cargo_root_finds_immediate_parent_cargo_toml() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("Cargo.toml"), "").unwrap();
        let src = tmp.path().join("src/lib.rs");
        std::fs::create_dir_all(src.parent().unwrap()).unwrap();
        let found = find_cargo_root(&src).unwrap();
        assert_eq!(found, tmp.path());
    }

    #[test]
    fn find_cargo_root_walks_up_multiple_dirs() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("Cargo.toml"), "").unwrap();
        let deep = tmp.path().join("a/b/c/deep.rs");
        std::fs::create_dir_all(deep.parent().unwrap()).unwrap();
        let found = find_cargo_root(&deep).unwrap();
        assert_eq!(found, tmp.path());
    }

    #[test]
    fn find_cargo_root_returns_none_when_no_cargo_toml_in_ancestors() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("lonely.rs");
        assert!(find_cargo_root(&path).is_none());
    }
}
