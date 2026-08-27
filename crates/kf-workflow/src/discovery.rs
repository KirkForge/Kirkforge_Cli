use std::path::PathBuf;

/// Find a workflow file by name in the standard search paths.
///
/// Searches:
/// 1. `.kf-code/workflows/<name>.json` in the current directory.
/// 2. `~/.local/share/kf-code/workflows/<name>.json`.
pub fn find_workflow_file(name: &str) -> Option<PathBuf> {
    // template names come from model tool-call JSON; a separator would let an
    // absolute path discard the search base and '..' walk out of it (WO 47.17)
    if !is_bare_filename(name) {
        return None;
    }
    let local = PathBuf::from(".kf-code/workflows").join(format!("{name}.json"));
    if local.exists() {
        return Some(local);
    }
    if let Some(data_dir) = directories::BaseDirs::new() {
        let shared = data_dir
            .data_local_dir()
            .join("kf-code/workflows")
            .join(format!("{name}.json"));
        if shared.exists() {
            return Some(shared);
        }
    }
    None
}

/// Return the path to the user share directory for workflows.
pub fn user_workflow_dir() -> PathBuf {
    directories::BaseDirs::new()
        .map(|b| b.data_local_dir().join("kf-code/workflows"))
        .unwrap_or_else(|| PathBuf::from(".kf-code/workflows"))
}

// a template must be a single path component: no separators (which would make
// an absolute name discard the search base via join semantics), no parent refs
fn is_bare_filename(name: &str) -> bool {
    !name.contains('/') && !name.contains('\\') && name != ".." && name != "."
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_absolute_paths() {
        assert!(find_workflow_file("/tmp/evil").is_none());
        assert!(find_workflow_file("C:\\temp\\evil").is_none());
        assert!(!is_bare_filename("/tmp/evil"));
    }

    #[test]
    fn rejects_traversal() {
        assert!(find_workflow_file("../../etc/passwd").is_none());
        assert!(find_workflow_file("a/../b").is_none());
        assert!(find_workflow_file("..\\evil").is_none());
        assert!(find_workflow_file("..").is_none());
    }

    #[test]
    fn accepts_plain_names() {
        assert!(is_bare_filename("code-review"));
        assert!(is_bare_filename("my_template-2"));
        assert!(is_bare_filename("..suffix"));
    }
}
