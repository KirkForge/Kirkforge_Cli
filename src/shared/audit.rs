//! Append-only JSONL audit log for destructive tool calls.
//!
//! Records one line per denied or successful destructive invocation
//! (`write_file`, `edit_file`, `bash`). Arguments are redacted before
//! serialization: literal file contents, `old_string`/`new_string`, and
//! arbitrary values from other tools are stripped or truncated.

use serde::{Deserialize, Serialize};
use std::fs::OpenOptions;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// A single audit-log entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AuditEntry {
    /// A destructive tool call (write_file, edit_file, bash).
    Tool {
        /// RFC 3339 UTC timestamp.
        timestamp: String,
        /// Tool name (e.g. `write_file`, `bash`).
        tool: String,
        /// Redacted tool arguments.
        args: serde_json::Value,
        /// Whether the tool completed successfully.
        success: bool,
        /// Reason the call was denied, if applicable.
        #[serde(skip_serializing_if = "Option::is_none")]
        denial_reason: Option<String>,
        /// Optional session identifier.
        #[serde(skip_serializing_if = "Option::is_none")]
        session_id: Option<String>,
    },
    /// A hook verdict — denial or fail-open failure (WO 11.6, ADR-061).
    Hook {
        /// RFC 3339 UTC timestamp.
        timestamp: String,
        /// The event name (e.g. `pre-tool-bash`, `post-turn`).
        event: String,
        /// The plugin name if it's a plugin hook, else `None` for built-in.
        #[serde(skip_serializing_if = "Option::is_none")]
        plugin: Option<String>,
        /// The verdict: `allow`, `deny`, or `allow_fail_open`.
        verdict: String,
        /// For `deny`: the reason. For `allow_fail_open`: the error.
        #[serde(skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
        /// Optional session identifier.
        #[serde(skip_serializing_if = "Option::is_none")]
        session_id: Option<String>,
    },
    /// A plugin tool invocation (H4).
    PluginTool {
        /// RFC 3339 UTC timestamp.
        timestamp: String,
        /// Plugin tool name.
        name: String,
        /// First 200 chars of the tool arguments.
        args_summary: String,
        /// Exit code of the subprocess, if available.
        #[serde(skip_serializing_if = "Option::is_none")]
        exit_code: Option<i32>,
        /// Wall-clock duration in milliseconds.
        duration_ms: u64,
    },
}

impl AuditEntry {
    /// Construct a `Tool` variant with a redacted args snapshot.
    pub fn tool(
        tool: &str,
        args: &serde_json::Value,
        success: bool,
        denial_reason: Option<&str>,
    ) -> Self {
        Self::Tool {
            timestamp: chrono::Utc::now().to_rfc3339(),
            tool: tool.to_string(),
            args: redact_args(tool, args),
            success,
            denial_reason: denial_reason.map(|s| s.to_string()),
            session_id: None,
        }
    }

    /// Construct a `Hook` variant recording a verdict (WO 11.6).
    pub fn hook(event: &str, plugin: Option<&str>, verdict: &str, reason: Option<&str>) -> Self {
        Self::Hook {
            timestamp: chrono::Utc::now().to_rfc3339(),
            event: event.to_string(),
            plugin: plugin.map(|s| s.to_string()),
            verdict: verdict.to_string(),
            reason: reason.map(|s| s.to_string()),
            session_id: None,
        }
    }

    /// Construct a `PluginTool` variant (H4).
    pub fn plugin_tool(
        name: &str,
        args_summary: &str,
        exit_code: Option<i32>,
        duration_ms: u64,
    ) -> Self {
        Self::PluginTool {
            timestamp: chrono::Utc::now().to_rfc3339(),
            name: name.to_string(),
            args_summary: args_summary.to_string(),
            exit_code,
            duration_ms,
        }
    }
}

/// Append-only audit log.
///
/// When constructed with `path = None`, logging is a no-op. This is the
/// safe fallback when the data directory cannot be determined.
pub struct AuditLog {
    path: Option<PathBuf>,
    writer: Mutex<Option<BufWriter<std::fs::File>>>,
}

impl AuditLog {
    /// Open (or create) the audit log at `path`.
    ///
    /// If `path` is `None`, every log call is silently dropped.
    pub fn new(path: Option<PathBuf>) -> Self {
        let writer = path.as_ref().and_then(|p| {
            if let Some(parent) = p.parent() {
                if let Err(e) = std::fs::create_dir_all(parent) {
                    tracing::warn!(
                        error = %e,
                        path = %p.display(),
                        "failed to create audit log directory; disabling audit log"
                    );
                    return None;
                }
            }
            match OpenOptions::new().append(true).create(true).open(p) {
                Ok(f) => Some(BufWriter::new(f)),
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        path = %p.display(),
                        "failed to open audit log; disabling audit log"
                    );
                    None
                }
            }
        });
        Self {
            path,
            writer: Mutex::new(writer),
        }
    }

    /// Return the configured log path, if any.
    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// Record a destructive tool call.
    ///
    /// `args` is redacted in-place according to [`redact_args`] before being
    /// serialized. The call is best-effort: I/O failures are logged but never
    /// surfaced to the model or user.
    pub fn log_destructive(
        &self,
        tool: &str,
        args: &serde_json::Value,
        success: bool,
        denial_reason: Option<&str>,
    ) {
        let entry = AuditEntry::tool(tool, args, success, denial_reason);
        self.write_entry(&entry);
    }

    /// Record a hook verdict (denial or fail-open failure). WO 11.6.
    ///
    /// Best-effort: I/O failures are logged but never surfaced.
    pub fn log_hook(&self, event: &str, plugin: Option<&str>, verdict: &str, reason: Option<&str>) {
        let entry = AuditEntry::hook(event, plugin, verdict, reason);
        self.write_entry(&entry);
    }

    /// Record a plugin tool invocation (H4). Best-effort.
    pub fn log_plugin_tool(
        &self,
        name: &str,
        args_summary: &str,
        exit_code: Option<i32>,
        duration_ms: u64,
    ) {
        let entry = AuditEntry::plugin_tool(name, args_summary, exit_code, duration_ms);
        self.write_entry(&entry);
    }

    /// Serialize and append a single entry line.
    fn write_entry(&self, entry: &AuditEntry) {
        let line = match serde_json::to_string(entry) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(error = %e, "failed to serialize audit entry");
                return;
            }
        };
        if let Ok(mut guard) = self.writer.lock() {
            if let Some(ref mut w) = *guard {
                if let Err(e) = writeln!(w, "{line}") {
                    tracing::warn!(error = %e, "failed to write audit entry");
                }
            }
        }
    }
}

impl Drop for AuditLog {
    fn drop(&mut self) {
        if let Ok(mut guard) = self.writer.lock() {
            if let Some(ref mut w) = *guard {
                if let Err(e) = w.flush() {
                    tracing::warn!(error = %e, "failed to flush audit log on drop");
                }
            }
        }
    }
}

// ── Hash-chained audit trail (WO 29.4 — port of @kirkforge/core-events) ────
//
// Tamper-evident audit log with SHA-256 (or HMAC-SHA256 when keyed) chain
// hashes. Mirrors the TS surface: AuditAction (29 literals), AuditOutcome,
// AuditEvent, initialHash, chainHashOf, MemoryAuditSink, FileAuditSink (with
// size-based rotation), AuditLogger, createAuditSink. Dead sinks (http,
// syslog, worm) are deliberately NOT ported — zero production consumers.

/// Classification of what an audit event records. Mirrors the 29-literal
/// `AuditAction` union in `@kirkforge/core-events`. Wire format is the
/// dotted string (`auth.success`, `policy.deny`, …) so audit logs written
/// by the TS implementation remain readable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AuditAction {
    #[serde(rename = "auth.success")]
    AuthSuccess,
    #[serde(rename = "auth.failure")]
    AuthFailure,
    #[serde(rename = "auth.token_refresh")]
    AuthTokenRefresh,
    #[serde(rename = "policy.check")]
    PolicyCheck,
    #[serde(rename = "policy.deny")]
    PolicyDeny,
    #[serde(rename = "policy.change")]
    PolicyChange,
    #[serde(rename = "tenant.create")]
    TenantCreate,
    #[serde(rename = "tenant.evict")]
    TenantEvict,
    #[serde(rename = "tenant.access")]
    TenantAccess,
    #[serde(rename = "verify.start")]
    VerifyStart,
    #[serde(rename = "verify.complete")]
    VerifyComplete,
    #[serde(rename = "correct.start")]
    CorrectStart,
    #[serde(rename = "correct.complete")]
    CorrectComplete,
    #[serde(rename = "observe.record")]
    ObserveRecord,
    #[serde(rename = "observe.recall")]
    ObserveRecall,
    #[serde(rename = "memory.read")]
    MemoryRead,
    #[serde(rename = "memory.write")]
    MemoryWrite,
    #[serde(rename = "memory.delete")]
    MemoryDelete,
    #[serde(rename = "secret.access")]
    SecretAccess,
    #[serde(rename = "secret.resolve")]
    SecretResolve,
    #[serde(rename = "config.change")]
    ConfigChange,
    #[serde(rename = "tool.invoke")]
    ToolInvoke,
    #[serde(rename = "tool.deny")]
    ToolDeny,
    #[serde(rename = "model.invoke")]
    ModelInvoke,
    #[serde(rename = "model.deny")]
    ModelDeny,
    #[serde(rename = "system.startup")]
    SystemStartup,
    #[serde(rename = "system.shutdown")]
    SystemShutdown,
    #[serde(rename = "serve.start")]
    ServeStart,
    #[serde(rename = "serve.shutdown")]
    ServeShutdown,
    #[serde(rename = "system.error")]
    SystemError,
}

impl AuditAction {
    /// The dotted wire string (`"auth.success"`, etc.). Same output the
    /// serde rename produces, exposed for hash-chain canonicalization.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::AuthSuccess => "auth.success",
            Self::AuthFailure => "auth.failure",
            Self::AuthTokenRefresh => "auth.token_refresh",
            Self::PolicyCheck => "policy.check",
            Self::PolicyDeny => "policy.deny",
            Self::PolicyChange => "policy.change",
            Self::TenantCreate => "tenant.create",
            Self::TenantEvict => "tenant.evict",
            Self::TenantAccess => "tenant.access",
            Self::VerifyStart => "verify.start",
            Self::VerifyComplete => "verify.complete",
            Self::CorrectStart => "correct.start",
            Self::CorrectComplete => "correct.complete",
            Self::ObserveRecord => "observe.record",
            Self::ObserveRecall => "observe.recall",
            Self::MemoryRead => "memory.read",
            Self::MemoryWrite => "memory.write",
            Self::MemoryDelete => "memory.delete",
            Self::SecretAccess => "secret.access",
            Self::SecretResolve => "secret.resolve",
            Self::ConfigChange => "config.change",
            Self::ToolInvoke => "tool.invoke",
            Self::ToolDeny => "tool.deny",
            Self::ModelInvoke => "model.invoke",
            Self::ModelDeny => "model.deny",
            Self::SystemStartup => "system.startup",
            Self::SystemShutdown => "system.shutdown",
            Self::ServeStart => "serve.start",
            Self::ServeShutdown => "serve.shutdown",
            Self::SystemError => "system.error",
        }
    }
}

/// Outcome of the audited action. Mirrors `AuditOutcome` in core-events.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AuditOutcome {
    #[serde(rename = "success")]
    Success,
    #[serde(rename = "deny")]
    Deny,
    #[serde(rename = "error")]
    Error,
    #[serde(rename = "skipped")]
    Skipped,
}

impl AuditOutcome {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::Deny => "deny",
            Self::Error => "error",
            Self::Skipped => "skipped",
        }
    }
}

/// A single tamper-evident audit record. `chain_hash` binds this event to
/// the previous one (or to [`initial_hash`] for the first event), so any
/// after-the-fact edit to a recorded field breaks verification.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditEvent {
    pub id: String,
    pub sequence: u64,
    /// RFC 3339 / ISO 8601 UTC timestamp.
    pub timestamp: String,
    pub action: AuditAction,
    pub outcome: AuditOutcome,
    pub actor_id: String,
    pub tenant_id: String,
    pub reason: String,
    /// SHA-256 (or HMAC-SHA256) chain hash — assigned by the sink.
    pub chain_hash: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub policy_hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trace_id: Option<String>,
    /// Additional context. Keys are recursively sorted before hashing so
    /// key-reorder does not break the chain.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
}

/// Genesis hash for an empty chain. With `hmac_key`, uses HMAC-SHA256 so
/// anyone without the key cannot recompute a valid chain; otherwise plain
/// SHA-256 (relies on WORM/append-only storage for tamper-evidence).
pub fn initial_hash(hmac_key: Option<&str>) -> String {
    use hmac::Mac;
    const GENESIS: &[u8] = b"kirkforge-audit-genesis";
    match hmac_key {
        Some(key) => {
            let mut mac = <hmac::Hmac<sha2::Sha256> as hmac::Mac>::new_from_slice(key.as_bytes())
                .expect("HMAC accepts any key length");
            mac.update(GENESIS);
            hex::encode(mac.finalize().into_bytes())
        }
        None => {
            use sha2::Digest;
            let mut h = sha2::Sha256::new();
            h.update(GENESIS);
            hex::encode(h.finalize())
        }
    }
}

/// Compute the next chain hash over `prev_hash || event fields || metadata`.
///
/// The payload format is fixed to match the TS implementation byte-for-byte:
/// `prev|action|outcome|actor|tenant|reason|timestamp|sequence|metadataJson`.
/// `metadataJson` is the recursive-key-sorted canonical form, so reordering
/// nested metadata keys does not change the hash but editing any value does.
pub fn chain_hash_of(prev_hash: &str, event: &AuditEvent, hmac_key: Option<&str>) -> String {
    use hmac::Mac;
    // null/None metadata both canonicalize to "{}" — matches TS
    // `event.metadata ?? {}` (nullish coalescing converts both null and
    // undefined to the empty object).
    let metadata_json = match &event.metadata {
        Some(v) if !v.is_null() => canonical_json(v, 0),
        _ => "{}".to_string(),
    };
    let payload = format!(
        "{}|{}|{}|{}|{}|{}|{}|{}|{}",
        prev_hash,
        event.action.as_str(),
        event.outcome.as_str(),
        event.actor_id,
        event.tenant_id,
        event.reason,
        event.timestamp,
        event.sequence,
        metadata_json,
    );
    match hmac_key {
        Some(key) => {
            let mut mac = <hmac::Hmac<sha2::Sha256> as hmac::Mac>::new_from_slice(key.as_bytes())
                .expect("HMAC accepts any key length");
            mac.update(payload.as_bytes());
            hex::encode(mac.finalize().into_bytes())
        }
        None => {
            use sha2::Digest;
            let mut h = sha2::Sha256::new();
            h.update(payload.as_bytes());
            hex::encode(h.finalize())
        }
    }
}

/// Recursive key-sorted canonical JSON. Mirrors the TS `canonicalJson`:
/// object keys are sorted at every depth, arrays preserve order, the
/// depth is capped at 32 to guard against stack overflow on pathological
/// nesting.
fn canonical_json(value: &serde_json::Value, depth: u32) -> String {
    if depth > 32 {
        return "\"<max-depth>\"".to_string();
    }
    match value {
        serde_json::Value::Null => "null".to_string(),
        serde_json::Value::Bool(b) => b.to_string(),
        serde_json::Value::Number(n) => n.to_string(),
        serde_json::Value::String(s) => serde_json::to_string(s).unwrap_or_else(|_| "null".into()),
        serde_json::Value::Array(arr) => {
            let body: Vec<String> = arr.iter().map(|v| canonical_json(v, depth + 1)).collect();
            format!("[{}]", body.join(","))
        }
        serde_json::Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            let body: Vec<String> = keys
                .into_iter()
                .map(|k| {
                    let key_str = serde_json::to_string(k).unwrap_or_else(|_| "\"\"".into());
                    format!("{}:{}", key_str, canonical_json(&map[k], depth + 1))
                })
                .collect();
            format!("{{{}}}", body.join(","))
        }
    }
}

/// Parameters for [`AuditLogger::record`]. Mirrors the TS `record` arg.
#[derive(Debug, Clone, Default)]
pub struct AuditRecord<'a> {
    pub action: Option<AuditAction>,
    pub outcome: Option<AuditOutcome>,
    pub actor_id: &'a str,
    pub tenant_id: &'a str,
    pub reason: &'a str,
    pub policy_hash: Option<&'a str>,
    pub trace_id: Option<&'a str>,
    pub metadata: Option<serde_json::Value>,
}

/// In-memory audit sink — for tests and short-lived dev runs.
/// Stores each event with its computed chain hash; `verify_chain` replays
/// the chain to detect any after-the-fact tamper.
pub struct MemoryAuditSink {
    events: Mutex<Vec<AuditEvent>>,
    last_hash: Mutex<String>,
    hmac_key: Option<String>,
}

impl MemoryAuditSink {
    pub fn new(hmac_key: Option<&str>) -> Self {
        Self {
            events: Mutex::new(Vec::new()),
            last_hash: Mutex::new(initial_hash(hmac_key)),
            hmac_key: hmac_key.map(|s| s.to_string()),
        }
    }

    pub fn write(&self, event: AuditEvent) -> bool {
        let mut last = self.last_hash.lock().expect("audit sink mutex poisoned");
        let chain = chain_hash_of(&last, &event, self.hmac_key.as_deref());
        let mut sealed = event;
        sealed.chain_hash = chain.clone();
        *last = chain;
        drop(last);
        self.events
            .lock()
            .expect("audit sink mutex poisoned")
            .push(sealed);
        true
    }

    pub fn flush(&self) -> bool {
        true
    }

    pub fn close(&self) {}

    pub fn get_events(&self) -> Vec<AuditEvent> {
        self.events
            .lock()
            .expect("audit sink mutex poisoned")
            .clone()
    }

    /// Replay the chain from genesis; true iff every event's recorded
    /// `chain_hash` matches a fresh computation.
    pub fn verify_chain(&self) -> bool {
        let events = self.events.lock().expect("audit sink mutex poisoned");
        let mut prev = initial_hash(self.hmac_key.as_deref());
        for event in events.iter() {
            let expected = chain_hash_of(&prev, event, self.hmac_key.as_deref());
            if event.chain_hash != expected {
                return false;
            }
            prev = event.chain_hash.clone();
        }
        true
    }
}

/// File-backed audit sink with size-based rotation.
///
/// When the active log exceeds `max_file_size_bytes`, the current file is
/// renamed to `<path>.1` (and existing `.N` shift to `.N+1`); a new empty
/// file is then started. Each rotated file carries a complete hash chain
/// from genesis to its last event.
pub struct FileAuditSink {
    file_path: PathBuf,
    buffer: Mutex<Vec<AuditEvent>>,
    flush_size: usize,
    last_hash: Mutex<String>,
    max_file_size_bytes: u64,
    max_rotated_files: u32,
    hmac_key: Option<String>,
}

impl FileAuditSink {
    pub fn new(
        file_path: PathBuf,
        hmac_key: Option<&str>,
        max_file_size_bytes: u64,
        max_rotated_files: u32,
        flush_size: usize,
    ) -> Self {
        if let Some(parent) = file_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        Self {
            file_path,
            buffer: Mutex::new(Vec::new()),
            flush_size,
            last_hash: Mutex::new(initial_hash(hmac_key)),
            max_file_size_bytes,
            max_rotated_files,
            hmac_key: hmac_key.map(|s| s.to_string()),
        }
    }

    pub fn write(&self, event: AuditEvent) -> bool {
        let mut buf = self.buffer.lock().expect("audit sink mutex poisoned");
        buf.push(event);
        if buf.len() >= self.flush_size {
            drop(buf);
            return self.flush();
        }
        true
    }

    pub fn flush(&self) -> bool {
        let mut buf = self.buffer.lock().expect("audit sink mutex poisoned");
        if buf.is_empty() {
            return true;
        }
        let mut last = self.last_hash.lock().expect("audit sink mutex poisoned");
        let key = self.hmac_key.as_deref();
        // Rotate before writing if the active file is already over the cap.
        self.rotate();
        let mut lines: Vec<String> = Vec::with_capacity(buf.len());
        for event in buf.iter() {
            let chain = chain_hash_of(&last, event, key);
            let mut sealed = event.clone();
            sealed.chain_hash = chain.clone();
            *last = chain;
            match serde_json::to_string(&sealed) {
                Ok(s) => lines.push(s),
                Err(_) => return false,
            }
        }
        drop(last);
        let content = lines.join("\n") + "\n";
        let result = std::fs::OpenOptions::new()
            .append(true)
            .create(true)
            .open(&self.file_path)
            .and_then(|mut f| f.write_all(content.as_bytes()));
        if result.is_err() {
            return false;
        }
        buf.clear();
        true
    }

    pub fn close(&self) {
        let _ = self.flush();
    }

    /// If the active file exceeds `max_file_size_bytes`, shift existing
    /// rotated files `.N → .N+1` and rename the active file to `.1`.
    /// Rotation failures are swallowed (not fatal — we keep appending to
    /// the current file, matching the TS behavior).
    fn rotate(&self) {
        let Ok(stats) = std::fs::metadata(&self.file_path) else {
            return;
        };
        if stats.len() < self.max_file_size_bytes {
            return;
        }
        // Shift .N → .N+1 from highest down so we don't clobber.
        let mut i = self.max_rotated_files.saturating_sub(1);
        while i >= 1 {
            let rotated = rotated_path(&self.file_path, i);
            let next = rotated_path(&self.file_path, i + 1);
            if rotated.exists() {
                let _ = std::fs::rename(&rotated, &next);
            }
            if i == 1 {
                break;
            }
            i -= 1;
        }
        let _ = std::fs::rename(&self.file_path, rotated_path(&self.file_path, 1));
    }
}

fn rotated_path(base: &Path, n: u32) -> PathBuf {
    let mut s = base.as_os_str().to_owned();
    s.push(format!(".{n}"));
    PathBuf::from(s)
}

/// Type of audit sink for [`create_audit_sink`]. Only `Memory` and `File`
/// are LIVE; http/syslog are intentionally absent (R4 — dead sinks).
#[derive(Debug, Clone)]
pub enum AuditSinkKind {
    Memory,
    File { file_path: PathBuf },
}

/// Factory matching the TS `createAuditSink({ type })` for the LIVE surface.
/// Returns `Err` for unknown sink kinds — matches the TS `throw`.
pub fn create_audit_sink(kind: AuditSinkKind) -> std::result::Result<AuditSink, String> {
    Ok(match kind {
        AuditSinkKind::Memory => AuditSink::Memory(MemoryAuditSink::new(None)),
        AuditSinkKind::File { file_path } => AuditSink::File(FileAuditSink::new(
            file_path,
            None,
            50 * 1024 * 1024,
            10,
            100,
        )),
    })
}

/// Owned enum of sink instances. `AuditLogger` accepts this so callers
/// can swap sinks (tests use `Memory`, production uses `File`).
pub enum AuditSink {
    Memory(MemoryAuditSink),
    File(FileAuditSink),
}

impl AuditSink {
    pub fn name(&self) -> &'static str {
        match self {
            Self::Memory(_) => "memory",
            Self::File(_) => "file",
        }
    }

    pub fn write(&self, event: AuditEvent) -> bool {
        match self {
            Self::Memory(s) => s.write(event),
            Self::File(s) => s.write(event),
        }
    }

    pub fn flush(&self) -> bool {
        match self {
            Self::Memory(s) => s.flush(),
            Self::File(s) => s.flush(),
        }
    }

    pub fn close(&self) {
        match self {
            Self::Memory(s) => s.close(),
            Self::File(s) => s.close(),
        }
    }
}

/// Top-level audit recorder. Wraps an [`AuditSink`] and assigns sequence +
/// id to each event. Mirrors the TS `AuditLogger` class.
pub struct AuditLogger {
    sink: AuditSink,
    sequence: Mutex<u64>,
}

impl AuditLogger {
    pub fn new(sink: AuditSink) -> Self {
        Self {
            sink,
            sequence: Mutex::new(0),
        }
    }

    /// Record an audit event. Returns `true` if the sink accepted it.
    pub fn record(&self, params: AuditRecord<'_>) -> bool {
        let mut seq = self.sequence.lock().expect("audit logger mutex poisoned");
        let assigned_sequence = *seq;
        let id = format!(
            "audit-{}-{}",
            chrono::Utc::now().timestamp_millis(),
            assigned_sequence
        );
        *seq += 1;
        drop(seq);
        let action = params.action.unwrap_or(AuditAction::SystemError);
        let outcome = params.outcome.unwrap_or(AuditOutcome::Success);
        let event = AuditEvent {
            id,
            sequence: assigned_sequence,
            timestamp: chrono::Utc::now().to_rfc3339(),
            action,
            outcome,
            actor_id: params.actor_id.to_string(),
            tenant_id: params.tenant_id.to_string(),
            reason: params.reason.to_string(),
            chain_hash: String::new(),
            policy_hash: params.policy_hash.map(|s| s.to_string()),
            trace_id: params.trace_id.map(|s| s.to_string()),
            metadata: params.metadata,
        };
        self.sink.write(event)
    }

    pub fn flush(&self) -> bool {
        self.sink.flush()
    }

    pub fn close(&self) {
        self.sink.close();
    }
}

/// Redact sensitive values from tool arguments before they reach the log.
///
/// Policy:
/// * `content`, `old_string`, `new_string` are dropped entirely.
/// * For `bash`, `command` is kept but truncated to 1 KiB.
/// * For file tools, `path` is kept.
/// * For all other keys the value is replaced with `""` so the shape of the
///   call is still visible without leaking secrets.
fn redact_args(tool: &str, args: &serde_json::Value) -> serde_json::Value {
    let Some(obj) = args.as_object() else {
        return serde_json::Value::Null;
    };
    let mut out = serde_json::Map::with_capacity(obj.len());
    for (key, value) in obj {
        match key.as_str() {
            "content" | "old_string" | "new_string" => continue,
            "command" if tool == "bash" => {
                let cmd = value.as_str().unwrap_or("");
                out.insert(
                    key.clone(),
                    serde_json::Value::String(truncate_string(cmd, 1024)),
                );
            }
            "path" => {
                if let Some(s) = value.as_str() {
                    out.insert(key.clone(), serde_json::Value::String(s.to_string()));
                }
            }
            _ => {
                out.insert(key.clone(), serde_json::Value::String(String::new()));
            }
        }
    }
    serde_json::Value::Object(out)
}

fn truncate_string(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    // Slice only at a character boundary; a naive byte slice can split a
    // multi-byte UTF-8 sequence and panic.
    let idx = s
        .char_indices()
        .take_while(|(i, _)| *i <= max)
        .last()
        .map(|(i, _)| i)
        .unwrap_or(0);
    format!("{}...[truncated]", &s[..idx])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_audit_log_appends_json_lines() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "kf_code_audit_lines_test_{}_{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("audit.ndjson");

        let log = AuditLog::new(Some(path.clone()));
        let args = serde_json::json!({"path": "/tmp/out.txt", "content": "SECRET"});
        log.log_destructive("write_file", &args, true, None);
        log.log_destructive("write_file", &args, false, Some("outside sandbox"));
        // Ensure buffered writes land on disk before reading.
        drop(log);

        let contents = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = contents.trim().split('\n').collect();
        assert_eq!(lines.len(), 2, "expected two JSON lines, got: {contents}");
        for line in &lines {
            let entry: AuditEntry = serde_json::from_str(line).unwrap();
            let AuditEntry::Tool { tool, args, .. } = entry else {
                panic!("expected Tool variant, got {entry:?}");
            };
            assert_eq!(tool, "write_file");
            assert!(args.get("content").is_none(), "content must be redacted");
            assert_eq!(
                args.get("path").and_then(|v| v.as_str()),
                Some("/tmp/out.txt")
            );
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_audit_log_redacts_bash_command() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "kf_code_audit_bash_test_{}_{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("audit.ndjson");

        let log = AuditLog::new(Some(path.clone()));
        let long_cmd = "echo ".to_string() + &"x".repeat(2048);
        let args = serde_json::json!({"command": long_cmd});
        log.log_destructive("bash", &args, true, None);
        drop(log);

        let contents = std::fs::read_to_string(&path).unwrap();
        let entry: AuditEntry = serde_json::from_str(contents.trim()).unwrap();
        let AuditEntry::Tool { args, .. } = entry else {
            panic!("expected Tool variant, got {entry:?}");
        };
        let logged_cmd = args.get("command").and_then(|v| v.as_str()).unwrap();
        assert!(logged_cmd.len() <= 1100, "bash command should be truncated");
        assert!(logged_cmd.ends_with("...[truncated]"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_audit_log_records_hook_verdict() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "kf_code_audit_hook_test_{}_{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("audit.ndjson");

        let log = AuditLog::new(Some(path.clone()));
        log.log_hook("pre-tool-bash", Some("sec-plugin"), "deny", Some("blocked"));
        log.log_hook(
            "pre-tool-bash",
            None,
            "allow_fail_open",
            Some("hook timed out"),
        );
        drop(log);

        let contents = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = contents.trim().split('\n').collect();
        assert_eq!(lines.len(), 2, "expected two hook entries, got: {contents}");
        let e0: AuditEntry = serde_json::from_str(lines[0]).unwrap();
        match e0 {
            AuditEntry::Hook {
                event,
                plugin,
                verdict,
                reason,
                ..
            } => {
                assert_eq!(event, "pre-tool-bash");
                assert_eq!(plugin.as_deref(), Some("sec-plugin"));
                assert_eq!(verdict, "deny");
                assert_eq!(reason.as_deref(), Some("blocked"));
            }
            other => panic!("expected Hook variant, got {other:?}"),
        }
        let e1: AuditEntry = serde_json::from_str(lines[1]).unwrap();
        match e1 {
            AuditEntry::Hook {
                event,
                plugin,
                verdict,
                reason,
                ..
            } => {
                assert_eq!(event, "pre-tool-bash");
                assert_eq!(plugin, None);
                assert_eq!(verdict, "allow_fail_open");
                assert_eq!(reason.as_deref(), Some("hook timed out"));
            }
            other => panic!("expected Hook variant, got {other:?}"),
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_truncate_string_respects_utf8_boundaries() {
        // Use a 2-byte UTF-8 character so the 1024-byte boundary falls in the
        // middle of a character. The old byte-slice implementation would panic.
        let two_byte = "é";
        let long_cmd = two_byte.repeat(600);
        let truncated = truncate_string(&long_cmd, 1024);
        assert!(truncated.ends_with("...[truncated]"));
        assert!(
            truncated.len() <= 1024 + "...[truncated]".len(),
            "truncated command should not exceed max plus marker: {truncated}"
        );
        assert!(
            truncated.is_char_boundary(truncated.len() - "...[truncated]".len()),
            "truncate point must be on a character boundary"
        );
    }

    // ── WO 29.4 hash-chain + sink ports ───────────────────────────────────

    use super::{
        canonical_json, chain_hash_of, create_audit_sink, initial_hash, AuditAction, AuditEvent,
        AuditLogger, AuditOutcome, AuditRecord, AuditSinkKind, FileAuditSink, MemoryAuditSink,
    };

    fn base_event() -> AuditEvent {
        AuditEvent {
            id: "evt-1".into(),
            sequence: 1,
            timestamp: "2026-01-01T00:00:00Z".into(),
            action: AuditAction::PolicyDeny,
            outcome: AuditOutcome::Deny,
            actor_id: "user1".into(),
            tenant_id: "t1".into(),
            reason: "Tool not allowed".into(),
            chain_hash: String::new(),
            policy_hash: None,
            trace_id: None,
            metadata: None,
        }
    }

    #[test]
    fn memory_sink_stores_and_retrieves_events() {
        let sink = MemoryAuditSink::new(None);
        assert!(sink.write(base_event()));
        let mut e2 = base_event();
        e2.id = "test-2".into();
        e2.sequence = 2;
        e2.action = AuditAction::PolicyDeny;
        e2.outcome = AuditOutcome::Deny;
        e2.actor_id = "user2".into();
        e2.tenant_id = "t2".into();
        e2.reason = "Tool not allowed".into();
        assert!(sink.write(e2));
        let events = sink.get_events();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].action, AuditAction::PolicyDeny);
        assert_eq!(events[1].action, AuditAction::PolicyDeny);
    }

    #[test]
    fn memory_sink_computes_chain_hashes() {
        let sink = MemoryAuditSink::new(None);
        assert!(sink.write(base_event()));
        let events = sink.get_events();
        assert!(!events[0].chain_hash.is_empty());
    }

    #[test]
    fn memory_sink_verifies_chain_integrity() {
        let sink = MemoryAuditSink::new(None);
        for i in 0..10 {
            let mut e = base_event();
            e.id = format!("test-{i}");
            e.sequence = i as u64 + 1;
            e.action = AuditAction::VerifyStart;
            sink.write(e);
        }
        assert!(sink.verify_chain());
    }

    #[test]
    fn memory_sink_detects_tampered_chain() {
        let sink = MemoryAuditSink::new(None);
        sink.write(base_event());
        // Tamper in-place — verify_chain re-derives and must mismatch.
        sink.events
            .lock()
            .unwrap()
            .first_mut()
            .expect("event present")
            .chain_hash = "tampered".into();
        assert!(!sink.verify_chain());
    }

    #[test]
    fn memory_sink_flush_and_close_succeed() {
        let sink = MemoryAuditSink::new(None);
        assert!(sink.write(base_event()));
        assert!(sink.flush());
        sink.close();
    }

    #[test]
    fn audit_logger_records_events_through_sink() {
        let sink = MemoryAuditSink::new(None);
        let logger = AuditLogger::new(super::AuditSink::Memory(sink));
        assert!(logger.record(AuditRecord {
            action: Some(AuditAction::AuthSuccess),
            outcome: Some(AuditOutcome::Success),
            actor_id: "user1",
            tenant_id: "t1",
            reason: "API key auth",
            ..Default::default()
        }));
        assert!(logger.record(AuditRecord {
            action: Some(AuditAction::PolicyDeny),
            outcome: Some(AuditOutcome::Deny),
            actor_id: "user2",
            tenant_id: "t2",
            reason: "Tool 'curl' not allowed",
            policy_hash: Some("abc123"),
            ..Default::default()
        }));
        assert!(logger.flush());
        let events = match &logger.sink {
            super::AuditSink::Memory(m) => m.get_events(),
            _ => unreachable!(),
        };
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].action, AuditAction::AuthSuccess);
        assert_eq!(events[1].action, AuditAction::PolicyDeny);
        assert_eq!(events[1].policy_hash.as_deref(), Some("abc123"));
    }

    #[test]
    fn audit_logger_includes_trace_id() {
        let sink = MemoryAuditSink::new(None);
        let logger = AuditLogger::new(super::AuditSink::Memory(sink));
        assert!(logger.record(AuditRecord {
            action: Some(AuditAction::VerifyStart),
            outcome: Some(AuditOutcome::Success),
            actor_id: "user1",
            tenant_id: "t1",
            reason: "verification started",
            trace_id: Some("trace-123"),
            ..Default::default()
        }));
        let events = match &logger.sink {
            super::AuditSink::Memory(m) => m.get_events(),
            _ => unreachable!(),
        };
        assert_eq!(events[0].trace_id.as_deref(), Some("trace-123"));
    }

    // ── chainHashOf regression: tamper detection ──────────────────────────

    #[test]
    fn changing_outcome_breaks_chain() {
        let original = chain_hash_of("prev", &base_event(), None);
        let mut tampered = base_event();
        tampered.outcome = AuditOutcome::Success;
        let tampered_hash = chain_hash_of("prev", &tampered, None);
        assert_ne!(original, tampered_hash);
    }

    #[test]
    fn changing_reason_breaks_chain() {
        let original = chain_hash_of("prev", &base_event(), None);
        let mut tampered = base_event();
        tampered.reason = "Approved by admin".into();
        assert_ne!(original, chain_hash_of("prev", &tampered, None));
    }

    #[test]
    fn changing_nested_metadata_breaks_chain() {
        let mut with_meta = base_event();
        with_meta.metadata = Some(serde_json::json!({
            "ctx": { "ip": "10.0.0.1", "path": "/verify" }
        }));
        let original = chain_hash_of("prev", &with_meta, None);
        let mut tampered = with_meta.clone();
        tampered.metadata = Some(serde_json::json!({
            "ctx": { "ip": "10.0.0.2", "path": "/verify" }
        }));
        assert_ne!(original, chain_hash_of("prev", &tampered, None));
    }

    #[test]
    fn reordering_nested_metadata_keys_does_not_break_chain() {
        let mut e = base_event();
        e.metadata = Some(serde_json::json!({ "b": 2, "a": 1 }));
        let h1 = chain_hash_of("prev", &e, None);
        e.metadata = Some(serde_json::json!({ "a": 1, "b": 2 }));
        let h2 = chain_hash_of("prev", &e, None);
        assert_eq!(h1, h2);
    }

    #[test]
    fn deeply_nested_metadata_is_included_in_hash() {
        let mut with_deep = base_event();
        with_deep.metadata = Some(serde_json::json!({
            "level1": { "level2": { "level3": "secret" } }
        }));
        let original = chain_hash_of("prev", &with_deep, None);
        let mut tampered = with_deep.clone();
        tampered.metadata = Some(serde_json::json!({
            "level1": { "level2": { "level3": "tampered" } }
        }));
        assert_ne!(original, chain_hash_of("prev", &tampered, None));
    }

    #[test]
    fn null_vs_absent_metadata_produces_consistent_hash() {
        let mut with_null = base_event();
        with_null.metadata = Some(serde_json::Value::Null);
        let h_null = chain_hash_of("prev", &with_null, None);
        let h_absent = chain_hash_of("prev", &base_event(), None);
        assert_eq!(h_null, h_absent);
    }

    // ── HMAC-keyed audit chain ────────────────────────────────────────────

    #[test]
    fn hmac_key_produces_different_hash_than_plain() {
        let plain = chain_hash_of(&initial_hash(None), &base_event(), None);
        let keyed_seed = initial_hash(Some("my-secret-key"));
        let keyed = chain_hash_of(&keyed_seed, &base_event(), Some("my-secret-key"));
        assert_ne!(plain, keyed);
        assert_eq!(plain.len(), 64, "SHA-256 hex is 64 chars");
        assert_eq!(keyed.len(), 64, "HMAC-SHA256 hex is 64 chars");
    }

    #[test]
    fn hmac_key_produces_different_genesis() {
        let plain = initial_hash(None);
        let keyed = initial_hash(Some("test-key"));
        assert_ne!(plain, keyed);
        assert_eq!(plain.len(), 64);
        assert_eq!(keyed.len(), 64);
    }

    #[test]
    fn memory_sink_uses_hmac_key_for_chain_integrity() {
        let sink = MemoryAuditSink::new(Some("test-hmac-key"));
        let mut e = base_event();
        e.action = AuditAction::AuthSuccess;
        e.outcome = AuditOutcome::Success;
        sink.write(e);
        let events = sink.get_events();
        assert!(!events[0].chain_hash.is_empty());
        assert!(sink.verify_chain());
    }

    // ── createAuditSink factory ───────────────────────────────────────────

    #[test]
    fn create_sink_memory() {
        let sink = create_audit_sink(AuditSinkKind::Memory).expect("memory sink");
        assert_eq!(sink.name(), "memory");
    }

    #[test]
    fn create_sink_file() {
        let dir = std::env::temp_dir().join(format!(
            "kf_audit_factory_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let path = dir.join("audit.jsonl");
        let sink = create_audit_sink(AuditSinkKind::File {
            file_path: path.clone(),
        })
        .expect("file sink");
        assert_eq!(sink.name(), "file");
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ── FileAuditSink + rotation ──────────────────────────────────────────

    use std::sync::atomic::{AtomicUsize, Ordering};

    static FILE_SINK_COUNTER: AtomicUsize = AtomicUsize::new(0);

    fn fresh_audit_dir(label: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "kf_code_audit_{label}_{}_{}_{}",
            std::process::id(),
            FILE_SINK_COUNTER.fetch_add(1, Ordering::SeqCst),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn file_sink_writes_sealed_events_with_chain_hashes() {
        let dir = fresh_audit_dir("chain");
        let path = dir.join("audit.jsonl");
        let sink = FileAuditSink::new(path.clone(), None, 50 * 1024 * 1024, 10, 1);
        assert!(sink.write(base_event()));
        let mut e2 = base_event();
        e2.id = "evt-2".into();
        e2.sequence = 2;
        e2.action = AuditAction::VerifyComplete;
        sink.write(e2);
        sink.close();
        let contents = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = contents.trim().split('\n').collect();
        assert_eq!(lines.len(), 2);
        let first: AuditEvent = serde_json::from_str(lines[0]).unwrap();
        let second: AuditEvent = serde_json::from_str(lines[1]).unwrap();
        assert!(!first.chain_hash.is_empty());
        assert!(!second.chain_hash.is_empty());
        assert_ne!(first.chain_hash, second.chain_hash);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn file_sink_rotates_when_file_exceeds_max_size() {
        let dir = fresh_audit_dir("rotate");
        let path = dir.join("audit.jsonl");
        // Tiny cap forces rotation between flushes.
        let sink = FileAuditSink::new(path.clone(), None, 80, 3, 1);
        for i in 0..6 {
            let mut e = base_event();
            e.id = format!("evt-{i}");
            e.sequence = i as u64;
            e.reason = format!("event number {i} with enough payload to exceed the tiny cap");
            sink.write(e);
        }
        sink.close();
        // Active file plus at least one rotated (.1).
        assert!(path.exists(), "active file must exist");
        let rotated1 = rotated_path(&path, 1);
        assert!(
            rotated1.exists(),
            "expected .1 rotation, dir contents: {:?}",
            std::fs::read_dir(&dir).unwrap().collect::<Vec<_>>()
        );
        // Each rotated file must contain at least one valid sealed event.
        let rotated_contents = std::fs::read_to_string(&rotated1).unwrap();
        let first_line = rotated_contents.trim().lines().next().unwrap();
        let _: AuditEvent = serde_json::from_str(first_line).expect("rotated line parses");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn file_sink_respects_max_rotated_files() {
        let dir = fresh_audit_dir("max");
        let path = dir.join("audit.jsonl");
        // Cap of 1 byte + max 2 rotated files → after many writes only .1 and .2 survive.
        let sink = FileAuditSink::new(path.clone(), None, 1, 2, 1);
        for i in 0..10 {
            let mut e = base_event();
            e.id = format!("evt-{i}");
            e.sequence = i as u64;
            sink.write(e);
        }
        sink.close();
        assert!(rotated_path(&path, 1).exists());
        assert!(rotated_path(&path, 2).exists());
        assert!(
            !rotated_path(&path, 3).exists(),
            ".3 must not exist — max_rotated_files=2"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn canonical_json_sorts_keys_at_every_depth() {
        let v = serde_json::json!({ "b": { "y": 2, "x": 1 }, "a": [3, 2, 1] });
        let out = canonical_json(&v, 0);
        assert_eq!(
            out, r#"{"a":[3,2,1],"b":{"x":1,"y":2}}"#,
            "object keys sort, array order preserves"
        );
    }
}
