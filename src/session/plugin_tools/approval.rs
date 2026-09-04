//! Plugin content-hash consent ledger (WO 43.17).
//!
//! Mirrors `mcp_project.rs` (WO 42.5) 1:1: an `approved_plugins.json`
//! ledger in the data dir stores `{ name, root, content_hash }` per
//! approved plugin. The hash is sha256 over `kf-code.toml` plus every
//! file the manifest declares as a capability (tool/hook/verifier
//! commands + skill files). A mismatch (script edited after approval)
//! re-gates: the plugin is skipped with a warning naming
//! `/plugins approve <name>`.
//!
//! Both gates are layered (WO 45.61): a signature-verified plugin still
//! passes through the ledger. The manifest-only signature does not cover
//! the command scripts the manifest points to; the bundle_hash does.
//! A signed plugin must ALSO be ledger-approved with a matching hash.

use kf_plugin_host::sdk::{Capability, PluginManifest};
use std::path::{Path, PathBuf};

/// Path of the persisted plugin approval ledger inside the data dir.
pub fn approval_db_path() -> Option<PathBuf> {
    crate::session::data_dir()
        .ok()
        .map(|d| d.join("approved_plugins.json"))
}

/// One approved plugin: its name, root path, and the content hash of the
/// bundle at approval time. `None` means the entry predates the hash
/// (legacy) — treated as "needs re-approval" (safe default), matching
/// `mcp_project::ApprovedProject`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ApprovedPlugin {
    pub name: String,
    pub root: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_hash: Option<String>,
}

/// Load the approved-plugin ledger. Absent file → empty vec. Lenient of
/// both the new (`Vec<ApprovedPlugin>`) and legacy formats (bare names or
/// paths → re-approval), matching `load_approved_projects`.
pub fn load_approved_plugins() -> Vec<ApprovedPlugin> {
    let path = match approval_db_path() {
        Some(p) => p,
        None => return Vec::new(),
    };
    let content = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };
    if let Ok(parsed) = serde_json::from_str::<Vec<ApprovedPlugin>>(&content) {
        return parsed;
    }
    // Legacy: treat anything unparseable as empty (safe re-approval).
    Vec::new()
}

/// sha256 hex over `kf-code.toml` + every capability file the manifest
/// declares, sorted by path for determinism. Files that are declared but
/// absent on disk contribute their literal path string (so a missing-file
/// → missing-file round-trips, but adding the file changes the hash).
pub fn bundle_hash(plugin_root: &Path, manifest: &PluginManifest) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();

    // Manifest first — it binds the declared capability set.
    let manifest_path = plugin_root.join("kf-code.toml");
    if let Ok(bytes) = std::fs::read(&manifest_path) {
        hasher.update(&bytes);
    }

    // Collect declared capability file paths, sorted for determinism.
    let mut paths: Vec<PathBuf> = Vec::new();
    for cap in &manifest.capabilities {
        match cap {
            Capability::Tool {
                command: Some(p), ..
            } => paths.push(plugin_root.join(p)),
            Capability::Hook { command, .. } => paths.push(plugin_root.join(command)),
            Capability::Verifier {
                command: Some(p), ..
            } => paths.push(plugin_root.join(p)),
            Capability::Skill {
                skill_file: Some(f),
                ..
            } => paths.push(plugin_root.join(f)),
            _ => {}
        }
    }
    paths.sort();

    for p in &paths {
        // Hash the path bytes so a renamed file changes the hash even if
        // the content is identical; then hash the file bytes (or a
        // sentinel for missing files so absence is detectable).
        hasher.update(p.to_string_lossy().as_bytes());
        hasher.update(b"\x00");
        match std::fs::read(p) {
            Ok(bytes) => hasher.update(&bytes),
            Err(_) => hasher.update(b"\x01MISSING\x01"),
        }
        hasher.update(b"\x00");
    }

    hex::encode(hasher.finalize())
}

/// Record an approval for a plugin, stamping the current bundle hash.
/// Best-effort: a write failure is logged and swallowed (the next launch
/// re-prompts, which is safe). Reads the manifest from `plugin_root` to
/// compute the hash; if the manifest is unreadable, stamps `None`
/// (re-approval on next load).
pub fn record_plugin_approval(name: &str, plugin_root: &Path) {
    let root_key = plugin_root.to_string_lossy().to_string();
    let hash = PluginManifest::from_file(&plugin_root.join("kf-code.toml"))
        .ok()
        .map(|m| bundle_hash(plugin_root, &m));
    let mut approved = load_approved_plugins();
    if let Some(entry) = approved
        .iter_mut()
        .find(|e| e.name == name && e.root == root_key)
    {
        entry.content_hash = hash;
    } else {
        approved.push(ApprovedPlugin {
            name: name.to_string(),
            root: root_key,
            content_hash: hash,
        });
    }
    let path = match approval_db_path() {
        Some(p) => p,
        None => return,
    };
    let body = match serde_json::to_string(&approved) {
        Ok(b) => b,
        Err(e) => {
            tracing::warn!(error = %e, "failed to serialize approved plugins ledger");
            return;
        }
    };
    if let Err(e) = std::fs::write(&path, body) {
        tracing::warn!(error = %e, path = %path.display(), "failed to persist plugin approval");
    }
}

/// Check whether a plugin is approved *and* its bundle is unchanged since
/// approval. `true` means the plugin may load without re-prompting. A
/// present bundle whose stored hash is missing or differs → `false`.
pub fn is_plugin_approved(name: &str, plugin_root: &Path) -> bool {
    let root_key = plugin_root.to_string_lossy().to_string();
    let approved = load_approved_plugins();
    let Some(entry) = approved
        .into_iter()
        .find(|e| e.name == name && e.root == root_key)
    else {
        return false;
    };
    let manifest = match PluginManifest::from_file(&plugin_root.join("kf-code.toml")) {
        Ok(m) => m,
        Err(_) => return false,
    };
    let current = bundle_hash(plugin_root, &manifest);
    entry.content_hash.is_some_and(|h| h == current)
}

/// Warning string naming the approval command, for the loader to emit when
/// a plugin is skipped by the consent gate.
pub fn plugin_approval_hint(name: &str) -> String {
    format!(
        "{name}: content-hash mismatch or not approved; run `/plugins approve {name}` to re-approve"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::DataDirGuard;

    fn make_plugin(dir: &Path, body: &str) {
        std::fs::write(dir.join("kf-code.toml"), body).unwrap();
    }

    fn base_manifest() -> &'static str {
        r#"
name = "demo"
version = "0.1.0"
description = "demo"
trust = "read-only"

[[capabilities]]
type = "skill"
trigger = "/demo"
prompt = "hello"
"#
    }

    #[test]
    fn approval_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let _g = DataDirGuard::set(dir.path().to_path_buf());
        let plugin = dir.path().join("p");
        std::fs::create_dir_all(&plugin).unwrap();
        make_plugin(&plugin, base_manifest());
        assert!(!is_plugin_approved("demo", &plugin));
        record_plugin_approval("demo", &plugin);
        assert!(is_plugin_approved("demo", &plugin));
    }

    #[test]
    fn edited_manifest_re_gates() {
        let dir = tempfile::tempdir().unwrap();
        let _g = DataDirGuard::set(dir.path().to_path_buf());
        let plugin = dir.path().join("p");
        std::fs::create_dir_all(&plugin).unwrap();
        make_plugin(&plugin, base_manifest());
        record_plugin_approval("demo", &plugin);
        assert!(is_plugin_approved("demo", &plugin));
        // Edit the manifest → hash changes → re-gate.
        make_plugin(
            &plugin,
            r#"
name = "demo"
version = "0.2.0"
description = "changed"
trust = "read-only"
"#,
        );
        assert!(
            !is_plugin_approved("demo", &plugin),
            "edited manifest must re-gate"
        );
    }

    #[test]
    fn edited_capability_script_re_gates() {
        let dir = tempfile::tempdir().unwrap();
        let _g = DataDirGuard::set(dir.path().to_path_buf());
        let plugin = dir.path().join("p");
        std::fs::create_dir_all(&plugin).unwrap();
        std::fs::write(plugin.join("tool.sh"), "#!/bin/sh\nprintf ok").unwrap();
        make_plugin(
            &plugin,
            r#"
name = "demo"
version = "0.1.0"
description = "demo"
trust = "shell"

[[capabilities]]
type = "tool"
name = "demo/run"
description = "run"
command = "tool.sh"
"#,
        );
        record_plugin_approval("demo", &plugin);
        assert!(is_plugin_approved("demo", &plugin));
        // Edit the script → hash changes → re-gate.
        std::fs::write(plugin.join("tool.sh"), "#!/bin/sh\nprintf pwn").unwrap();
        assert!(
            !is_plugin_approved("demo", &plugin),
            "edited script must re-gate"
        );
    }

    #[test]
    fn re_approve_after_edit_loads() {
        let dir = tempfile::tempdir().unwrap();
        let _g = DataDirGuard::set(dir.path().to_path_buf());
        let plugin = dir.path().join("p");
        std::fs::create_dir_all(&plugin).unwrap();
        make_plugin(&plugin, base_manifest());
        record_plugin_approval("demo", &plugin);
        make_plugin(
            &plugin,
            r#"
name = "demo"
version = "0.2.0"
description = "changed"
trust = "read-only"
"#,
        );
        assert!(!is_plugin_approved("demo", &plugin));
        record_plugin_approval("demo", &plugin);
        assert!(is_plugin_approved("demo", &plugin));
    }

    #[test]
    fn different_plugin_not_approved() {
        let dir = tempfile::tempdir().unwrap();
        let _g = DataDirGuard::set(dir.path().to_path_buf());
        let plugin = dir.path().join("p");
        std::fs::create_dir_all(&plugin).unwrap();
        make_plugin(&plugin, base_manifest());
        record_plugin_approval("demo", &plugin);
        let other = dir.path().join("other");
        std::fs::create_dir_all(&other).unwrap();
        make_plugin(&other, base_manifest());
        assert!(!is_plugin_approved("demo", &other));
    }

    #[test]
    fn bundle_hash_is_deterministic() {
        let dir = tempfile::tempdir().unwrap();
        let plugin = dir.path().join("p");
        std::fs::create_dir_all(&plugin).unwrap();
        std::fs::write(plugin.join("tool.sh"), "x").unwrap();
        let m = PluginManifest::from_file(&plugin.join("kf-code.toml")).ok();
        // No manifest → no hash check needed here; just exercise the fn.
        let body = r#"
name = "demo"
version = "0.1.0"
description = "demo"
trust = "shell"

[[capabilities]]
type = "tool"
name = "demo/run"
description = "run"
command = "tool.sh"
"#;
        make_plugin(&plugin, body);
        let manifest = PluginManifest::from_file(&plugin.join("kf-code.toml")).unwrap();
        let h1 = bundle_hash(&plugin, &manifest);
        let h2 = bundle_hash(&plugin, &manifest);
        assert_eq!(h1, h2, "bundle_hash must be deterministic");
        let _ = m; // silence unused warning
    }

    #[test]
    fn legacy_entry_without_hash_re_approves() {
        let dir = tempfile::tempdir().unwrap();
        let _g = DataDirGuard::set(dir.path().to_path_buf());
        let plugin = dir.path().join("p");
        std::fs::create_dir_all(&plugin).unwrap();
        make_plugin(&plugin, base_manifest());
        // Hand-write a ledger with a legacy entry (no content_hash).
        let db = approval_db_path().unwrap();
        let legacy = serde_json::to_string(&vec![ApprovedPlugin {
            name: "demo".to_string(),
            root: plugin.to_string_lossy().to_string(),
            content_hash: None,
        }])
        .unwrap();
        std::fs::write(&db, legacy).unwrap();
        assert!(
            !is_plugin_approved("demo", &plugin),
            "legacy entry without hash must re-approve"
        );
    }
}
