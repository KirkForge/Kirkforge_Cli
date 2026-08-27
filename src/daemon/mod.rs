//! Session daemon — lightweight background process that tracks the last
//! few sessions and provides a fast resume path.
//!
//! The daemon does **not** run the TUI or the executor. It owns only
//! session metadata: which `*.conv.ndjson` files exist, which are the
//! most recent, and how to resolve a short id/prefix to a full path.
//!
//! Communication is line-delimited JSON over a Unix domain socket.

pub mod client;
pub mod paths;

#[cfg(unix)]
pub mod server;

use crate::session::session_index::SessionEntry;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::collections::VecDeque;
use std::ffi::OsStr;
use std::sync::Arc;

#[cfg(unix)]
use std::os::unix::process::CommandExt;

use anyhow::Context;

// RBAC permission tiers (WO 43.6). The daemon maps its auth token to an
// `Actor` with a role and checks a per-op `Permission` after the existing
// `check_auth` token gate. Single-token deployments default to admin.
use kf_rbac::Role;
#[cfg(unix)]
use kf_rbac::{Actor, AuthMethod, Permission};

/// Read the auth token from the `KF_CODE_DAEMON_TOKEN_FILE` env var.
/// Returns `None` if the env var is not set or the file cannot be read.
pub fn read_auth_token() -> Option<String> {
    std::env::var("KF_CODE_DAEMON_TOKEN_FILE")
        .ok()
        .and_then(|path| std::fs::read_to_string(&path).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Constant-time token check shared by the session daemon and jobd.
/// Returns `Ok(())` if no token is configured (auth disabled) or the
/// supplied token matches. Both sides are hashed to a fixed-size digest
/// before the constant-time compare so timing cannot leak token length
/// (WO 46.32; extracted and reused by jobd in WO 47.16).
pub fn check_auth_ct(supplied: Option<&str>, expected: Option<&str>) -> Result<(), String> {
    let expected = match expected {
        None => return Ok(()),
        Some(t) => t,
    };
    match supplied {
        None => Err("authentication required".to_string()),
        Some(given) => {
            use sha2::{Digest, Sha256};
            let expected_digest = Sha256::digest(expected.as_bytes());
            let given_digest = Sha256::digest(given.as_bytes());
            if subtle::ConstantTimeEq::ct_eq(expected_digest.as_slice(), given_digest.as_slice())
                .into()
            {
                Ok(())
            } else {
                Err("authentication failed".to_string())
            }
        }
    }
}

/// Read the daemon role from `KF_CODE_DAEMON_ROLE` (WO 43.6). Falls back
/// to `admin` when unset or unparseable, so single-token deployments keep
/// today's all-access behavior. Unknown values are denied by
/// `Role::from_str`; we fall back to admin (not viewer) because the
/// existing single-token contract granted full access — downgrading
/// silently would be a behavior change.
pub fn read_daemon_role() -> Role {
    std::env::var("KF_CODE_DAEMON_ROLE")
        .ok()
        .and_then(|s| s.parse::<Role>().ok())
        .unwrap_or(Role::Admin)
}

/// Spawn SIGINT/SIGHUP/SIGTERM handlers that all notify the same `shutdown`
/// handle. Used by both the session daemon and the jobs daemon.
#[cfg(unix)]
pub fn spawn_shutdown_signal_handlers(shutdown: Arc<tokio::sync::Notify>) {
    let ctrl_c_shutdown = shutdown.clone();
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            tracing::info!("received SIGINT; shutting down");
            ctrl_c_shutdown.notify_one();
        }
    });

    use tokio::signal::unix::{signal, SignalKind};

    let hup_shutdown = shutdown.clone();
    tokio::spawn(async move {
        match signal(SignalKind::hangup()) {
            Ok(mut hup) => {
                if hup.recv().await.is_some() {
                    tracing::info!("received SIGHUP; shutting down");
                    hup_shutdown.notify_one();
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, "could not install SIGHUP handler");
            }
        }
    });

    let term_shutdown = shutdown.clone();
    tokio::spawn(async move {
        match signal(SignalKind::terminate()) {
            Ok(mut term) => {
                if term.recv().await.is_some() {
                    tracing::info!("received SIGTERM; shutting down");
                    term_shutdown.notify_one();
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, "could not install SIGTERM handler");
            }
        }
    });
}

#[cfg(not(unix))]
pub fn spawn_shutdown_signal_handlers(shutdown: Arc<tokio::sync::Notify>) {
    let ctrl_c_shutdown = shutdown;
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            tracing::info!("received SIGINT; shutting down");
            ctrl_c_shutdown.notify_one();
        }
    });
}

/// Write one NDJSON line to a stream: serialize `resp`, append `\n`, flush.
/// Shared by the session daemon server and the jobs daemon client handler.
pub async fn write_ndjson_response<W, T>(stream: &mut W, resp: &T) -> anyhow::Result<()>
where
    W: tokio::io::AsyncWriteExt + Unpin,
    T: serde::Serialize,
{
    let line = serde_json::to_string(resp).context("serialize response")?;
    stream
        .write_all(line.as_bytes())
        .await
        .context("write response")?;
    stream.write_all(b"\n").await.context("write newline")?;
    stream.flush().await.context("flush response")?;
    Ok(())
}

/// Detach the current process into the background by re-executing the same
/// binary with `args` in a new session.
///
/// The caller should pass the subcommand and flags that will run the target
/// daemon in the foreground (e.g. `["daemon", "--foreground"]`). After spawning
/// the detached child, the parent exits with status 0 so the terminal session
/// is released.
#[cfg(unix)]
pub fn daemonize<I, S>(args: I) -> anyhow::Result<()>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let current_exe = std::env::current_exe().context("get current exe")?;
    let mut cmd = std::process::Command::new(current_exe);
    cmd.args(args);
    if let Ok(v) = std::env::var("KF_CODE_DATA_DIR") {
        cmd.env("KF_CODE_DATA_DIR", v);
    }
    // Detach stdio: the daemonized child is long-lived and must not hold the
    // caller's piped stdout/stderr open (same reason as `start_daemon` in
    // client.rs — piped callers would hang on read_to_end after we exit).
    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    // Create a new session so the daemon survives the closing of the
    // terminal/session that spawned it. Without setsid the daemon remains in
    // the parent's process group and gets SIGHUP when the user logs out or the
    // parent exits.
    unsafe {
        cmd.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    cmd.spawn().context("spawn daemon foreground process")?;
    std::process::exit(0);
}

#[cfg(not(unix))]
pub fn daemonize<I, S>(_args: I) -> anyhow::Result<()>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    anyhow::bail!("background daemon mode is only supported on Unix; use --foreground")
}

/// Maximum size (in bytes) for a single daemon request/response frame.
///
/// The daemon protocol is line-delimited JSON; a missing newline from a
/// corrupted peer would otherwise let `read_line` grow without bound.
/// One megabyte is far larger than any legitimate request or response.
pub const MAX_FRAME_SIZE: usize = 1024 * 1024;

/// Read one LF-delimited line from `reader` into `buf`, capping the total
/// frame size at [`MAX_FRAME_SIZE`].
///
/// Returns the length of the line in bytes. Errors with `InvalidData` if
/// the frame exceeds the cap before a newline is seen.
pub async fn read_line_limited<R>(reader: &mut R, buf: &mut String) -> std::io::Result<usize>
where
    R: tokio::io::AsyncBufReadExt + Unpin,
{
    buf.clear();
    let mut raw = Vec::new();
    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            *buf = String::from_utf8_lossy(&raw).into_owned();
            return Ok(buf.len());
        }
        let remaining = MAX_FRAME_SIZE.saturating_sub(raw.len());
        if remaining == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("daemon frame exceeds {MAX_FRAME_SIZE} byte limit"),
            ));
        }
        let to_take = std::cmp::min(available.len(), remaining);
        let chunk = &available[..to_take];
        if let Some(pos) = chunk.iter().position(|&b| b == b'\n') {
            raw.extend_from_slice(&chunk[..=pos]);
            reader.consume(pos + 1);
            *buf = String::from_utf8_lossy(&raw).into_owned();
            return Ok(buf.len());
        }
        raw.extend_from_slice(chunk);
        reader.consume(to_take);
    }
}

/// Maximum number of recent sessions the daemon remembers.
pub const RECENT_SESSIONS_LIMIT: usize = 5;
/// Bounded capacity for each per-instance broadcast channel.
/// ponytail: 1024 is arbitrary but bounded; tune when throughput measurement shows it's wrong.
pub const INSTANCE_CHANNEL_CAPACITY: usize = 1024;
/// A request sent from a client to the daemon.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "op")]
pub enum Request {
    /// Health check. Optionally carries a version string for compatibility
    /// gating and an auth token.
    #[serde(rename = "ping")]
    Ping {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        version: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        auth_token: Option<String>,
    },

    /// Return the last `RECENT_SESSIONS_LIMIT` sessions, newest first.
    #[serde(rename = "list")]
    List {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        auth_token: Option<String>,
    },

    /// Resolve a session id or prefix to a log path.
    #[serde(rename = "resolve")]
    Resolve {
        id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        auth_token: Option<String>,
    },

    /// Mark a session as recently used.
    #[serde(rename = "touch")]
    Touch {
        id: String,
        path: PathBufSerde,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        auth_token: Option<String>,
    },

    /// Ask the daemon to shut down gracefully.
    #[serde(rename = "shutdown")]
    Shutdown {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        auth_token: Option<String>,
    },

    /// Claim exclusive ownership of a session. Returns `Busy` if another
    /// connection already holds it; `Ok` otherwise. The claim is released
    /// automatically when the owning connection closes.
    #[serde(rename = "claim")]
    Claim {
        id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        auth_token: Option<String>,
    },

    /// Ask the daemon to broadcast `Quit` to all registered instances and
    /// then shut down. Carries an optional auth token like `Shutdown`.
    #[serde(rename = "quit_all")]
    QuitAll {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        auth_token: Option<String>,
    },

    /// Notify the daemon that scheduled jobs changed (completed, created,
    /// or cancelled) so it can push `JobsChanged` to all registered TUI
    /// instances. Sent by the jobs daemon after a batch run.
    #[serde(rename = "notify_jobs_changed")]
    NotifyJobsChanged {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        auth_token: Option<String>,
    },

    /// Open a long-lived instance channel. The daemon pushes
    /// `InstanceEvent`s to every registered instance. The stream
    /// switches to push-only after the registration handshake.
    #[serde(rename = "instance_register")]
    InstanceRegister {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        version: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        auth_token: Option<String>,
    },
}

/// A response sent from the daemon back to a client.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "status")]
pub enum Response {
    #[serde(rename = "ok")]
    Ok { data: Option<serde_json::Value> },
    #[serde(rename = "error")]
    Error { message: String },
    #[serde(rename = "busy")]
    Busy { message: String },
}

/// An event pushed by the daemon to every registered instance.
///
/// Serialised as one NDJSON line per event. The writer-per-connection
/// guarantee means no interleaving between concurrent broadcasts.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "event")]
pub enum InstanceEvent {
    /// The list of recent sessions changed (new fork, touch, etc.).
    #[serde(rename = "threads_changed")]
    ThreadsChanged,

    /// A scheduled job completed (or was created/cancelled).
    #[serde(rename = "jobs_changed")]
    JobsChanged,

    /// The daemon is shutting down; clients should disconnect.
    #[serde(rename = "quit")]
    Quit,
}

impl Response {
    pub fn ok_empty() -> Self {
        Response::Ok { data: None }
    }

    pub fn ok_json(value: serde_json::Value) -> Self {
        Response::Ok { data: Some(value) }
    }

    pub fn error(message: impl Into<String>) -> Self {
        Response::Error {
            message: message.into(),
        }
    }
}

/// Wrapper so `PathBuf` serializes nicely in JSON without needing a custom
/// module on every use.
#[derive(Debug, Clone, Deserialize, Serialize, Default, PartialEq, Eq)]
#[serde(transparent)]
pub struct PathBufSerde {
    pub path: std::path::PathBuf,
}

impl From<std::path::PathBuf> for PathBufSerde {
    fn from(path: std::path::PathBuf) -> Self {
        Self { path }
    }
}

impl From<PathBufSerde> for std::path::PathBuf {
    fn from(p: PathBufSerde) -> Self {
        p.path
    }
}

/// In-memory state kept by the daemon.
#[derive(Debug, Clone)]
pub struct DaemonState {
    /// Last N sessions, newest at the front.
    pub recent: VecDeque<SessionEntry>,
    /// Auth token loaded from `KF_CODE_DAEMON_TOKEN_FILE`. `None` means
    /// auth is disabled (legacy, no-auth mode).
    pub expected_token: Option<String>,
    /// Role assigned to the token bearer (WO 43.6). Read from
    /// `KF_CODE_DAEMON_ROLE` (fallback `admin`). Single-token
    /// deployments keep admin behavior.
    pub role: Role,
    /// Set of session ids currently claimed by an active connection.
    /// Released automatically when the owning connection closes.
    pub open_sessions: HashSet<String>,
    /// Registered instance channels. Each sender feeds a single writer
    /// task that serialises `InstanceEvent`s onto one `UnixStream`.
    /// Dead senders (disconnected instances) are pruned by `broadcast`.
    #[cfg(unix)]
    pub instances: Arc<std::sync::Mutex<Vec<tokio::sync::mpsc::Sender<InstanceEvent>>>>,
}

#[cfg(unix)]
impl Default for DaemonState {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(unix)]
impl DaemonState {
    pub fn new() -> Self {
        let expected_token = std::env::var("KF_CODE_DAEMON_TOKEN_FILE")
            .ok()
            .and_then(|path| std::fs::read_to_string(&path).ok())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        Self {
            recent: VecDeque::with_capacity(RECENT_SESSIONS_LIMIT + 1),
            expected_token,
            role: read_daemon_role(),
            open_sessions: HashSet::new(),
            instances: Arc::new(std::sync::Mutex::new(Vec::new())),
        }
    }

    /// Push `ev` to every registered instance, dropping senders whose
    /// receiver has been dropped (disconnected instance). Uses `try_send`
    /// so a slow instance never blocks the broadcaster. A full channel
    /// (slow consumer) logs a warning rather than silently dropping.
    pub fn broadcast(&self, ev: InstanceEvent) {
        let mut instances = self.instances.lock().unwrap();
        instances.retain(|tx| match tx.try_send(ev.clone()) {
            Ok(()) => true,
            Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
                tracing::warn!("instance channel full; dropping event");
                true
            }
            Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => false,
        });
    }

    /// Check the supplied token against `expected_token`. Returns `Ok(())`
    /// if auth is disabled (no token configured) or the token matches.
    /// Returns `Err(response)` if the token is wrong or missing when
    /// required. Timing-safe comparison lives in `check_auth_ct`.
    pub fn check_auth(&self, supplied: Option<&str>) -> Result<(), Response> {
        check_auth_ct(supplied, self.expected_token.as_deref()).map_err(Response::error)
    }

    /// Build the `Actor` for a request, for the RBAC permission check
    /// (WO 43.6). Call after `check_auth` has succeeded: when auth is off
    /// the actor is admin (preserves today's all-access behavior); when
    /// auth is on the actor carries the configured `role`. The token
    /// value is not re-validated here — `check_auth` is the gate.
    pub fn actor_for_token(&self, supplied: Option<&str>) -> Actor {
        let id = match supplied {
            Some(_) => format!("daemon:{}", self.role.as_str()),
            None => "daemon:unauthenticated".to_string(),
        };
        Actor {
            id,
            role: self.role,
            tenant_id: String::new(),
            auth_method: AuthMethod::ApiKey,
            verified_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        }
    }

    /// Map a request op to the `Permission` it requires (WO 43.6).
    /// Shutdown/QuitAll → OperatorRestart; List/Resolve/Touch/Claim →
    /// ViewerResults; Ping/NotifyJobsChanged/InstanceRegister →
    /// ViewerStatus. Admin satisfies all of these (see ADMIN_PERMS).
    pub fn required_permission(req: &Request) -> Permission {
        match req {
            Request::Shutdown { .. } | Request::QuitAll { .. } => Permission::OperatorRestart,
            Request::List { .. }
            | Request::Resolve { .. }
            | Request::Touch { .. }
            | Request::Claim { .. } => Permission::ViewerResults,
            Request::Ping { .. }
            | Request::NotifyJobsChanged { .. }
            | Request::InstanceRegister { .. } => Permission::ViewerStatus,
        }
    }
}

#[cfg(not(unix))]
impl Default for DaemonState {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(not(unix))]
impl DaemonState {
    pub fn new() -> Self {
        let expected_token = std::env::var("KF_CODE_DAEMON_TOKEN_FILE")
            .ok()
            .and_then(|path| std::fs::read_to_string(&path).ok())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        Self {
            recent: VecDeque::with_capacity(RECENT_SESSIONS_LIMIT + 1),
            expected_token,
            role: read_daemon_role(),
            open_sessions: HashSet::new(),
        }
    }
}

impl DaemonState {
    /// Refresh the recent list from disk.
    ///
    /// This re-scans the sessions directory rather than reusing the
    /// cached `.index.ndjson`, so newly appended messages are reflected
    /// in the daemon's recent-session list.
    pub fn refresh(&mut self) {
        match crate::session::session_index::SessionIndex::load_or_refresh()
            .and_then(|mut index| index.refresh().map(|_| index.list()))
        {
            Ok(entries) => {
                self.recent = entries.into_iter().take(RECENT_SESSIONS_LIMIT).collect();
            }
            Err(e) => {
                tracing::warn!(error = %e, "daemon failed to list sessions");
            }
        }
    }

    /// Move the touched session to the front, or refresh from disk if it
    /// isn't already known.
    pub fn touch(&mut self, id: &str, path: std::path::PathBuf) {
        if let Some(idx) = self.recent.iter().position(|e| e.id == id) {
            let mut entry = self.recent.remove(idx).unwrap_or_else(|| SessionEntry {
                id: id.to_string(),
                path: path.clone(),
                started_at: chrono::Local::now().to_rfc3339(),
                message_count: 0,
                size_bytes: 0,
            });
            entry.path = path;
            self.recent.push_front(entry);
        } else {
            // Don't know this session yet — refresh from disk so we keep
            // the existing metadata if it exists.
            self.refresh();
            if !self.recent.iter().any(|e| e.id == id) {
                self.recent.push_front(SessionEntry {
                    id: id.to_string(),
                    path,
                    started_at: chrono::Local::now().to_rfc3339(),
                    message_count: 0,
                    size_bytes: 0,
                });
            }
        }
        while self.recent.len() > RECENT_SESSIONS_LIMIT {
            self.recent.pop_back();
        }
    }

    /// Resolve an id or prefix against the in-memory recent list.
    /// Falls back to a full disk scan if no recent match.
    pub fn resolve(&self, id_or_prefix: &str) -> Option<SessionEntry> {
        // Exact match in recent list.
        for e in &self.recent {
            if e.id == id_or_prefix {
                return Some(e.clone());
            }
        }
        // Prefix match in recent list (newest first already).
        for e in &self.recent {
            if e.id.starts_with(id_or_prefix) {
                return Some(e.clone());
            }
        }
        // Full disk fallback.
        match crate::session::session_index::resolve_session_id(id_or_prefix) {
            Ok(Some(path)) => {
                let id = path
                    .file_stem()
                    .and_then(|f| f.to_str())
                    .unwrap_or(id_or_prefix)
                    .trim_end_matches(".conv")
                    .to_string();
                Some(SessionEntry {
                    id,
                    path,
                    started_at: chrono::Local::now().to_rfc3339(),
                    message_count: 0,
                    size_bytes: 0,
                })
            }
            _ => None,
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::shared::test_util::EnvGuard;

    fn entry(id: &str, path: &str) -> SessionEntry {
        SessionEntry {
            id: id.to_string(),
            path: std::path::PathBuf::from(path),
            started_at: chrono::Local::now().to_rfc3339(),
            message_count: 1,
            size_bytes: 100,
        }
    }

    /// Set `KF_CODE_DATA_DIR` to an empty temporary directory for the
    /// duration of the test. Returns a guard that restores the env and
    /// drops the temp dir on Drop.
    struct DataDirGuard {
        _dir: tempfile::TempDir,
        _env: EnvGuard,
    }

    fn with_empty_data_dir() -> DataDirGuard {
        let dir = tempfile::tempdir().unwrap();
        let env = EnvGuard::set("KF_CODE_DATA_DIR", dir.path());
        DataDirGuard {
            _dir: dir,
            _env: env,
        }
    }

    #[test]
    fn touch_moves_entry_to_front() {
        let _guard = crate::session::test_data_dir_lock().blocking_lock();
        let _dir = with_empty_data_dir();

        let mut state = DaemonState::new();
        state.recent.push_back(entry("a", "/a"));
        state.recent.push_back(entry("b", "/b"));
        state.recent.push_back(entry("c", "/c"));

        state.touch("b", std::path::PathBuf::from("/b2"));
        assert_eq!(state.recent[0].id, "b");
        assert_eq!(state.recent[0].path, std::path::PathBuf::from("/b2"));
        assert_eq!(state.recent.len(), 3);
    }

    #[test]
    fn touch_adds_unknown_entry() {
        let _guard = crate::session::test_data_dir_lock().blocking_lock();
        let _dir = with_empty_data_dir();

        let mut state = DaemonState::new();
        state.recent.push_back(entry("a", "/a"));

        state.touch("x", std::path::PathBuf::from("/x"));
        assert_eq!(state.recent[0].id, "x");
        // Unknown entries refresh from disk, so the synthetic in-memory "a"
        // is replaced by whatever is on disk (nothing, in the empty temp dir).
        assert_eq!(state.recent.len(), 1);
    }

    #[test]
    fn check_auth_allows_when_no_token_configured() {
        let _guard = crate::session::test_data_dir_lock().blocking_lock();
        let _dir = with_empty_data_dir();
        let state = DaemonState::new();
        assert!(
            state.check_auth(None).is_ok(),
            "no token configured, None should pass"
        );
        assert!(
            state.check_auth(Some("anything")).is_ok(),
            "no token configured, any token should pass"
        );
    }

    #[test]
    fn check_auth_rejects_missing_or_wrong_token() {
        let _guard = crate::session::test_data_dir_lock().blocking_lock();
        let _dir = with_empty_data_dir();
        let mut state = DaemonState::new();
        state.expected_token = Some("secret-token".to_string());
        assert!(
            state.check_auth(None).is_err(),
            "missing token should be rejected"
        );
        assert!(
            state.check_auth(Some("wrong")).is_err(),
            "wrong token should be rejected"
        );
        assert!(
            state.check_auth(Some("secret-token")).is_ok(),
            "correct token should pass"
        );
    }

    #[test]
    fn recent_list_is_capped() {
        let _guard = crate::session::test_data_dir_lock().blocking_lock();
        let dir = with_empty_data_dir();
        let dir_path = dir._dir.path().to_path_buf();

        // Create real session files so the daemon's refresh sees them.
        let sessions_dir = dir_path.join("sessions");
        std::fs::create_dir_all(&sessions_dir).unwrap();
        // WO 38.12: replaced 10x thread::sleep(10ms) with explicit mtime
        // via set_modified. Filesystems round mtimes to whole seconds, so
        // use 2s steps for a guaranteed-distinct ordering. Open with write
        // access (Windows SetFileTime requires GENERIC_WRITE).
        let base = std::time::SystemTime::UNIX_EPOCH;
        for i in 0..10u64 {
            let path = sessions_dir.join(format!("s{i}.conv.ndjson"));
            std::fs::write(&path, "").unwrap();
            let mtime = base + std::time::Duration::from_secs(i * 2 + 100);
            let file = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
            file.set_modified(mtime).unwrap();
            drop(file);
        }

        let mut state = DaemonState::new();
        for i in 0..10 {
            let path = sessions_dir.join(format!("s{i}.conv.ndjson"));
            state.touch(&format!("s{i}"), path);
        }
        assert_eq!(state.recent.len(), RECENT_SESSIONS_LIMIT);
        assert_eq!(state.recent[0].id, "s9");
    }
}
