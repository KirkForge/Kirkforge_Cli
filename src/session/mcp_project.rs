//! Project-local `.mcp.json` discovery (WO 39.2, Claude compat phase 1).
//!
//! A Claude-style `.mcp.json` in the project root declares MCP servers
//! under an `mcpServers` object. This module parses it into
//! `McpServerConfig` entries and enforces a per-project approval gate:
//! the file lives in the repo, so a cloned project can ship attacker-
//! controllable spawn config. The first time a project's `.mcp.json` is
//! seen, kf-code must ask before spawning anything; the approval is
//! persisted in the data dir so subsequent launches are silent.

use crate::shared::McpServerConfig;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Parsed shape of a `.mcp.json` file. Only the `mcpServers` key is
/// consumed; unknown keys are ignored.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct ProjectMcpJson {
    #[serde(default, rename = "mcpServers")]
    pub mcp_servers: HashMap<String, ProjectMcpServer>,
}

/// One entry under `mcpServers`. Claude's format supports both `command`
/// (stdio) and `url` (http) shapes; we map both onto `McpServerConfig`.
#[derive(Debug, Clone, serde::Deserialize, Default)]
pub struct ProjectMcpServer {
    #[serde(default)]
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: HashMap<String, String>,
    /// HTTP transport: a server URL.
    #[serde(default)]
    pub url: String,
    /// HTTP bearer token (Claude's `headers` is not supported yet; a
    /// bare `token`/`authorization` field is). ponytail: parse Claude's
    /// `headers: { "Authorization": "Bearer X" }` form when a real
    /// fixture needs it; the bare field covers the common case.
    #[serde(default)]
    pub token: String,
    /// Explicit transport override; inferred from `command` vs `url` when
    /// absent.
    #[serde(default)]
    pub r#type: String,
}

/// Load and parse `.mcp.json` from a project root. Returns `Ok(None)`
/// when the file is absent (the common case). Returns an error only for
/// a present-but-unparseable file so callers can surface bad config.
pub fn parse_project_mcp_json(project_root: &Path) -> anyhow::Result<Option<ProjectMcpJson>> {
    let path = project_root.join(".mcp.json");
    if !path.exists() {
        return Ok(None);
    }
    let content = std::fs::read_to_string(&path)?;
    let parsed: ProjectMcpJson = serde_json::from_str(&content)
        .map_err(|e| anyhow::anyhow!("failed to parse {}: {e}", path.display()))?;
    Ok(Some(parsed))
}

/// Convert a parsed `ProjectMcpJson` into `McpServerConfig` entries,
/// preserving the declaration order of `mcpServers` (sorted by name for
/// determinism).
pub fn to_mcp_server_configs(doc: &ProjectMcpJson) -> Vec<McpServerConfig> {
    let mut names: Vec<&String> = doc.mcp_servers.keys().collect();
    names.sort();
    names
        .into_iter()
        .filter_map(|name| {
            let entry = doc.mcp_servers.get(name)?;
            Some(ProjectMcpServer::into_config(name, entry))
        })
        .collect()
}

impl ProjectMcpServer {
    fn into_config(name: &str, entry: &Self) -> McpServerConfig {
        let transport = if !entry.r#type.is_empty() {
            entry.r#type.clone()
        } else if !entry.url.is_empty() {
            "http".to_string()
        } else {
            "stdio".to_string()
        };
        McpServerConfig {
            name: name.to_string(),
            transport,
            command: entry.command.clone(),
            args: entry.args.clone(),
            env_vars: entry.env.clone(),
            url: entry.url.clone(),
            bearer_token: entry.token.clone(),
        }
    }
}

// ── Per-project approval gate ──────────────────────────────────────

/// Path of the persisted approval ledger inside the kf-code data dir.
pub fn approval_db_path() -> Option<PathBuf> {
    crate::session::data_dir()
        .ok()
        .map(|d| d.join("approved_mcp_projects.json"))
}

/// One approved project: its path plus a content hash of the `.mcp.json`
/// that was approved. The hash lets us re-gate when the file changes under
/// an already-approved path (WO 42.5). `None` means the entry predates the
/// hash, or the file was absent at approval time — both are treated as
/// "needs re-approval" when a file is present, which is the safe default.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ApprovedProject {
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_hash: Option<String>,
}

/// Project fingerprint: the absolute path of the project root. A path
/// change (different checkout) re-prompts.
pub fn project_key(project_root: &Path) -> String {
    project_root.to_string_lossy().to_string()
}

/// sha256 hex of the bytes of `project_root/.mcp.json`, or `None` if the
/// file is absent. Used both at approval time (to stamp the entry) and at
/// load time (to detect a change).
fn current_content_hash(project_root: &Path) -> Option<String> {
    let path = project_root.join(".mcp.json");
    let bytes = std::fs::read(&path).ok()?;
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    Some(hex::encode(hasher.finalize()))
}

/// Load the approved-project ledger. Absent file → empty vec. Lenient of
/// both the new (`Vec<ApprovedProject>`) and legacy (`Vec<String>`)
/// formats: a legacy entry is read back with `content_hash: None`, which
/// `is_approved` treats as "needs re-approval" when a file is present.
pub fn load_approved_projects() -> Vec<ApprovedProject> {
    let path = match approval_db_path() {
        Some(p) => p,
        None => return Vec::new(),
    };
    let content = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };
    // New format first.
    if let Ok(parsed) = serde_json::from_str::<Vec<ApprovedProject>>(&content) {
        return parsed;
    }
    // Legacy format: `Vec<String>` of bare paths. Migrate to entries with
    // no hash (→ re-approval on next load, the safe default).
    serde_json::from_str::<Vec<String>>(&content)
        .unwrap_or_default()
        .into_iter()
        .map(|path| ApprovedProject {
            path,
            content_hash: None,
        })
        .collect()
}

/// Record an approval for a project path, stamping the current `.mcp.json`
/// content hash so a later edit re-gates. Best-effort: a write failure is
/// logged and swallowed (the next launch re-prompts, which is safe).
pub fn record_approval(project_root: &Path) {
    let key = project_key(project_root);
    let hash = current_content_hash(project_root);
    let mut approved = load_approved_projects();
    if let Some(entry) = approved.iter_mut().find(|e| e.path == key) {
        entry.content_hash = hash;
    } else {
        approved.push(ApprovedProject {
            path: key,
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
            tracing::warn!(error = %e, "failed to serialize approved MCP projects");
            return;
        }
    };
    if let Err(e) = std::fs::write(&path, body) {
        tracing::warn!(error = %e, path = %path.display(), "failed to persist MCP project approval");
    }
}

/// Check whether a project is approved *and* its `.mcp.json` is unchanged
/// since approval. `true` means the servers may load without prompting.
/// A present file whose stored hash is missing or differs → `false` (re-gate).
pub fn is_approved(project_root: &Path) -> bool {
    let key = project_key(project_root);
    let approved = load_approved_projects();
    let Some(entry) = approved.into_iter().find(|e| e.path == key) else {
        return false;
    };
    match current_content_hash(project_root) {
        // File present: must match the stored hash exactly.
        Some(current) => entry.content_hash.is_some_and(|h| h == current),
        // File absent now. Approved iff it was also absent at approval time
        // (stored None) — there is nothing to spawn, so no re-gate needed.
        None => entry.content_hash.is_none(),
    }
}

/// Decide whether to load `.mcp.json` servers for a project, given the
/// config flag and the persisted approval state. Returns the effective
/// server list (empty when blocked) and a `prompted` flag so the caller
/// can issue the approval prompt when one is needed.
///
/// This is a pure function over inputs; the actual prompt UI is the
/// caller's job (run_session prints to stderr in line mode, the TUI
/// would surface a modal). Keeping it pure makes it testable without a
/// terminal.
pub fn resolve_project_mcp(
    config_flag: bool,
    doc: Option<&ProjectMcpJson>,
    already_approved: bool,
) -> (Vec<McpServerConfig>, bool) {
    if !config_flag {
        return (Vec::new(), false);
    }
    let Some(doc) = doc else {
        return (Vec::new(), false);
    };
    if !already_approved {
        // Blocked: do not spawn. The caller should prompt and, on approval,
        // call `record_approval` then re-invoke with `already_approved = true`.
        return (Vec::new(), true);
    }
    (to_mcp_server_configs(doc), false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_mcp_json(dir: &Path, body: &str) -> PathBuf {
        let path = dir.join(".mcp.json");
        std::fs::write(&path, body).unwrap();
        path
    }

    #[test]
    fn parse_stdio_and_http_servers() {
        let tmp = tempfile::tempdir().unwrap();
        write_mcp_json(
            tmp.path(),
            r#"{
                "mcpServers": {
                    "stdio-srv": {
                        "command": "npx",
                        "args": ["-y", "some-server"]
                    },
                    "http-srv": {
                        "url": "https://example.com/mcp",
                        "token": "secret-token"
                    }
                }
            }"#,
        );
        let doc = parse_project_mcp_json(tmp.path()).unwrap().unwrap();
        let configs = to_mcp_server_configs(&doc);
        assert_eq!(configs.len(), 2);

        let stdio = configs.iter().find(|c| c.name == "stdio-srv").unwrap();
        assert_eq!(stdio.transport, "stdio");
        assert_eq!(stdio.command, "npx");
        assert_eq!(stdio.args, vec!["-y", "some-server"]);

        let http = configs.iter().find(|c| c.name == "http-srv").unwrap();
        assert_eq!(http.transport, "http");
        assert_eq!(http.url, "https://example.com/mcp");
        assert_eq!(http.bearer_token, "secret-token");
    }

    #[test]
    fn parse_env_vars_passed_through() {
        let tmp = tempfile::tempdir().unwrap();
        write_mcp_json(
            tmp.path(),
            r#"{
                "mcpServers": {
                    "srv": {
                        "command": "node",
                        "args": ["s.js"],
                        "env": { "FOO": "bar", "DEBUG": "1" }
                    }
                }
            }"#,
        );
        let doc = parse_project_mcp_json(tmp.path()).unwrap().unwrap();
        let cfg = &to_mcp_server_configs(&doc)[0];
        assert_eq!(cfg.env_vars.get("FOO").unwrap(), "bar");
        assert_eq!(cfg.env_vars.get("DEBUG").unwrap(), "1");
    }

    #[test]
    fn parse_explicit_type_overrides_inference() {
        let tmp = tempfile::tempdir().unwrap();
        write_mcp_json(
            tmp.path(),
            r#"{
                "mcpServers": {
                    "srv": { "command": "x", "type": "http", "url": "http://h/mcp" }
                }
            }"#,
        );
        let doc = parse_project_mcp_json(tmp.path()).unwrap().unwrap();
        let cfg = &to_mcp_server_configs(&doc)[0];
        assert_eq!(cfg.transport, "http");
    }

    #[test]
    fn parse_missing_file_returns_none() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(parse_project_mcp_json(tmp.path()).unwrap().is_none());
    }

    #[test]
    fn parse_empty_mcp_servers_returns_empty_configs() {
        let tmp = tempfile::tempdir().unwrap();
        write_mcp_json(tmp.path(), r#"{ "mcpServers": {} }"#);
        let doc = parse_project_mcp_json(tmp.path()).unwrap().unwrap();
        assert!(to_mcp_server_configs(&doc).is_empty());
    }

    #[test]
    fn parse_malformed_json_returns_error() {
        let tmp = tempfile::tempdir().unwrap();
        write_mcp_json(tmp.path(), r#"{ not valid json"#);
        let err = parse_project_mcp_json(tmp.path());
        assert!(err.is_err());
    }

    #[test]
    fn resolve_blocks_when_not_approved() {
        let tmp = tempfile::tempdir().unwrap();
        write_mcp_json(
            tmp.path(),
            r#"{ "mcpServers": { "s": { "command": "x" } } }"#,
        );
        let doc = parse_project_mcp_json(tmp.path()).unwrap().unwrap();
        let (servers, prompted) = resolve_project_mcp(true, Some(&doc), false);
        assert!(
            servers.is_empty(),
            "unapproved project must not load servers"
        );
        assert!(prompted, "caller should be told to prompt");
    }

    #[test]
    fn resolve_admits_on_approval() {
        let tmp = tempfile::tempdir().unwrap();
        write_mcp_json(
            tmp.path(),
            r#"{ "mcpServers": { "s": { "command": "x" } } }"#,
        );
        let doc = parse_project_mcp_json(tmp.path()).unwrap().unwrap();
        let (servers, prompted) = resolve_project_mcp(true, Some(&doc), true);
        assert_eq!(servers.len(), 1);
        assert_eq!(servers[0].name, "s");
        assert!(!prompted);
    }

    #[test]
    fn resolve_config_flag_off_blocks_everything() {
        let tmp = tempfile::tempdir().unwrap();
        write_mcp_json(
            tmp.path(),
            r#"{ "mcpServers": { "s": { "command": "x" } } }"#,
        );
        let doc = parse_project_mcp_json(tmp.path()).unwrap().unwrap();
        let (servers, prompted) = resolve_project_mcp(false, Some(&doc), true);
        assert!(servers.is_empty());
        assert!(!prompted);
    }

    #[test]
    fn resolve_no_file_is_noop() {
        let (servers, prompted) = resolve_project_mcp(true, None, false);
        assert!(servers.is_empty());
        assert!(!prompted);
    }

    #[test]
    fn approval_persistence_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let _guard = crate::session::DataDirGuard::set(dir.path().to_path_buf());
        let proj = PathBuf::from("/fake/project");
        assert!(!is_approved(&proj));
        record_approval(&proj);
        assert!(is_approved(&proj));
        // A different project is still unapproved.
        assert!(!is_approved(&PathBuf::from("/other/project")));
    }

    #[test]
    fn approval_persistence_survives_reload() {
        let dir = tempfile::tempdir().unwrap();
        let _guard = crate::session::DataDirGuard::set(dir.path().to_path_buf());
        let proj = PathBuf::from("/fake/project2");
        record_approval(&proj);
        // Drop the guard's scope effect by re-reading from the same data dir.
        assert!(is_approved(&proj));
        // The file must exist and contain the project key.
        let db = approval_db_path().unwrap();
        let content = std::fs::read_to_string(&db).unwrap();
        assert!(content.contains("/fake/project2"));
    }

    // ── WO 42.5: content-based re-approval ──────────────────────────

    #[test]
    fn approval_persists_with_content_hash() {
        let dir = tempfile::tempdir().unwrap();
        let _guard = crate::session::DataDirGuard::set(dir.path().to_path_buf());
        let proj = dir.path().to_path_buf();
        write_mcp_json(&proj, r#"{ "mcpServers": { "s": { "command": "x" } } }"#);
        assert!(!is_approved(&proj));
        record_approval(&proj);
        // The persisted entry must carry a non-empty content hash.
        let db = approval_db_path().unwrap();
        let body: Vec<ApprovedProject> =
            serde_json::from_str(&std::fs::read_to_string(&db).unwrap()).unwrap();
        let entry = body
            .iter()
            .find(|e| e.path == proj.to_string_lossy())
            .unwrap();
        let hash = entry.content_hash.as_ref().expect("hash stamped");
        assert!(!hash.is_empty());
    }

    #[test]
    fn modified_file_re_prompts() {
        let dir = tempfile::tempdir().unwrap();
        let _guard = crate::session::DataDirGuard::set(dir.path().to_path_buf());
        let proj = dir.path().to_path_buf();
        write_mcp_json(&proj, r#"{ "mcpServers": { "s": { "command": "x" } } }"#);
        record_approval(&proj);
        assert!(is_approved(&proj));
        // Attacker edits the file: add a new malicious server entry.
        write_mcp_json(
            &proj,
            r#"{ "mcpServers": { "s": { "command": "x" }, "evil": { "command": "pwn" } } }"#,
        );
        assert!(
            !is_approved(&proj),
            "a changed .mcp.json must re-gate, not load silently"
        );
    }

    #[test]
    fn unchanged_file_loads_silently() {
        let dir = tempfile::tempdir().unwrap();
        let _guard = crate::session::DataDirGuard::set(dir.path().to_path_buf());
        let proj = dir.path().to_path_buf();
        let body = r#"{ "mcpServers": { "s": { "command": "x" } } }"#;
        write_mcp_json(&proj, body);
        record_approval(&proj);
        // Re-read without touching the file — must stay approved.
        assert!(is_approved(&proj));
        assert!(is_approved(&proj));
    }

    #[test]
    fn legacy_entry_without_hash_triggers_re_approval() {
        let dir = tempfile::tempdir().unwrap();
        let _guard = crate::session::DataDirGuard::set(dir.path().to_path_buf());
        let proj = dir.path().to_path_buf();
        write_mcp_json(&proj, r#"{ "mcpServers": { "s": { "command": "x" } } }"#);
        // Hand-write a legacy-format ledger (bare path strings, no hashes).
        let db = approval_db_path().unwrap();
        let legacy = serde_json::to_string(&vec![proj.to_string_lossy().to_string()]).unwrap();
        std::fs::write(&db, legacy).unwrap();
        // The project is "in the ledger" but has no content hash → re-gate.
        assert!(
            !is_approved(&proj),
            "legacy entry without hash must re-approve (safe default)"
        );
    }
}
