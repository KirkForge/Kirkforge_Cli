use std::path::PathBuf;

/// Find a workflow file by name in the standard search paths.
///
/// Searches:
/// 1. `.kf-code/workflows/<name>.json` in the current directory.
/// 2. `~/.local/share/kf-code/workflows/<name>.json`.
pub fn find_workflow_file(name: &str) -> Option<PathBuf> {
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
