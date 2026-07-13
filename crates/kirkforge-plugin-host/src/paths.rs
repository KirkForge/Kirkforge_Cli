//! Path validation helpers for plugin command resolution.
//!
//! Plugin manifests declare the on-disk command (script or binary) that the
//! host must invoke for a tool, hook, or verifier. The host must ensure that
//! the resolved path stays inside the plugin's own directory so a malformed
//! or malicious manifest cannot turn a plugin load into an arbitrary system
//! command execution.

use std::path::{Component, Path, PathBuf};

/// True if `command` is a relative path that resolves to a location inside
/// `root` without using parent-directory escapes.
///
/// Absolute paths and paths containing `..` components that would climb above
/// the plugin root are rejected. The check is lexical: it does not require the
/// target file to exist, so it also catches missing tool scripts early.
pub fn is_command_within_root(root: &Path, command: &Path) -> bool {
    if command.as_os_str().is_empty() {
        return false;
    }

    let mut normalized = PathBuf::new();
    for component in command.components() {
        match component {
            Component::Normal(name) => normalized.push(name),
            Component::CurDir => {}
            Component::ParentDir => {
                if !normalized.pop() {
                    return false;
                }
            }
            // Reject absolute paths and Windows prefix components.
            Component::RootDir | Component::Prefix(_) => return false,
        }
    }

    // After normalization a path like "foo/.." becomes empty and is still
    // inside the root, but it is not a usable command.
    if normalized.as_os_str().is_empty() {
        return false;
    }

    root.join(&normalized).starts_with(root)
}

/// Extract the command path declared by a capability, if it has one.
pub fn capability_command(cap: &kirkforge_plugin::Capability) -> Option<&Path> {
    use kirkforge_plugin::Capability;
    match cap {
        Capability::Tool { command, .. } => command.as_deref(),
        Capability::Hook { command, .. } => Some(command.as_path()),
        Capability::Verifier { command, .. } => command.as_deref(),
        Capability::Skill { .. } => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn relative_child_is_within_root() {
        let root = PathBuf::from("/plugins/demo");
        assert!(is_command_within_root(&root, Path::new("tools/run.sh")));
        assert!(is_command_within_root(&root, Path::new("./tools/run.sh")));
    }

    #[test]
    fn absolute_path_is_outside_root() {
        let root = PathBuf::from("/plugins/demo");
        assert!(!is_command_within_root(&root, Path::new("/bin/sh")));
    }

    #[test]
    fn parent_escape_is_outside_root() {
        let root = PathBuf::from("/plugins/demo");
        assert!(!is_command_within_root(&root, Path::new("../evil.sh")));
        assert!(!is_command_within_root(
            &root,
            Path::new("tools/../../evil.sh")
        ));
    }

    #[test]
    fn empty_or_dot_only_command_is_rejected() {
        let root = PathBuf::from("/plugins/demo");
        assert!(!is_command_within_root(&root, Path::new("")));
        assert!(!is_command_within_root(&root, Path::new(".")));
    }
}
