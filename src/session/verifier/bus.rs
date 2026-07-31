//! Unified verifier bus — collects verdicts from all registered verifiers
//! after a tool call and provides structured feedback to the executor.
//!
//! ADR-043: the KVB (KirkForge Verification Bus) unifies the existing
//! verifier systems (security, lint, build, git, test, plugin) behind
//! a single `VerifierBus` struct. The executor queries the bus after
//! file-modifying tool calls; error verdicts are injected into the
//! conversation so the model sees them immediately.

use kirkforge_plugin_host::PluginVerifier;
use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Command;

/// Which verifier produced this finding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VerifierSource {
    Plugin(String),
    Git,
    Build,
    Test,
    Lint,
    Security,
    Rustfmt,
    Custom(String),
}

impl std::fmt::Display for VerifierSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            VerifierSource::Plugin(name) => write!(f, "plugin:{name}"),
            VerifierSource::Git => write!(f, "git"),
            VerifierSource::Build => write!(f, "build"),
            VerifierSource::Test => write!(f, "test"),
            VerifierSource::Lint => write!(f, "lint"),
            VerifierSource::Security => write!(f, "security"),
            VerifierSource::Rustfmt => write!(f, "rustfmt"),
            VerifierSource::Custom(name) => write!(f, "custom:{name}"),
        }
    }
}

/// Finding severity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Info,
    Warning,
    Error,
}

impl std::fmt::Display for Severity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Severity::Info => write!(f, "info"),
            Severity::Warning => write!(f, "warning"),
            Severity::Error => write!(f, "error"),
        }
    }
}

/// A single finding from one verifier.
#[derive(Debug, Clone)]
pub struct VerdictEntry {
    pub source: VerifierSource,
    pub severity: Severity,
    pub message: String,
    pub file: Option<PathBuf>,
    pub line: Option<u32>,
}

/// Context for a verification run.
#[derive(Debug, Clone)]
pub struct VerifyContext {
    pub sandbox_dir: PathBuf,
    pub changed_files: Vec<PathBuf>,
}

/// The unified verifier bus. Verifiers register here, and the
/// executor queries verdicts after each tool call.
pub struct VerifierBus {
    verdicts: Vec<VerdictEntry>,
    verifiers: Vec<Box<dyn BusVerifier>>,
}

/// Trait for bus-aware verifiers. Unlike the event-driven `Verifier`
/// trait (which operates on `BusEvent`), `BusVerifier` receives a
/// `VerifyContext` with the changed files and returns structured
/// `VerdictEntry`s.
pub trait BusVerifier: Send + Sync {
    fn name(&self) -> &str;
    fn verify(&self, ctx: &VerifyContext) -> Vec<VerdictEntry>;
}

impl VerifierBus {
    pub fn new() -> Self {
        Self {
            verdicts: Vec::new(),
            verifiers: Vec::new(),
        }
    }

    pub fn register(&mut self, verifier: Box<dyn BusVerifier>) -> anyhow::Result<()> {
        let name = verifier.name().to_string();
        if self.verifiers.iter().any(|v| v.name() == name) {
            anyhow::bail!("Verifier '{name}' is already registered on the bus");
        }
        self.verifiers.push(verifier);
        Ok(())
    }

    /// Register a plugin-declared verifier. `plugin_root` is the plugin
    /// directory and `command` is the verifier command path (resolved
    /// relative to `plugin_root`, as declared in the manifest). The
    /// verifier runs via the same env-cleared subprocess path as the host
    /// `PluginVerifier` (exit 0 = pass, non-zero = fail with stderr as the
    /// message), with `plugin_root` as the subprocess cwd. Results are
    /// tagged `VerifierSource::Plugin(name)`.
    pub fn add_plugin_verifier(
        &mut self,
        name: String,
        priority: u8,
        plugin_root: PathBuf,
        command: PathBuf,
    ) -> anyhow::Result<()> {
        if self.verifiers.iter().any(|v| v.name() == name) {
            anyhow::bail!("Verifier '{name}' is already registered on the bus");
        }
        let verifier = PluginBusVerifier {
            inner: PluginVerifier {
                name,
                command,
                plugin_root,
            },
            priority,
        };
        self.verifiers.push(Box::new(verifier));
        Ok(())
    }

    /// Run all registered verifiers against the given context.
    /// Collects all verdicts (does not short-circuit on first error).
    pub fn run(&mut self, ctx: &VerifyContext) {
        self.verdicts.clear();
        for verifier in &self.verifiers {
            let entries = verifier.verify(ctx);
            self.verdicts.extend(entries);
        }
    }

    /// All verdicts from the last run.
    pub fn verdicts(&self) -> &[VerdictEntry] {
        &self.verdicts
    }

    /// Whether any verdict has severity Error.
    pub fn has_errors(&self) -> bool {
        self.verdicts.iter().any(|v| v.severity == Severity::Error)
    }

    /// Clear all collected verdicts.
    pub fn clear(&mut self) {
        self.verdicts.clear();
    }

    /// Drop registered verifiers whose `name()` is NOT in `keep`. Used by
    /// live plugin reload to prune old plugin verifiers while keeping the
    /// built-in bus verifiers. ADR-028.
    pub fn retain_verifiers<F>(&mut self, keep: F)
    where
        F: Fn(&str) -> bool,
    {
        self.verifiers.retain(|v| keep(v.name()));
    }

    /// Number of registered verifiers (built-in + plugin).
    pub fn verifier_count(&self) -> usize {
        self.verifiers.len()
    }
}

/// Format the last run's verdicts as a human-readable table (WO 11.7).
///
/// Columns: `Verifier | Source | Verdict | File:Line | Message`.
/// Grouped by verifier name. Empty verdicts → a "no verdicts" line.
pub fn format_verdict_report(verdicts: &[VerdictEntry]) -> String {
    if verdicts.is_empty() {
        return "No verifier verdicts from the last turn.".to_string();
    }
    let mut lines = vec![
        format!(
            "{:<15} {:<18} {:<8} {:<24} {}",
            "Verifier", "Source", "Verdict", "File:Line", "Message"
        ),
        "-".repeat(80),
    ];
    for v in verdicts {
        let file_line = match (&v.file, v.line) {
            (Some(f), Some(l)) => format!("{}:{}", f.display(), l),
            (Some(f), None) => f.display().to_string(),
            _ => "—".to_string(),
        };
        let file_line_display = if file_line.len() > 24 {
            // Walk back to the nearest UTF-8 char boundary so a multi-byte
            // char at byte 22 (e.g. `café.txt`) doesn't panic the slice.
            let mut boundary = 23;
            while !file_line.is_char_boundary(boundary) {
                boundary -= 1;
            }
            format!("{}…", &file_line[..boundary])
        } else {
            file_line
        };
        lines.push(format!(
            "{:<15} {:<18} {:<8} {:<24} {}",
            v.source,
            v.source.to_string(),
            v.severity,
            file_line_display,
            v.message.chars().take(60).collect::<String>(),
        ));
    }
    // Summary line.
    let pass = verdicts
        .iter()
        .filter(|v| v.severity != Severity::Error)
        .count();
    let fail = verdicts.len() - pass;
    lines.push(format!(
        "\n{} verdict(s): {} pass, {} fail",
        verdicts.len(),
        pass,
        fail
    ));
    lines.join("\n")
}

impl Default for VerifierBus {
    fn default() -> Self {
        Self::new()
    }
}

// ── Built-in bus verifier adapters ──────────────────────────────────────
//
// These adapters are stubs that register on the bus. The existing
// event-driven verifier system (VerifierHandler + CorrectionLoop)
// already handles async verification via BusEvents. The bus collects
// structured findings from BusVerifiers that don't need async I/O.
// Async verifiers continue to operate through the event bus.
//
// Future work: migrate the async verifiers to implement BusVerifier
// once the bus supports async verification.

/// Adapter: security verifier stub on the bus.
///
/// The full async security verifier runs via the event bus. This stub
/// registers on the bus so it's counted in `verifier_bus.verifiers()`
/// and can be extended later.
pub struct SecurityBusVerifier;

impl BusVerifier for SecurityBusVerifier {
    fn name(&self) -> &str {
        "security"
    }

    fn verify(&self, _ctx: &VerifyContext) -> Vec<VerdictEntry> {
        Vec::new()
    }
}

/// Adapter: git verifier stub on the bus.
pub struct GitBusVerifier;

impl BusVerifier for GitBusVerifier {
    fn name(&self) -> &str {
        "git"
    }

    fn verify(&self, _ctx: &VerifyContext) -> Vec<VerdictEntry> {
        Vec::new()
    }
}

/// Adapter: a plugin-declared verifier on the bus.
///
/// Wraps the host crate's `PluginVerifier`, which spawns the verifier
/// command with a curated (env-cleared) environment: exit 0 means pass,
/// any non-zero exit fails with stderr as the message. This is the same
/// subprocess convention used by `PluginToolWrapper` for plugin tools.
/// ADR-028: plugin verifiers register into the unified bus rather than
/// only the old event-driven `Verifier` trait path.
pub struct PluginBusVerifier {
    inner: PluginVerifier,
    priority: u8,
}

impl BusVerifier for PluginBusVerifier {
    fn name(&self) -> &str {
        &self.inner.name
    }

    fn verify(&self, ctx: &VerifyContext) -> Vec<VerdictEntry> {
        // `ceiling:` env-var contract divergence (bucketlist 3.30). This
        // bus path passes `KF_CHANGED_FILES` (newline-separated list from
        // `VerifyContext`), while the legacy event-driven path
        // (`PluginVerifierAdapter` in `plugin.rs`) passes `KF_EVENT_KIND`
        // + `KF_EVENT_JSON` (the full serialized `BusEvent`). The two
        // paths intentionally serve different shapes: the bus verifier is
        // sync and context-based (a file list), the event-driven verifier
        // is async and event-based (the full event payload). Unifying the
        // env-var contract would change the behaviour plugin verifier
        // scripts depend on; the divergence is documented, not closed.
        let mut env = HashMap::new();
        env.insert("KF_VERIFIER_NAME".to_string(), self.inner.name.clone());
        env.insert(
            "KF_CHANGED_FILES".to_string(),
            ctx.changed_files
                .iter()
                .map(|p| p.display().to_string())
                .collect::<Vec<_>>()
                .join("\n"),
        );
        let _ = self.priority;
        match self.inner.run(&env) {
            Ok(kirkforge_plugin_host::VerifierVerdict::Pass) => Vec::new(),
            Ok(kirkforge_plugin_host::VerifierVerdict::Fail { message }) => {
                vec![VerdictEntry {
                    source: VerifierSource::Plugin(self.inner.name.clone()),
                    severity: Severity::Error,
                    message,
                    file: ctx.changed_files.first().cloned(),
                    line: None,
                }]
            }
            Err(e) => vec![VerdictEntry {
                source: VerifierSource::Plugin(self.inner.name.clone()),
                severity: Severity::Error,
                message: format!("plugin verifier execution failed: {e}"),
                file: None,
                line: None,
            }],
        }
    }
}

/// Build a VerifierBus with all built-in verifiers registered.
pub fn default_verifier_bus() -> VerifierBus {
    let mut bus = VerifierBus::new();
    bus.register(Box::new(SecurityBusVerifier))
        .expect("built-in verifier names are unique");
    bus.register(Box::new(GitBusVerifier))
        .expect("built-in verifier names are unique");
    bus
}

// ── WO 10.8: TS orchestrator NDJSON bridge ─────────────────────────────
//
// ADR-028 §5: the cross-language NDJSON wire bridge. The
// `TsOrchestratorBridgeVerifier` implements `BusVerifier` by shelling
// out to the TS orchestrator's bridge emitter (a Node script that runs
// the orchestrator's security/lint/types/graph/imports emitters on the
// changed files and writes one JSON object per line to stdout). Each
// NDJSON line is parsed into a `VerdictEntry`. Malformed lines become
// `Severity::Warning` verdicts (never silently dropped).

/// NDJSON wire format: one JSON object per line.
///
/// ```jsonc
/// {"verifier": "security", "severity": "error", "file": "src/foo.rs",
///  "line": 42, "message": "eval() call detected", "rule": "no-eval"}
/// ```
///
/// The `verifier` field maps to `VerifierSource::Custom(name)`; the
/// `severity` field is case-insensitive ("error"/"warning"/"info");
/// `file` and `line` are optional; `rule` is appended to the message
/// when present. Unknown fields are ignored (forward-compatible).
#[derive(serde::Deserialize)]
struct NdjsonVerdict {
    verifier: String,
    severity: String,
    message: String,
    #[serde(default)]
    file: Option<String>,
    #[serde(default)]
    line: Option<u32>,
    #[serde(default)]
    rule: Option<String>,
}

/// Parse the severity string from the NDJSON verdict. Unknown values
/// default to `Warning` (the caller should not silently drop a verdict
/// it cannot classify).
fn parse_severity(s: &str) -> Severity {
    match s.to_ascii_lowercase().as_str() {
        "error" | "critical" | "high" => Severity::Error,
        "warning" | "medium" => Severity::Warning,
        "info" | "low" => Severity::Info,
        _ => Severity::Warning,
    }
}

/// A `BusVerifier` that shells out to the TS orchestrator's bridge
/// emitter and parses NDJSON verdicts from stdout. The bridge command
/// is a Node script (or any executable) that accepts the changed files
/// as arguments (or via the `KF_CHANGED_FILES` env var, like
/// `PluginBusVerifier`) and writes one JSON verdict per line to stdout.
///
/// The verifier is registered on the bus only when the TS orchestrator
/// plugin is loaded (the executor setup gates this). Malformed NDJSON
/// lines produce `Severity::Warning` verdicts with a descriptive
/// message; they are never silently dropped (ADR-028 §5).
pub struct TsOrchestratorBridgeVerifier {
    /// Display name for the bridge (used in `name()` and
    /// `VerifierSource::Custom`).
    name: String,
    /// The command to run (resolved relative to `plugin_root`).
    command: PathBuf,
    /// The plugin root directory (cwd of the subprocess).
    plugin_root: PathBuf,
}

impl TsOrchestratorBridgeVerifier {
    pub fn new(name: String, command: PathBuf, plugin_root: PathBuf) -> Self {
        Self {
            name,
            command,
            plugin_root,
        }
    }

    /// Run the bridge command and capture stdout. The changed files are
    /// passed via the `KF_CHANGED_FILES` env var (newline-separated,
    /// matching `PluginBusVerifier`) and as command-line arguments.
    fn run_bridge(&self, ctx: &VerifyContext) -> Result<String, String> {
        let cmd_path = self.plugin_root.join(&self.command);
        if !cmd_path.exists() {
            return Err(format!("bridge command not found: {}", cmd_path.display()));
        }

        let mut env = HashMap::new();
        env.insert("KF_VERIFIER_NAME".to_string(), self.name.clone());
        env.insert(
            "KF_CHANGED_FILES".to_string(),
            ctx.changed_files
                .iter()
                .map(|p| p.display().to_string())
                .collect::<Vec<_>>()
                .join("\n"),
        );

        let mut attempts = 0;
        let output = loop {
            let mut cmd = Command::new(&cmd_path);
            cmd.env_clear();
            for (k, v) in kirkforge_plugin_host::env::curated_env(&env) {
                cmd.env(k, v);
            }
            cmd.current_dir(&self.plugin_root);
            // Pass changed files as args too (the bridge script may
            // prefer argv over env).
            for f in &ctx.changed_files {
                cmd.arg(f);
            }
            match cmd.output() {
                Err(e) if e.kind() == std::io::ErrorKind::ExecutableFileBusy && attempts < 3 => {
                    std::thread::sleep(std::time::Duration::from_millis(10));
                    attempts += 1;
                    continue;
                }
                other => break other.map_err(|e| format!("bridge execution failed: {e}"))?,
            }
        };

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(format!(
                "bridge exited with {:?}: {}",
                output.status.code(),
                stderr.trim()
            ));
        }
        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    }

    /// Parse NDJSON stdout into `VerdictEntry`s. Malformed lines become
    /// `Severity::Warning` verdicts so the operator sees the bridge is
    /// producing bad output (never silently dropped).
    fn parse_ndjson(&self, stdout: &str, ctx: &VerifyContext) -> Vec<VerdictEntry> {
        let mut entries = Vec::new();
        for (lineno, line) in stdout.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            match serde_json::from_str::<NdjsonVerdict>(line) {
                Ok(v) => {
                    let message = if let Some(rule) = &v.rule {
                        format!("[{rule}] {}", v.message)
                    } else {
                        v.message
                    };
                    entries.push(VerdictEntry {
                        source: VerifierSource::Custom(format!("ts:{}", v.verifier)),
                        severity: parse_severity(&v.severity),
                        message,
                        file: v.file.as_ref().map(PathBuf::from),
                        line: v.line,
                    });
                }
                Err(e) => {
                    entries.push(VerdictEntry {
                        source: VerifierSource::Custom(format!("ts:{}", self.name)),
                        severity: Severity::Warning,
                        message: format!(
                            "malformed NDJSON verdict on line {}: {e} — raw: {line}",
                            lineno + 1
                        ),
                        file: ctx.changed_files.first().cloned(),
                        line: None,
                    });
                }
            }
        }
        entries
    }
}

impl BusVerifier for TsOrchestratorBridgeVerifier {
    fn name(&self) -> &str {
        &self.name
    }

    fn verify(&self, ctx: &VerifyContext) -> Vec<VerdictEntry> {
        match self.run_bridge(ctx) {
            Ok(stdout) => self.parse_ndjson(&stdout, ctx),
            Err(e) => vec![VerdictEntry {
                source: VerifierSource::Custom(format!("ts:{}", self.name)),
                severity: Severity::Warning,
                message: format!("TS orchestrator bridge failed: {e}"),
                file: None,
                line: None,
            }],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct StubVerifier {
        name: String,
        entries: Vec<VerdictEntry>,
    }

    impl BusVerifier for StubVerifier {
        fn name(&self) -> &str {
            &self.name
        }
        fn verify(&self, _ctx: &VerifyContext) -> Vec<VerdictEntry> {
            self.entries.clone()
        }
    }

    fn make_ctx() -> VerifyContext {
        VerifyContext {
            sandbox_dir: PathBuf::from("/tmp/test"),
            changed_files: vec![PathBuf::from("src/lib.rs")],
        }
    }

    #[test]
    fn verifier_bus_register_and_run() {
        let mut bus = VerifierBus::new();
        bus.register(Box::new(StubVerifier {
            name: "stub_a".into(),
            entries: vec![VerdictEntry {
                source: VerifierSource::Build,
                severity: Severity::Info,
                message: "clean build".into(),
                file: Some(PathBuf::from("src/lib.rs")),
                line: None,
            }],
        }))
        .unwrap();
        bus.register(Box::new(StubVerifier {
            name: "stub_b".into(),
            entries: vec![VerdictEntry {
                source: VerifierSource::Git,
                severity: Severity::Warning,
                message: "dirty worktree".into(),
                file: None,
                line: None,
            }],
        }))
        .unwrap();

        bus.run(&make_ctx());
        assert_eq!(
            bus.verdicts().len(),
            2,
            "should collect verdicts from both stubs"
        );
    }

    #[test]
    fn verifier_bus_has_errors() {
        let mut bus = VerifierBus::new();
        bus.register(Box::new(StubVerifier {
            name: "error_stub".into(),
            entries: vec![VerdictEntry {
                source: VerifierSource::Security,
                severity: Severity::Error,
                message: "secret detected".into(),
                file: Some(PathBuf::from("src/config.rs")),
                line: Some(42),
            }],
        }))
        .unwrap();

        bus.run(&make_ctx());
        assert!(bus.has_errors(), "should detect error verdicts");
    }

    #[test]
    fn verifier_bus_no_errors_when_clean() {
        let mut bus = VerifierBus::new();
        bus.register(Box::new(StubVerifier {
            name: "clean_stub".into(),
            entries: vec![VerdictEntry {
                source: VerifierSource::Lint,
                severity: Severity::Info,
                message: "no issues".into(),
                file: None,
                line: None,
            }],
        }))
        .unwrap();

        bus.run(&make_ctx());
        assert!(
            !bus.has_errors(),
            "no error verdicts → has_errors() is false"
        );
    }

    #[test]
    fn verifier_bus_clear() {
        let mut bus = VerifierBus::new();
        bus.register(Box::new(StubVerifier {
            name: "stub".into(),
            entries: vec![VerdictEntry {
                source: VerifierSource::Test,
                severity: Severity::Error,
                message: "test failed".into(),
                file: Some(PathBuf::from("src/lib.rs")),
                line: None,
            }],
        }))
        .unwrap();

        bus.run(&make_ctx());
        assert!(!bus.verdicts().is_empty());
        bus.clear();
        assert!(bus.verdicts().is_empty(), "clear() should empty verdicts");
    }

    #[test]
    fn verify_context_changed_files() {
        let ctx = VerifyContext {
            sandbox_dir: PathBuf::from("/tmp/project"),
            changed_files: vec![PathBuf::from("src/main.rs"), PathBuf::from("src/lib.rs")],
        };
        assert_eq!(ctx.changed_files.len(), 2);
        assert_eq!(ctx.sandbox_dir, PathBuf::from("/tmp/project"));
    }

    #[test]
    fn verdict_source_display() {
        assert_eq!(VerifierSource::Git.to_string(), "git");
        assert_eq!(VerifierSource::Build.to_string(), "build");
        assert_eq!(
            VerifierSource::Plugin("my_plugin".into()).to_string(),
            "plugin:my_plugin"
        );
        assert_eq!(
            VerifierSource::Custom("lsp".into()).to_string(),
            "custom:lsp"
        );
    }

    #[test]
    fn severity_display() {
        assert_eq!(Severity::Info.to_string(), "info");
        assert_eq!(Severity::Warning.to_string(), "warning");
        assert_eq!(Severity::Error.to_string(), "error");
    }

    #[cfg(unix)]
    fn make_pass_script(dir: &std::path::Path) -> PathBuf {
        let script = dir.join("pass.sh");
        std::fs::write(&script, "#!/bin/sh\nexit 0\n").unwrap();
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&script).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&script, perms).unwrap();
        script
    }

    #[cfg(unix)]
    fn make_fail_script(dir: &std::path::Path, body: &str) -> PathBuf {
        let script = dir.join("fail.sh");
        std::fs::write(&script, format!("#!/bin/sh\n{body}")).unwrap();
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&script).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&script, perms).unwrap();
        script
    }

    #[cfg(unix)]
    #[test]
    fn add_plugin_verifier_pass_yields_no_verdicts() {
        let tmp = tempfile::tempdir().unwrap();
        let _ = make_pass_script(tmp.path());
        let mut bus = VerifierBus::new();
        bus.add_plugin_verifier(
            "pass_v".into(),
            5,
            tmp.path().to_path_buf(),
            PathBuf::from("pass.sh"),
        )
        .unwrap();
        bus.run(&make_ctx());
        assert!(
            bus.verdicts().is_empty(),
            "passing verifier adds no verdicts"
        );
        assert!(!bus.has_errors());
    }

    #[cfg(unix)]
    #[test]
    fn add_plugin_verifier_fail_yields_error_verdict() {
        let tmp = tempfile::tempdir().unwrap();
        let _ = make_fail_script(tmp.path(), "echo 'bad pattern' >&2\nexit 1\n");
        let mut bus = VerifierBus::new();
        bus.add_plugin_verifier(
            "fail_v".into(),
            5,
            tmp.path().to_path_buf(),
            PathBuf::from("fail.sh"),
        )
        .unwrap();
        bus.run(&make_ctx());
        assert_eq!(bus.verdicts().len(), 1);
        let v = &bus.verdicts()[0];
        assert_eq!(v.source, VerifierSource::Plugin("fail_v".into()));
        assert_eq!(v.severity, Severity::Error);
        assert!(v.message.contains("bad pattern"));
        assert!(bus.has_errors());
    }

    #[test]
    fn add_plugin_verifier_missing_command_yields_error_verdict() {
        let mut bus = VerifierBus::new();
        bus.add_plugin_verifier(
            "ghost".into(),
            1,
            PathBuf::from("/nonexistent"),
            PathBuf::from("does-not-exist.sh"),
        )
        .unwrap();
        bus.run(&make_ctx());
        assert_eq!(bus.verdicts().len(), 1);
        assert_eq!(bus.verdicts()[0].severity, Severity::Error);
        assert!(bus.has_errors());
    }

    // ── WO 10.8: TsOrchestratorBridgeVerifier tests ──

    #[cfg(unix)]
    fn make_bridge_script(dir: &std::path::Path, body: &str) -> PathBuf {
        let script = dir.join("bridge.sh");
        std::fs::write(&script, format!("#!/bin/sh\n{body}")).unwrap();
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&script).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&script, perms).unwrap();
        script
    }

    /// WO 10.8: a mock TS orchestrator bridge emits one `security`
    /// error verdict via NDJSON → the bridge verifier produces a
    /// `VerdictEntry` with `Severity::Error`.
    #[cfg(unix)]
    #[test]
    fn ts_orchestrator_bridge_verifier() {
        let tmp = tempfile::tempdir().unwrap();
        let _ = make_bridge_script(
            tmp.path(),
            "echo '{\"verifier\":\"security\",\"severity\":\"error\",\"file\":\"src/secret.rs\",\"line\":42,\"message\":\"eval() call detected\",\"rule\":\"no-eval\"}'\nexit 0\n",
        );
        let mut bus = VerifierBus::new();
        bus.register(Box::new(TsOrchestratorBridgeVerifier::new(
            "ts-bridge".into(),
            PathBuf::from("bridge.sh"),
            tmp.path().to_path_buf(),
        )))
        .unwrap();

        bus.run(&make_ctx());
        assert_eq!(bus.verdicts().len(), 1, "one NDJSON line → one verdict");
        let v = &bus.verdicts()[0];
        assert_eq!(
            v.severity,
            Severity::Error,
            "severity field 'error' → Severity::Error"
        );
        assert!(
            v.message.contains("eval() call detected"),
            "message should carry the NDJSON message: {}",
            v.message
        );
        assert!(
            v.message.contains("[no-eval]"),
            "rule field should be prefixed to the message: {}",
            v.message
        );
        assert_eq!(
            v.file,
            Some(PathBuf::from("src/secret.rs")),
            "file field should map to VerdictEntry.file"
        );
        assert_eq!(
            v.line,
            Some(42),
            "line field should map to VerdictEntry.line"
        );
        assert!(
            matches!(v.source, VerifierSource::Custom(ref s) if s == "ts:security"),
            "verifier field should map to VerifierSource::Custom(\"ts:security\"): {:?}",
            v.source
        );
        assert!(bus.has_errors(), "an Error verdict → has_errors()");
    }

    /// WO 10.8: a bridge that emits multiple NDJSON lines produces
    /// multiple verdicts (one per line). Empty lines are skipped.
    #[cfg(unix)]
    #[test]
    fn ts_orchestrator_bridge_verifier_multiple_lines() {
        let tmp = tempfile::tempdir().unwrap();
        let _ = make_bridge_script(
            tmp.path(),
            "echo '{\"verifier\":\"lint\",\"severity\":\"warning\",\"message\":\"unused import\"}'\necho ''\necho '{\"verifier\":\"types\",\"severity\":\"error\",\"message\":\"type mismatch\"}'\nexit 0\n",
        );
        let mut bus = VerifierBus::new();
        bus.register(Box::new(TsOrchestratorBridgeVerifier::new(
            "ts-bridge".into(),
            PathBuf::from("bridge.sh"),
            tmp.path().to_path_buf(),
        )))
        .unwrap();

        bus.run(&make_ctx());
        assert_eq!(
            bus.verdicts().len(),
            2,
            "two non-empty NDJSON lines → two verdicts (empty line skipped)"
        );
        assert_eq!(bus.verdicts()[0].severity, Severity::Warning);
        assert_eq!(bus.verdicts()[1].severity, Severity::Error);
    }

    /// WO 10.8: malformed NDJSON lines become `Severity::Warning`
    /// verdicts (never silently dropped).
    #[cfg(unix)]
    #[test]
    fn ts_orchestrator_bridge_verifier_malformed_ndjson_becomes_warning() {
        let tmp = tempfile::tempdir().unwrap();
        let _ = make_bridge_script(
            tmp.path(),
            "echo 'this is not json'\necho '{\"verifier\":\"security\",\"severity\":\"error\",\"message\":\"real finding\"}'\nexit 0\n",
        );
        let mut bus = VerifierBus::new();
        bus.register(Box::new(TsOrchestratorBridgeVerifier::new(
            "ts-bridge".into(),
            PathBuf::from("bridge.sh"),
            tmp.path().to_path_buf(),
        )))
        .unwrap();

        bus.run(&make_ctx());
        assert_eq!(bus.verdicts().len(), 2, "malformed + valid → 2 verdicts");
        assert_eq!(
            bus.verdicts()[0].severity,
            Severity::Warning,
            "malformed line → Warning verdict"
        );
        assert!(
            bus.verdicts()[0].message.contains("malformed NDJSON"),
            "malformed verdict message should explain the issue: {}",
            bus.verdicts()[0].message
        );
        assert_eq!(bus.verdicts()[1].severity, Severity::Error);
    }

    /// WO 10.8: a bridge command that fails (non-zero exit) produces a
    /// single `Severity::Warning` verdict with the error.
    #[cfg(unix)]
    #[test]
    fn ts_orchestrator_bridge_verifier_command_failure_yields_warning() {
        let tmp = tempfile::tempdir().unwrap();
        let _ = make_bridge_script(tmp.path(), "echo 'bridge crashed' >&2\nexit 1\n");
        let mut bus = VerifierBus::new();
        bus.register(Box::new(TsOrchestratorBridgeVerifier::new(
            "ts-bridge".into(),
            PathBuf::from("bridge.sh"),
            tmp.path().to_path_buf(),
        )))
        .unwrap();

        bus.run(&make_ctx());
        assert_eq!(bus.verdicts().len(), 1);
        assert_eq!(bus.verdicts()[0].severity, Severity::Warning);
        assert!(
            bus.verdicts()[0].message.contains("bridge crashed"),
            "failure verdict should carry stderr: {}",
            bus.verdicts()[0].message
        );
        assert!(!bus.has_errors(), "a Warning is not an Error");
    }

    /// WO 10.8: severity string parsing covers the TS emitter
    /// severities (critical/high → Error, medium → Warning, low → Info).
    #[test]
    fn parse_severity_maps_ts_emitter_levels() {
        assert_eq!(parse_severity("critical"), Severity::Error);
        assert_eq!(parse_severity("high"), Severity::Error);
        assert_eq!(parse_severity("error"), Severity::Error);
        assert_eq!(parse_severity("medium"), Severity::Warning);
        assert_eq!(parse_severity("warning"), Severity::Warning);
        assert_eq!(parse_severity("low"), Severity::Info);
        assert_eq!(parse_severity("info"), Severity::Info);
        assert_eq!(
            parse_severity("unknown"),
            Severity::Warning,
            "unknown severity defaults to Warning (not dropped)"
        );
    }

    #[test]
    fn parse_severity_is_case_insensitive() {
        assert_eq!(parse_severity("ERROR"), Severity::Error);
        assert_eq!(parse_severity("Error"), Severity::Error);
        assert_eq!(parse_severity("WARNING"), Severity::Warning);
        assert_eq!(parse_severity("INFO"), Severity::Info);
        assert_eq!(parse_severity("High"), Severity::Error);
    }

    #[test]
    fn format_verdict_report_empty_says_no_verdicts() {
        let report = format_verdict_report(&[]);
        assert!(report.contains("No verifier verdicts"));
        assert!(
            !report.contains("Verifier"),
            "empty case has no table header"
        );
    }

    #[test]
    fn format_verdict_report_renders_header_row() {
        let verdicts = vec![VerdictEntry {
            source: VerifierSource::Build,
            severity: Severity::Info,
            message: "ok".into(),
            file: None,
            line: None,
        }];
        let report = format_verdict_report(&verdicts);
        assert!(report.contains("Verifier"));
        assert!(report.contains("Source"));
        assert!(report.contains("Verdict"));
        assert!(report.contains("File:Line"));
        assert!(report.contains("Message"));
    }

    #[test]
    fn format_verdict_report_lists_verdict_entries() {
        let verdicts = vec![
            VerdictEntry {
                source: VerifierSource::Build,
                severity: Severity::Error,
                message: "build failed".into(),
                file: Some(PathBuf::from("src/lib.rs")),
                line: Some(42),
            },
            VerdictEntry {
                source: VerifierSource::Lint,
                severity: Severity::Warning,
                message: "unused import".into(),
                file: Some(PathBuf::from("src/main.rs")),
                line: None,
            },
        ];
        let report = format_verdict_report(&verdicts);
        assert!(report.contains("build failed"));
        assert!(report.contains("unused import"));
        assert!(report.contains("src/lib.rs:42"));
        assert!(report.contains("src/main.rs"));
        assert!(report.contains("2 verdict(s):"));
        assert!(report.contains("1 pass, 1 fail"));
    }

    #[test]
    fn format_verdict_report_shows_dash_when_file_and_line_absent() {
        let verdicts = vec![VerdictEntry {
            source: VerifierSource::Security,
            severity: Severity::Info,
            message: "ok".into(),
            file: None,
            line: None,
        }];
        let report = format_verdict_report(&verdicts);
        assert!(
            report.contains('—'),
            "no file/line should render as em-dash"
        );
        assert!(report.contains("1 verdict(s):"));
        assert!(report.contains("1 pass, 0 fail"));
    }

    #[test]
    fn format_verdict_report_truncates_long_file_paths() {
        let long_path = PathBuf::from(format!("src/{}/mod.rs", "x".repeat(40)));
        let verdicts = vec![VerdictEntry {
            source: VerifierSource::Build,
            severity: Severity::Error,
            message: "err".into(),
            file: Some(long_path),
            line: Some(1),
        }];
        let report = format_verdict_report(&verdicts);
        assert!(
            report.contains('…'),
            "long file:line should be truncated with ellipsis"
        );
    }

    /// WO 15.8 (2.5): a path with a multi-byte UTF-8 char at byte index 22
    /// (e.g. `café.txt`) must not panic the `&file_line[..23]` slice.
    #[test]
    fn format_verdict_report_truncates_multibyte_path_without_panicking() {
        // `café` — é is 2 bytes, so a path landing é at the truncation
        // boundary exercises the char-boundary walk-back.
        let path = PathBuf::from(format!("src/{}é.txt", "a".repeat(18)));
        let verdicts = vec![VerdictEntry {
            source: VerifierSource::Build,
            severity: Severity::Error,
            message: "err".into(),
            file: Some(path),
            line: None,
        }];
        let report = format_verdict_report(&verdicts);
        assert!(
            report.contains('…'),
            "long multi-byte path should be truncated with ellipsis"
        );
    }

    #[test]
    fn format_verdict_report_truncates_long_messages_to_60_chars() {
        let long_message = "x".repeat(200);
        let verdicts = vec![VerdictEntry {
            source: VerifierSource::Lint,
            severity: Severity::Warning,
            message: long_message.clone(),
            file: None,
            line: None,
        }];
        let report = format_verdict_report(&verdicts);
        assert!(
            !report.contains(&long_message),
            "the full 200-char message must not appear in the report"
        );
        let row = report
            .lines()
            .find(|l| l.contains("lint"))
            .unwrap_or_else(|| panic!("no lint row in:\n{report}"));
        assert!(
            row.contains(&"x".repeat(60)),
            "row should include the truncated 60-char message: {row}"
        );
    }

    #[test]
    fn format_verdict_report_summary_counts_error_as_fail() {
        let verdicts = vec![
            VerdictEntry {
                source: VerifierSource::Build,
                severity: Severity::Error,
                message: "fail".into(),
                file: None,
                line: None,
            },
            VerdictEntry {
                source: VerifierSource::Lint,
                severity: Severity::Warning,
                message: "warn".into(),
                file: None,
                line: None,
            },
            VerdictEntry {
                source: VerifierSource::Security,
                severity: Severity::Info,
                message: "ok".into(),
                file: None,
                line: None,
            },
        ];
        let report = format_verdict_report(&verdicts);
        assert!(report.contains("3 verdict(s):"));
        assert!(report.contains("2 pass, 1 fail"));
    }

    #[test]
    fn default_verifier_bus_registers_two_built_in_verifiers() {
        let bus = default_verifier_bus();
        assert_eq!(bus.verifier_count(), 2);
    }

    #[test]
    fn default_verifier_bus_verifies_without_errors_on_empty_changed_files() {
        let mut bus = default_verifier_bus();
        let ctx = VerifyContext {
            sandbox_dir: PathBuf::from("/tmp"),
            changed_files: vec![],
        };
        bus.run(&ctx);
        assert!(!bus.has_errors(), "built-in stubs emit no verdicts");
        assert!(bus.verdicts().is_empty());
    }

    #[test]
    fn verifier_bus_rejects_duplicate_register() {
        let mut bus = VerifierBus::new();
        bus.register(Box::new(StubVerifier {
            name: "dup".into(),
            entries: vec![],
        }))
        .unwrap();
        let err = bus
            .register(Box::new(StubVerifier {
                name: "dup".into(),
                entries: vec![],
            }))
            .unwrap_err();
        assert!(
            err.to_string().contains("already registered"),
            "duplicate register should bail: {err}"
        );
        assert_eq!(bus.verifier_count(), 1, "second register was rejected");
    }

    #[test]
    fn verifier_bus_rejects_duplicate_add_plugin_verifier() {
        let mut bus = VerifierBus::new();
        bus.add_plugin_verifier(
            "plug_v".into(),
            1,
            PathBuf::from("/tmp"),
            PathBuf::from("x.sh"),
        )
        .unwrap();
        let err = bus
            .add_plugin_verifier(
                "plug_v".into(),
                2,
                PathBuf::from("/tmp"),
                PathBuf::from("y.sh"),
            )
            .unwrap_err();
        assert!(
            err.to_string().contains("already registered"),
            "duplicate add_plugin_verifier should bail: {err}"
        );
        assert_eq!(bus.verifier_count(), 1);
    }

    #[test]
    fn verifier_bus_retain_drops_verifiers_not_matching_predicate() {
        let mut bus = VerifierBus::new();
        bus.register(Box::new(StubVerifier {
            name: "keep_me".into(),
            entries: vec![],
        }))
        .unwrap();
        bus.register(Box::new(StubVerifier {
            name: "drop_me".into(),
            entries: vec![],
        }))
        .unwrap();
        assert_eq!(bus.verifier_count(), 2);
        bus.retain_verifiers(|n| n == "keep_me");
        assert_eq!(bus.verifier_count(), 1);
        bus.run(&make_ctx());
        assert!(bus.verdicts().is_empty());
    }

    #[test]
    fn verifier_bus_run_overwrites_previous_verdicts() {
        let mut bus = VerifierBus::new();
        bus.register(Box::new(StubVerifier {
            name: "stub".into(),
            entries: vec![VerdictEntry {
                source: VerifierSource::Build,
                severity: Severity::Error,
                message: "first run".into(),
                file: None,
                line: None,
            }],
        }))
        .unwrap();
        bus.run(&make_ctx());
        assert_eq!(bus.verdicts().len(), 1);
        assert_eq!(bus.verdicts()[0].message, "first run");
        bus.run(&make_ctx());
        assert_eq!(
            bus.verdicts().len(),
            1,
            "run() clears prior verdicts before collecting"
        );
        assert_eq!(bus.verdicts()[0].message, "first run");
    }

    #[test]
    fn parse_ndjson_empty_string_returns_no_verdicts() {
        let v = TsOrchestratorBridgeVerifier::new(
            "ts-bridge".into(),
            PathBuf::from("bridge.sh"),
            PathBuf::from("/tmp"),
        );
        assert!(v.parse_ndjson("", &make_ctx()).is_empty());
    }

    #[test]
    fn parse_ndjson_skips_blank_lines() {
        let v = TsOrchestratorBridgeVerifier::new(
            "ts-bridge".into(),
            PathBuf::from("bridge.sh"),
            PathBuf::from("/tmp"),
        );
        let entries = v.parse_ndjson("\n  \n\t\n", &make_ctx());
        assert!(entries.is_empty(), "blank lines should be skipped");
    }

    #[test]
    fn parse_ndjson_verdict_without_rule_has_plain_message() {
        let v = TsOrchestratorBridgeVerifier::new(
            "ts-bridge".into(),
            PathBuf::from("bridge.sh"),
            PathBuf::from("/tmp"),
        );
        let stdout =
            "{\"verifier\":\"lint\",\"severity\":\"warning\",\"message\":\"unused import\"}";
        let entries = v.parse_ndjson(stdout, &make_ctx());
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].message, "unused import");
        assert!(!entries[0].message.contains("["));
    }

    #[test]
    fn parse_ndjson_verdict_with_missing_optional_fields_uses_none() {
        let v = TsOrchestratorBridgeVerifier::new(
            "ts-bridge".into(),
            PathBuf::from("bridge.sh"),
            PathBuf::from("/tmp"),
        );
        let stdout = "{\"verifier\":\"x\",\"severity\":\"info\",\"message\":\"hi\"}";
        let entries = v.parse_ndjson(stdout, &make_ctx());
        assert_eq!(entries.len(), 1);
        assert!(entries[0].file.is_none());
        assert!(entries[0].line.is_none());
    }

    #[test]
    fn parse_ndjson_malformed_line_becomes_warning_verdict() {
        let v = TsOrchestratorBridgeVerifier::new(
            "ts-bridge".into(),
            PathBuf::from("bridge.sh"),
            PathBuf::from("/tmp"),
        );
        let stdout = "not valid json at all";
        let entries = v.parse_ndjson(stdout, &make_ctx());
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].severity, Severity::Warning);
        assert!(
            entries[0].message.contains("malformed NDJSON"),
            "got: {}",
            entries[0].message
        );
        assert!(entries[0].message.contains("line 1"));
    }

    #[test]
    fn parse_ndjson_malformed_line_includes_line_number() {
        let v = TsOrchestratorBridgeVerifier::new(
            "ts-bridge".into(),
            PathBuf::from("bridge.sh"),
            PathBuf::from("/tmp"),
        );
        let stdout =
            "{\"verifier\":\"ok\",\"severity\":\"info\",\"message\":\"good\"}\nbroken line";
        let entries = v.parse_ndjson(stdout, &make_ctx());
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[1].severity, Severity::Warning);
        assert!(
            entries[1].message.contains("line 2"),
            "got: {}",
            entries[1].message
        );
    }

    #[test]
    fn parse_ndjson_rule_is_prefixed_to_message_in_brackets() {
        let v = TsOrchestratorBridgeVerifier::new(
            "ts-bridge".into(),
            PathBuf::from("bridge.sh"),
            PathBuf::from("/tmp"),
        );
        let stdout =
            "{\"verifier\":\"sec\",\"severity\":\"error\",\"message\":\"bad\",\"rule\":\"no-eval\"}";
        let entries = v.parse_ndjson(stdout, &make_ctx());
        assert_eq!(entries.len(), 1);
        assert!(
            entries[0].message.contains("[no-eval]"),
            "got: {}",
            entries[0].message
        );
        assert!(entries[0].message.contains("bad"));
    }

    #[test]
    fn parse_ndjson_source_is_ts_prefixed_verifier_name() {
        let v = TsOrchestratorBridgeVerifier::new(
            "ts-bridge".into(),
            PathBuf::from("bridge.sh"),
            PathBuf::from("/tmp"),
        );
        let stdout = "{\"verifier\":\"security\",\"severity\":\"info\",\"message\":\"x\"}";
        let entries = v.parse_ndjson(stdout, &make_ctx());
        assert!(matches!(
            entries[0].source,
            VerifierSource::Custom(ref s) if s == "ts:security"
        ));
    }

    #[test]
    fn run_bridge_missing_command_returns_error() {
        let v = TsOrchestratorBridgeVerifier::new(
            "ts-bridge".into(),
            PathBuf::from("does-not-exist.sh"),
            PathBuf::from("/tmp/nonexistent-bridge-dir"),
        );
        let result = v.run_bridge(&make_ctx());
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.contains("bridge command not found"), "got: {err}");
    }

    #[test]
    fn parse_severity_empty_string_defaults_to_warning() {
        assert_eq!(parse_severity(""), Severity::Warning);
    }
}
