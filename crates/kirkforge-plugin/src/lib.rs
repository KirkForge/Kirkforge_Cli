//! Plugin author SDK for KirkForge.
//!
//! A KirkForge plugin is a directory containing a `kirkforge.toml` manifest
//! plus optional assets: a `SKILL.md`, shell-hook scripts, tool definitions,
//! and verifier declarations. The manifest declares what the plugin provides
//! and how much trust it requires.
//!
//! For v1 the runtime is **manifest-based**: the executor loads static
//! declarations and invokes shell hooks or skill prompts. Dynamic native/WASM
//! plugins are intentionally out of scope — they are a future phase once the
//! trust model is proven.

#![cfg_attr(not(test), deny(clippy::unwrap_used))]

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};

/// Supported plugin API versions. v1 is the only stable contract today;
/// future major changes will introduce new variants.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ApiVersion {
    #[default]
    V1,
}

impl std::fmt::Display for ApiVersion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ApiVersion::V1 => write!(f, "v1"),
        }
    }
}

/// Plugin manifest loaded from `kirkforge.toml`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub struct PluginManifest {
    /// Human-readable plugin name.
    pub name: String,
    /// Semver version string.
    pub version: String,
    /// Short description.
    pub description: String,
    /// Plugin API version. Default: v1. The host rejects manifests that
    /// declare a version it does not understand.
    #[serde(default)]
    pub api_version: ApiVersion,
    /// Maximum trust tier the plugin requests.
    #[serde(default)]
    pub trust: TrustTier,
    /// Capabilities exposed by the plugin.
    #[serde(default)]
    pub capabilities: Vec<Capability>,
    /// Optional map of extra metadata the host can ignore or surface.
    #[serde(default)]
    pub metadata: HashMap<String, String>,
    /// Other plugins this one depends on (WO 11.2, ADR-058). The loader
    /// applies a topological sort so dependencies load before
    /// dependents; a missing dependency is rejected with a clear error;
    /// a cycle is rejected with the cycle path. Defaults to empty for
    /// backward compatibility (existing manifests without the field
    /// parse and load unchanged).
    #[serde(default, rename = "depends_on")]
    pub depends_on: Vec<String>,
    /// Per-plugin resource limits that override the global
    /// `SandboxConfig` for this plugin's tools (WO 11.5, ADR-060). When
    /// `None`, the global default applies. When `Some`, the present
    /// fields override the global; absent fields fall back to the
    /// global.
    #[serde(default, rename = "resource_limits")]
    pub resource_limits: Option<ResourceLimits>,
}

impl PluginManifest {
    /// Load a manifest from a `kirkforge.toml` file.
    pub fn from_file(path: &Path) -> Result<Self, ManifestError> {
        let content = std::fs::read_to_string(path).map_err(|e| ManifestError::Io {
            path: path.to_path_buf(),
            source: e,
        })?;
        Self::parse(&content)
    }

    /// Parse a manifest from a TOML string.
    pub fn parse(content: &str) -> Result<Self, ManifestError> {
        toml::from_str(content).map_err(ManifestError::Parse)
    }

    /// Validate that the manifest uses a supported API version.
    pub fn validate_api_version(&self) -> Result<(), ManifestError> {
        match self.api_version {
            ApiVersion::V1 => Ok(()),
        }
    }

    /// Validate the manifest's semantic constraints.
    ///
    /// Collects every error it finds (does not short-circuit on the first) and
    /// returns `Err(Vec<ValidationError>)` if any rule fails. Pure: no I/O, no
    /// filesystem checks — just structural and format validation against the
    /// rules documented in `docs/workorders/8.8-plugin-manifest-validation.md`.
    pub fn validate(&self) -> Result<(), Vec<ValidationError>> {
        let mut errors = Vec::new();

        if self.name.is_empty() {
            errors.push(ValidationError::new("name", "name must not be empty"));
        } else if !is_valid_plugin_name(&self.name) {
            errors.push(ValidationError::new(
                "name",
                "name must contain only lowercase alphanumeric segments joined by single hyphens (e.g. 'my-plugin')",
            ));
        }

        if !is_valid_semver(&self.version) {
            errors.push(ValidationError::new(
                "version",
                "version must be valid semver (MAJOR.MINOR.PATCH, optional pre-release and build)",
            ));
        }

        // WO 11.2: depends_on entries must be valid plugin names.
        for (i, dep) in self.depends_on.iter().enumerate() {
            if dep.is_empty() {
                errors.push(ValidationError::new(
                    format!("depends_on[{i}]"),
                    "depends_on entry must not be empty",
                ));
            } else if !is_valid_plugin_name(dep) {
                errors.push(ValidationError::new(
                    format!("depends_on[{i}]"),
                    format!(
                        "depends_on entry '{dep}' must be a valid plugin name \
                         (lowercase alphanumeric segments joined by single hyphens)"
                    ),
                ));
            } else if dep == &self.name {
                errors.push(ValidationError::new(
                    format!("depends_on[{i}]"),
                    "plugin cannot depend on itself",
                ));
            }
        }

        match self.api_version {
            ApiVersion::V1 => {}
        }

        let mut skill_triggers: std::collections::HashMap<&str, usize> =
            std::collections::HashMap::new();
        let mut tool_names: std::collections::HashMap<&str, usize> =
            std::collections::HashMap::new();
        let mut verifier_names: std::collections::HashMap<&str, usize> =
            std::collections::HashMap::new();

        for (idx, cap) in self.capabilities.iter().enumerate() {
            let path = format!("capabilities[{idx}]");
            match cap {
                Capability::Skill {
                    trigger,
                    skill_file,
                    prompt,
                    ..
                } => {
                    if !trigger.starts_with('/') {
                        errors.push(ValidationError::new(
                            format!("{path}.trigger"),
                            "skill trigger must start with '/'",
                        ));
                    }
                    if trigger.is_empty() {
                        errors.push(ValidationError::new(
                            format!("{path}.trigger"),
                            "skill trigger must not be empty",
                        ));
                    }
                    if prompt.is_empty() && skill_file.is_none() {
                        errors.push(ValidationError::new(
                            path.clone(),
                            "skill must declare a non-empty 'prompt' or a 'skill-file'",
                        ));
                    }
                    if let Some(prev) = skill_triggers.insert(trigger.as_str(), idx) {
                        errors.push(ValidationError::new(
                            format!("{path}.trigger"),
                            format!("skill trigger '{trigger}' duplicates capabilities[{prev}]"),
                        ));
                    }
                }
                Capability::Tool {
                    name,
                    schema,
                    command,
                    ..
                } => {
                    if name.is_empty() {
                        errors.push(ValidationError::new(
                            format!("{path}.name"),
                            "tool name must not be empty",
                        ));
                    }
                    if let Some(cmd) = command {
                        if let Err(e) = check_relative_command_path(cmd) {
                            errors.push(ValidationError::new(format!("{path}.command"), e));
                        }
                    }
                    if !is_valid_tool_schema(schema) {
                        errors.push(ValidationError::new(
                            format!("{path}.schema"),
                            "tool schema must be a JSON object with a valid optional 'type' field",
                        ));
                    }
                    if !name.is_empty() {
                        if let Some(prev) = tool_names.insert(name.as_str(), idx) {
                            errors.push(ValidationError::new(
                                format!("{path}.name"),
                                format!("tool name '{name}' duplicates capabilities[{prev}]"),
                            ));
                        }
                    }
                }
                Capability::Hook { event, command } => {
                    if !is_known_event(event) {
                        errors.push(ValidationError::new(
                            format!("{path}.event"),
                            format!(
                                "hook event '{event}' is not a known event (allowed: {})",
                                KNOWN_EVENTS.join(", ")
                            ),
                        ));
                    }
                    if let Err(e) = check_relative_command_path(command) {
                        errors.push(ValidationError::new(format!("{path}.command"), e));
                    }
                }
                Capability::Verifier { name, .. } => {
                    if name.is_empty() {
                        errors.push(ValidationError::new(
                            format!("{path}.name"),
                            "verifier name must not be empty",
                        ));
                    }
                    if !name.is_empty() {
                        if let Some(prev) = verifier_names.insert(name.as_str(), idx) {
                            errors.push(ValidationError::new(
                                format!("{path}.name"),
                                format!("verifier name '{name}' duplicates capabilities[{prev}]"),
                            ));
                        }
                    }
                }
            }
        }

        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }

    /// True if the plugin declares at least one capability of `kind`.
    pub fn has_capability(&self, kind: CapabilityKind) -> bool {
        self.capabilities.iter().any(|c| c.kind() == kind)
    }
}

/// A single manifest validation error. `path` is a dotted/indexed locator
/// (e.g. `capabilities[2].command`) so the host can present it back to the
/// user. Serialize/Deserialize allow the error to flow across plugin-host
/// boundaries.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ValidationError {
    pub path: String,
    pub message: String,
}

impl ValidationError {
    pub fn new(path: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            message: message.into(),
        }
    }
}

impl std::fmt::Display for ValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.path, self.message)
    }
}

/// Canonical hook event names emitted by the runtime. Manifests declaring
/// any other event name will fail validation.
pub const KNOWN_EVENTS: &[&str] = &[
    "session-start",
    "pre-turn",
    "post-turn",
    "pre-tool-bash",
    "post-tool-bash",
    "pre-compact",
    "post-compact",
];

/// True if `event` is one of the canonical hook event names.
fn is_known_event(event: &str) -> bool {
    KNOWN_EVENTS.contains(&event)
}

/// True if `name` is a non-empty, kebab-case identifier: lowercase
/// alphanumeric segments joined by single hyphens, no leading/trailing
/// hyphen and no consecutive hyphens.
fn is_valid_plugin_name(name: &str) -> bool {
    if name.is_empty() || name.starts_with('-') || name.ends_with('-') {
        return false;
    }
    let mut prev_was_hyphen = false;
    for ch in name.chars() {
        if ch == '-' {
            if prev_was_hyphen {
                return false;
            }
            prev_was_hyphen = true;
        } else if ch.is_ascii_lowercase() || ch.is_ascii_digit() {
            prev_was_hyphen = false;
        } else {
            return false;
        }
    }
    true
}

/// Minimal semver `MAJOR.MINOR.PATCH` validator with optional pre-release
/// (`-…`) and build (`+…`) suffixes. Each numeric component is parsed
/// without leading zeros (except the literal `"0"`). Pre-release and
/// build segments may contain `[0-9A-Za-z-]` separated by `.`. This is
/// not a full semver 2.0.0 implementation — it is a tight subset that
/// rejects obvious garbage (empty parts, leading zeros on non-zero
/// numbers, non-ASCII, etc.) and accepts everything a reasonable
/// plugin author will write.
fn is_valid_semver(version: &str) -> bool {
    let mut parts = version.split('+');
    let core = parts.next().unwrap_or("");
    let (core, pre) = match core.split_once('-') {
        Some((c, p)) => (c, Some(p)),
        None => (core, None),
    };
    let Some((major, rest)) = core.split_once('.') else {
        return false;
    };
    let Some((minor, patch)) = rest.split_once('.') else {
        return false;
    };
    if !is_numeric_component(major) || !is_numeric_component(minor) || !is_numeric_component(patch)
    {
        return false;
    }
    if let Some(pre) = pre {
        if !is_valid_dot_separated_idents(pre) {
            return false;
        }
    }
    for build in parts {
        if !is_valid_dot_separated_idents(build) {
            return false;
        }
    }
    true
}

fn is_numeric_component(s: &str) -> bool {
    if s.is_empty() {
        return false;
    }
    if s.len() > 1 && s.starts_with('0') {
        return false;
    }
    s.chars().all(|c| c.is_ascii_digit())
}

fn is_valid_dot_separated_idents(s: &str) -> bool {
    if s.is_empty() {
        return false;
    }
    s.split('.').all(is_valid_ident)
}

fn is_valid_ident(s: &str) -> bool {
    if s.is_empty() {
        return false;
    }
    s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

/// True if `path` looks like a relative command path that does not
/// escape the plugin root.
fn check_relative_command_path(path: &Path) -> Result<(), String> {
    let as_str = path.to_str().unwrap_or("");
    if as_str.is_empty() {
        return Err("command path must not be empty".to_string());
    }
    if path.is_absolute() || as_str.starts_with('/') {
        return Err(format!(
            "command path '{as_str}' must be relative to the plugin root"
        ));
    }
    if as_str.contains('\\') {
        return Err(format!(
            "command path '{as_str}' must use forward slashes, not backslashes"
        ));
    }
    for component in path.components() {
        if matches!(component, std::path::Component::ParentDir) {
            return Err(format!(
                "command path '{as_str}' must not contain '..' segments"
            ));
        }
    }
    Ok(())
}

/// True if `value` is a JSON Schema object suitable for a tool argument
/// schema. Accepts `Null` (the empty/default schema) or an object with
/// at most a `type` field whose value is one of the JSON Schema primitive
/// types. The full JSON Schema 2020-12 spec is not enforced — the host
/// validator (in `src/session/executor/helpers.rs`) handles detailed
/// checks. This is just an upfront structural sanity check.
fn is_valid_tool_schema(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::Null => true,
        serde_json::Value::Object(map) => {
            if map.is_empty() {
                return true;
            }
            match map.get("type") {
                None => true,
                Some(serde_json::Value::String(s)) => {
                    matches!(
                        s.as_str(),
                        "object" | "string" | "number" | "integer" | "boolean" | "array" | "null"
                    )
                }
                Some(_) => false,
            }
        }
        _ => false,
    }
}

impl Default for PluginManifest {
    fn default() -> Self {
        Self {
            name: "unnamed".into(),
            version: "0.1.0".into(),
            description: String::new(),
            api_version: ApiVersion::V1,
            trust: TrustTier::ReadOnly,
            capabilities: Vec::new(),
            metadata: HashMap::new(),
            depends_on: Vec::new(),
            resource_limits: None,
        }
    }
}

/// Trust tier requested by a plugin.
///
/// The host's `max_plugin_trust` config can downgrade or block a plugin that
/// requests more trust than the operator allows.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "kebab-case")]
pub enum TrustTier {
    /// Only read-only operations (`read_file`, `grep`, `glob`).
    #[default]
    ReadOnly,
    /// May invoke shell commands (`bash`).
    Shell,
    /// May fetch URLs or talk to network services.
    Network,
    /// Arbitrary native code / unsafe operations (blocked by default).
    Unsafe,
}

impl TrustTier {
    /// Returns true if `self` is at least as privileged as `other`.
    pub fn permits(self, other: TrustTier) -> bool {
        self.rank() >= other.rank()
    }

    fn rank(self) -> u8 {
        match self {
            TrustTier::ReadOnly => 0,
            TrustTier::Shell => 1,
            TrustTier::Network => 2,
            TrustTier::Unsafe => 3,
        }
    }
}

impl fmt::Display for TrustTier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            TrustTier::ReadOnly => "read-only",
            TrustTier::Shell => "shell",
            TrustTier::Network => "network",
            TrustTier::Unsafe => "unsafe",
        };
        write!(f, "{s}")
    }
}

/// Per-plugin resource limits that override the global `SandboxConfig`
/// (WO 11.5, ADR-060). Each field is optional; when present, it
/// overrides the global default for that plugin's tools. When absent,
/// the global default applies.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct ResourceLimits {
    /// CPU time limit in seconds (maps to `RLIMIT_CPU`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpu_secs: Option<u64>,
    /// Address space limit in megabytes (maps to `RLIMIT_AS`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_mb: Option<u64>,
    /// Max file size in megabytes (maps to `RLIMIT_FSIZE`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filesize_mb: Option<u64>,
}

/// Classification of a capability for quick filtering.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CapabilityKind {
    Skill,
    Tool,
    Hook,
    Verifier,
}

/// A capability exposed by a plugin.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum Capability {
    /// A slash-command skill backed by a prompt.
    Skill {
        trigger: String,
        #[serde(default)]
        prompt: String,
        #[serde(rename = "skill-file", default)]
        skill_file: Option<PathBuf>,
        #[serde(rename = "model-hint", default)]
        model_hint: Option<String>,
    },
    /// A tool backed by a shell command or future native implementation.
    Tool {
        name: String,
        #[serde(default)]
        description: String,
        #[serde(default)]
        schema: serde_json::Value,
        #[serde(rename = "command", default)]
        command: Option<PathBuf>,
    },
    /// A lifecycle hook script.
    Hook { event: String, command: PathBuf },
    /// A verifier that runs deterministic checks after tool events.
    Verifier {
        name: String,
        #[serde(default)]
        priority: u8,
        #[serde(rename = "command", default)]
        command: Option<PathBuf>,
    },
}

impl Capability {
    pub fn kind(&self) -> CapabilityKind {
        match self {
            Capability::Skill { .. } => CapabilityKind::Skill,
            Capability::Tool { .. } => CapabilityKind::Tool,
            Capability::Hook { .. } => CapabilityKind::Hook,
            Capability::Verifier { .. } => CapabilityKind::Verifier,
        }
    }
}

/// Errors that can occur while loading/parsing a plugin manifest.
#[derive(Debug, thiserror::Error)]
pub enum ManifestError {
    #[error("cannot read manifest at {path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("failed to parse manifest: {0}")]
    Parse(#[from] toml::de::Error),
    #[error("unsupported api_version '{version}': host only supports v1")]
    UnsupportedApiVersion { version: String },
}

/// High-level plugin interface.
///
/// v1 plugins are loaded from disk and represented as a manifest plus
/// optional `SKILL.md` content. Future versions may add a dynamic trait
/// implementation for native plugins.
pub trait Plugin: Send + Sync {
    /// Plugin manifest.
    fn manifest(&self) -> &PluginManifest;
    /// Directory the plugin was loaded from.
    fn root(&self) -> &Path;
    /// Rendered skill prompt for a given trigger, if the plugin exposes one.
    fn skill_prompt(&self, trigger: &str, args: &str) -> Option<String>;
    /// All hook definitions (owned copy for now — v1 plugins are static).
    fn hooks(&self) -> Vec<Capability>;
    /// All verifier definitions.
    fn verifiers(&self) -> Vec<Capability>;
    /// All tool definitions.
    fn tools(&self) -> Vec<Capability>;
}

/// A lightweight v1 plugin loaded from a directory.
#[derive(Debug, Clone)]
pub struct LoadedPlugin {
    pub manifest: PluginManifest,
    pub root: PathBuf,
    pub skill_prompts: HashMap<String, String>,
    pub hooks: Vec<Capability>,
    pub verifiers: Vec<Capability>,
    pub tools: Vec<Capability>,
}

impl LoadedPlugin {
    /// Load a plugin directory containing a `kirkforge.toml`.
    pub fn load(path: &Path) -> Result<Self, ManifestError> {
        let manifest = PluginManifest::from_file(&path.join("kirkforge.toml"))?;
        let mut skill_prompts = HashMap::new();
        let mut hooks = Vec::new();
        let mut verifiers = Vec::new();
        let mut tools = Vec::new();

        for cap in manifest.capabilities.clone() {
            match &cap {
                Capability::Skill {
                    trigger,
                    skill_file,
                    prompt,
                    ..
                } => {
                    let content = skill_file
                        .as_ref()
                        .and_then(|f| std::fs::read_to_string(path.join(f)).ok())
                        .filter(|c| !c.trim().is_empty())
                        .unwrap_or_else(|| prompt.clone());
                    if !content.is_empty() {
                        skill_prompts.insert(trigger.clone(), content);
                    }
                }
                Capability::Hook { .. } => hooks.push(cap),
                Capability::Verifier { .. } => verifiers.push(cap),
                Capability::Tool { .. } => tools.push(cap),
            }
        }

        Ok(Self {
            manifest,
            root: path.to_path_buf(),
            skill_prompts,
            hooks,
            verifiers,
            tools,
        })
    }
}

impl Plugin for LoadedPlugin {
    fn manifest(&self) -> &PluginManifest {
        &self.manifest
    }

    fn root(&self) -> &Path {
        &self.root
    }

    fn skill_prompt(&self, trigger: &str, args: &str) -> Option<String> {
        self.skill_prompts.get(trigger).map(|template| {
            template
                .replace("{{args}}", args)
                .replace("{{trigger}}", trigger)
        })
    }

    fn hooks(&self) -> Vec<Capability> {
        self.hooks.clone()
    }

    fn verifiers(&self) -> Vec<Capability> {
        self.verifiers.clone()
    }

    fn tools(&self) -> Vec<Capability> {
        self.tools.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_minimal_manifest() {
        let toml = r#"
name = "my-linter"
version = "0.1.0"
description = "Lint Rust files"
"#;
        let m = PluginManifest::parse(toml).unwrap();
        assert_eq!(m.name, "my-linter");
        assert_eq!(m.trust, TrustTier::ReadOnly);
        assert!(m.capabilities.is_empty());
    }

    #[test]
    fn parse_manifest_with_capabilities() {
        let toml = r#"
name = "net-plugin"
version = "1.0.0"
description = "Fetch things"
trust = "network"

[[capabilities]]
type = "skill"
trigger = "/fetch"
prompt = "Fetch {{args}}"
model-hint = "fast"

[[capabilities]]
type = "hook"
event = "pre-tool-bash"
command = "hooks/pre-tool-bash.sh"
"#;
        let m = PluginManifest::parse(toml).unwrap();
        assert_eq!(m.trust, TrustTier::Network);
        assert_eq!(m.capabilities.len(), 2);
        assert!(m.has_capability(CapabilityKind::Skill));
        assert!(m.has_capability(CapabilityKind::Hook));
    }

    #[test]
    fn trust_tier_ordering() {
        assert!(TrustTier::Shell.permits(TrustTier::ReadOnly));
        assert!(!TrustTier::ReadOnly.permits(TrustTier::Shell));
        assert!(TrustTier::Unsafe.permits(TrustTier::Network));
    }

    #[test]
    fn loaded_plugin_renders_skill_prompt() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("my-plugin");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            root.join("kirkforge.toml"),
            r#"
name = "demo"
version = "0.1.0"
description = "demo"

[[capabilities]]
type = "skill"
trigger = "/demo"
prompt = "Demo task: {{args}}"
"#,
        )
        .unwrap();

        let plugin = LoadedPlugin::load(&root).unwrap();
        assert_eq!(
            plugin.skill_prompt("/demo", "hello"),
            Some("Demo task: hello".to_string())
        );
    }

    mod validate_tests {
        use super::*;

        fn base_manifest() -> PluginManifest {
            PluginManifest {
                name: "demo-plugin".into(),
                version: "1.2.3".into(),
                description: "demo".into(),
                api_version: ApiVersion::V1,
                trust: TrustTier::ReadOnly,
                capabilities: Vec::new(),
                metadata: HashMap::new(),
                depends_on: Vec::new(),
                resource_limits: None,
            }
        }

        fn tool_cap(name: &str, command: &str) -> Capability {
            Capability::Tool {
                name: name.into(),
                description: "test".into(),
                schema: serde_json::json!({"type": "object"}),
                command: Some(PathBuf::from(command)),
            }
        }

        fn hook_cap(event: &str, command: &str) -> Capability {
            Capability::Hook {
                event: event.into(),
                command: PathBuf::from(command),
            }
        }

        fn skill_cap(trigger: &str, prompt: &str) -> Capability {
            Capability::Skill {
                trigger: trigger.into(),
                prompt: prompt.into(),
                skill_file: None,
                model_hint: None,
            }
        }

        fn verifier_cap(name: &str) -> Capability {
            Capability::Verifier {
                name: name.into(),
                priority: 0,
                command: None,
            }
        }

        fn assert_path(errors: &[ValidationError], path: &str) {
            assert!(
                errors.iter().any(|e| e.path == path),
                "expected error at path '{path}', got: {errors:?}"
            );
        }

        #[test]
        fn valid_manifest_passes() {
            let m = base_manifest();
            assert!(m.validate().is_ok(), "{:?}", m.validate());
        }

        #[test]
        fn name_must_be_non_empty() {
            let mut m = base_manifest();
            m.name = String::new();
            let errs = m.validate().unwrap_err();
            assert_path(&errs, "name");
        }

        #[test]
        fn name_rejects_uppercase_and_special_chars() {
            for bad in ["Bad", "bad_name", "-leading", "trailing-", "dou--ble"] {
                let mut m = base_manifest();
                m.name = bad.into();
                let errs = m.validate().unwrap_err();
                assert_path(&errs, "name");
            }
        }

        #[test]
        fn version_rejects_invalid_semver() {
            for bad in ["1.2", "1.2.3.4", "v1.2.3", "01.2.3", "1.2.3-", ""] {
                let mut m = base_manifest();
                m.version = bad.into();
                let errs = m.validate().unwrap_err();
                assert_path(&errs, "version");
            }
        }

        #[test]
        fn version_accepts_pre_release_and_build() {
            for good in [
                "1.0.0",
                "0.0.0",
                "1.2.3-alpha",
                "1.2.3-alpha.1",
                "1.2.3-rc.1+build.42",
                "10.20.30",
            ] {
                let mut m = base_manifest();
                m.version = good.into();
                assert!(m.validate().is_ok(), "version {good} should be valid");
            }
        }

        #[test]
        fn tool_command_must_be_relative() {
            let mut m = base_manifest();
            m.capabilities.push(tool_cap("bad/abs", "/bin/sh"));
            let errs = m.validate().unwrap_err();
            assert_path(&errs, "capabilities[0].command");
        }

        #[test]
        fn tool_command_must_not_contain_parent_dir() {
            let mut m = base_manifest();
            m.capabilities.push(tool_cap("bad/parent", "../evil.sh"));
            let errs = m.validate().unwrap_err();
            assert_path(&errs, "capabilities[0].command");
        }

        #[test]
        fn tool_schema_with_invalid_type_is_rejected() {
            let mut m = base_manifest();
            m.capabilities.push(Capability::Tool {
                name: "ok".into(),
                description: String::new(),
                schema: serde_json::json!({"type": "banana"}),
                command: Some(PathBuf::from("tools/x.sh")),
            });
            let errs = m.validate().unwrap_err();
            assert_path(&errs, "capabilities[0].schema");
        }

        #[test]
        fn tool_schema_null_or_object_is_accepted() {
            let mut m = base_manifest();
            m.capabilities.push(Capability::Tool {
                name: "null-schema".into(),
                description: String::new(),
                schema: serde_json::Value::Null,
                command: Some(PathBuf::from("tools/x.sh")),
            });
            m.capabilities.push(Capability::Tool {
                name: "empty-schema".into(),
                description: String::new(),
                schema: serde_json::json!({}),
                command: Some(PathBuf::from("tools/y.sh")),
            });
            assert!(m.validate().is_ok(), "{:?}", m.validate());
        }

        #[test]
        fn hook_event_must_be_known() {
            let mut m = base_manifest();
            m.capabilities.push(hook_cap("totally-made-up", "h.sh"));
            let errs = m.validate().unwrap_err();
            assert_path(&errs, "capabilities[0].event");
        }

        #[test]
        fn hook_command_must_be_relative() {
            let mut m = base_manifest();
            m.capabilities.push(hook_cap("post-turn", "/abs/h.sh"));
            let errs = m.validate().unwrap_err();
            assert_path(&errs, "capabilities[0].command");
        }

        #[test]
        fn skill_trigger_must_start_with_slash() {
            let mut m = base_manifest();
            m.capabilities.push(skill_cap("bad-trigger", "do thing"));
            let errs = m.validate().unwrap_err();
            assert_path(&errs, "capabilities[0].trigger");
        }

        #[test]
        fn skill_without_prompt_or_skill_file_fails() {
            let mut m = base_manifest();
            m.capabilities.push(skill_cap("/x", ""));
            let errs = m.validate().unwrap_err();
            assert_path(&errs, "capabilities[0]");
        }

        #[test]
        fn verifier_name_must_be_non_empty() {
            let mut m = base_manifest();
            m.capabilities.push(verifier_cap(""));
            let errs = m.validate().unwrap_err();
            assert_path(&errs, "capabilities[0].name");
        }

        #[test]
        fn duplicate_skill_triggers_are_rejected() {
            let mut m = base_manifest();
            m.capabilities.push(skill_cap("/dup", "first"));
            m.capabilities.push(skill_cap("/dup", "second"));
            let errs = m.validate().unwrap_err();
            assert_path(&errs, "capabilities[1].trigger");
        }

        #[test]
        fn duplicate_tool_names_are_rejected() {
            let mut m = base_manifest();
            m.capabilities.push(tool_cap("dup", "a.sh"));
            m.capabilities.push(tool_cap("dup", "b.sh"));
            let errs = m.validate().unwrap_err();
            assert_path(&errs, "capabilities[1].name");
        }

        #[test]
        fn duplicate_verifier_names_are_rejected() {
            let mut m = base_manifest();
            m.capabilities.push(verifier_cap("dup"));
            m.capabilities.push(verifier_cap("dup"));
            let errs = m.validate().unwrap_err();
            assert_path(&errs, "capabilities[1].name");
        }

        #[test]
        fn multiple_errors_are_collected() {
            let mut m = base_manifest();
            m.name = "Bad".into();
            m.version = "not-semver".into();
            m.capabilities.push(tool_cap("t", "/abs.sh"));
            m.capabilities.push(skill_cap("no-slash", ""));
            let errs = m.validate().unwrap_err();
            // Four distinct errors expected, no short-circuit.
            assert!(
                errs.len() >= 4,
                "expected at least 4 errors, got {}: {errs:?}",
                errs.len()
            );
            assert_path(&errs, "name");
            assert_path(&errs, "version");
            assert_path(&errs, "capabilities[0].command");
            assert_path(&errs, "capabilities[1].trigger");
        }

        #[test]
        fn validation_error_serializes_to_json() {
            let err = ValidationError::new("capabilities[0].name", "name must not be empty");
            let json = serde_json::to_string(&err).unwrap();
            assert!(json.contains("capabilities[0].name"));
            assert!(json.contains("name must not be empty"));
            let back: ValidationError = serde_json::from_str(&json).unwrap();
            assert_eq!(back, err);
        }
    }
}

#[cfg(test)]
mod depends_on_tests {
    use super::*;

    fn manifest(name: &str, deps: &[&str]) -> PluginManifest {
        PluginManifest {
            name: name.into(),
            version: "0.1.0".into(),
            description: "test".into(),
            api_version: ApiVersion::V1,
            trust: TrustTier::ReadOnly,
            capabilities: Vec::new(),
            metadata: HashMap::new(),
            depends_on: deps.iter().map(|s| s.to_string()).collect(),
            resource_limits: None,
        }
    }

    #[test]
    fn empty_depends_on_is_valid() {
        assert!(manifest("a", &[]).validate().is_ok());
    }

    #[test]
    fn valid_depends_on_passes() {
        assert!(manifest("a", &["b", "c"]).validate().is_ok());
    }

    #[test]
    fn invalid_depends_on_name_rejected() {
        let m = manifest("a", &["BadName"]);
        let errs = m.validate().unwrap_err();
        assert!(errs.iter().any(|e| e.path == "depends_on[0]"));
    }

    #[test]
    fn self_dependency_rejected() {
        let m = manifest("a", &["a"]);
        let errs = m.validate().unwrap_err();
        assert!(errs
            .iter()
            .any(|e| e.message.contains("cannot depend on itself")));
    }

    #[test]
    fn empty_depends_on_entry_rejected() {
        let m = manifest("a", &[""]);
        let errs = m.validate().unwrap_err();
        assert!(errs.iter().any(|e| e.path == "depends_on[0]"));
    }

    #[test]
    fn depends_on_defaults_empty_when_absent() {
        let toml = r#"
name = "demo"
version = "0.1.0"
description = "demo"
"#;
        let m = PluginManifest::parse(toml).unwrap();
        assert!(m.depends_on.is_empty());
    }

    #[test]
    fn depends_on_parses_list() {
        let toml = r#"
name = "demo"
version = "0.1.0"
description = "demo"
depends_on = ["stratum", "other"]
"#;
        let m = PluginManifest::parse(toml).unwrap();
        assert_eq!(m.depends_on, vec!["stratum", "other"]);
    }
}
