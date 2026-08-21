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

/// Project fingerprint: the absolute path of the project root. A path
/// change (different checkout) re-prompts; this is intentionally simple
/// and does not hash the file contents (a changed `.mcp.json` under an
/// approved path is not re-gated — ponytail: hash the file when a real
/// audit demands it; the path key already prevents cross-project spray).
pub fn project_key(project_root: &Path) -> String {
    project_root.to_string_lossy().to_string()
}

/// Load the set of approved project paths. Absent file → empty set.
pub fn load_approved_projects() -> Vec<String> {
    let path = match approval_db_path() {
        Some(p) => p,
        None => return Vec::new(),
    };
    match std::fs::read_to_string(&path) {
        Ok(content) => {
            let parsed: Vec<String> = serde_json::from_str(&content).unwrap_or_default();
            parsed
        }
        Err(_) => Vec::new(),
    }
}

/// Record an approval for a project path. Best-effort: a write failure
/// is logged and swallowed (the next launch re-prompts, which is safe).
pub fn record_approval(project_root: &Path) {
    let key = project_key(project_root);
    let mut approved = load_approved_projects();
    if !approved.contains(&key) {
        approved.push(key);
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

/// Check whether a project has already been approved. `true` means the
/// `.mcp.json` servers may load without prompting.
pub fn is_approved(project_root: &Path) -> bool {
    let key = project_key(project_root);
    load_approved_projects().contains(&key)
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
}
