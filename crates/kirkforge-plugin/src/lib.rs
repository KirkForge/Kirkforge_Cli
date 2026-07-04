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

    /// True if the plugin declares at least one capability of `kind`.
    pub fn has_capability(&self, kind: CapabilityKind) -> bool {
        self.capabilities.iter().any(|c| c.kind() == kind)
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
}
