// Public/future surface in a binary crate: suppress dead-code warnings for pub items.
#![allow(dead_code)]

/// Skills system — slash-command skill registry and loader.
///
/// Skills are reusable capabilities defined in SKILL.md files with
/// YAML frontmatter. They register as slash commands (e.g. `/lint`,
/// `/test`, `/explain`) that the user or model can invoke.
///
/// # SKILL.md format
///
/// ```markdown
/// ---
/// name: lint
/// description: Run clippy on the project and report warnings
/// trigger: /lint
/// model: fast  # optional: "fast", "default", or a specific model name
/// ---
///
/// Run cargo clippy -- -D warnings on the current project.
/// Parse the output and fix any warnings found.
/// ```
///
/// The body after the frontmatter is the system prompt that's injected
/// when the skill is invoked.
use kirkforge_plugin::{Capability, TrustTier};
use kirkforge_plugin_host::{PluginRegistry, TrustPolicy};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Parsed frontmatter from a SKILL.md file.
#[derive(Debug, Clone, Default)]
pub struct SkillMeta {
    /// Short name (e.g. "lint", "test")
    pub name: String,
    /// One-line description shown in slash-completion
    pub description: String,
    /// Slash trigger (e.g. "/lint")
    pub trigger: String,
    /// Model hint: "fast", "default", or a specific model name
    pub model: Option<String>,
}

/// A loaded skill ready to invoke.
#[derive(Debug, Clone)]
pub struct Skill {
    pub meta: SkillMeta,
    /// System prompt body (everything after the frontmatter).
    pub prompt_body: String,
    /// The directory the skill was loaded from (for resolving relative paths).
    pub source_dir: PathBuf,
}

impl Skill {
    /// Render the full system prompt for this skill, appending user input.
    ///
    /// Plugin prompts may contain a `{{args}}` placeholder; it is replaced
    /// with the user input before the standard suffix is appended.
    pub fn render_prompt(&self, user_input: &str) -> String {
        let body = self.prompt_body.replace("{{args}}", user_input);
        format!("{}\n\nUser request: {}", body, user_input)
    }
}

/// Registry of loaded skills, indexed by slash trigger.
#[derive(Debug, Clone, Default)]
pub struct SkillRegistry {
    skills: HashMap<String, Skill>,
    triggers: HashMap<String, String>, // trigger → name
    scan_paths: Vec<PathBuf>,
    plugin_registry: PluginRegistry,
    plugin_warnings: Vec<String>,
    max_plugin_trust: TrustTier,
}

impl SkillRegistry {
    pub fn new() -> Self {
        Self {
            skills: HashMap::new(),
            triggers: HashMap::new(),
            scan_paths: vec![PathBuf::from(".claude/skills")],
            plugin_registry: PluginRegistry::new(),
            plugin_warnings: Vec::new(),
            max_plugin_trust: TrustTier::Shell,
        }
    }

    /// Set the maximum trust tier for loaded plugins.
    pub fn set_max_plugin_trust(&mut self, max: TrustTier) {
        self.max_plugin_trust = max;
    }

    /// Add a directory to scan for SKILL.md files.
    /// Paths are resolved relative to the project root (cwd).
    pub fn add_scan_path(&mut self, path: PathBuf) {
        if !self.scan_paths.contains(&path) {
            self.scan_paths.push(path);
        }
    }

    /// Scan all registered paths and load any SKILL.md files found.
    ///
    /// Also loads plugin directories from `~/.local/share/kirkforge/plugins`
    /// and registers any plugin skills whose trust tier is allowed.
    pub fn scan_and_load(&mut self) -> anyhow::Result<usize> {
        let mut count = 0;
        let paths = self.scan_paths.clone();
        for base in &paths {
            if !base.exists() {
                continue;
            }
            count += self.load_from_dir(base)?;
        }
        count += self.load_plugins()?;
        Ok(count)
    }

    /// Load plugins from the canonical data-directory plugins folder and
    /// register their skills.
    fn load_plugins(&mut self) -> anyhow::Result<usize> {
        let plugins_dir = crate::session::data_dir()
            .map(|d| d.join("plugins"))
            .unwrap_or_else(|_| PathBuf::from(".local/share/kirkforge/plugins"));

        self.plugin_registry = PluginRegistry::new();
        let warnings = self
            .plugin_registry
            .load_from_dir(&plugins_dir, TrustPolicy::up_to(self.max_plugin_trust))
            .unwrap_or_default();
        self.plugin_warnings = warnings;

        let mut count = 0;
        for trigger in self.plugin_registry.skill_triggers() {
            let Some((manifest, plugin)) = self.plugin_registry.skill_by_trigger(&trigger) else {
                continue;
            };
            let prompt_template = plugin.skill_prompt(&trigger, "").unwrap_or_default();
            let model_hint = manifest.capabilities.iter().find_map(|c| match c {
                Capability::Skill {
                    trigger: t,
                    model_hint,
                    ..
                } if t == &trigger => model_hint.clone(),
                _ => None,
            });

            let skill = Skill {
                meta: SkillMeta {
                    name: format!("{}-{}", manifest.name, trigger.trim_start_matches('/')),
                    description: format!("{} [{} plugin]", manifest.description, manifest.trust),
                    trigger: trigger.clone(),
                    model: model_hint,
                },
                prompt_body: prompt_template,
                source_dir: plugin
                    .manifest()
                    .metadata
                    .get("source_dir")
                    .map(PathBuf::from)
                    .unwrap_or_else(|| plugins_dir.clone()),
            };
            self.register(skill);
            count += 1;
        }
        Ok(count)
    }

    /// Warnings emitted while loading plugins (trust rejections, parse
    /// failures, etc.).
    pub fn plugin_warnings(&self) -> &[String] {
        &self.plugin_warnings
    }

    /// Active plugin manifests, useful for status/logging.
    pub fn active_plugins(&self) -> Vec<&kirkforge_plugin::PluginManifest> {
        self.plugin_registry
            .active_plugins()
            .iter()
            .map(|p| &p.plugin.manifest)
            .collect()
    }

    /// Human-readable summary of active plugin trust tiers for the TUI
    /// status bar.
    ///
    /// Returns `None` if no plugins are active. Otherwise returns a compact
    /// string like "🔒2 ⚡1 🌐0" with one glyph per tier. Rejected plugins
    /// are reported as "☠️N blocked" so the user can see that a manifest
    /// exceeded the configured `max_plugin_trust`.
    pub fn plugin_status_summary(&self) -> Option<String> {
        let active = self.plugin_registry.active_plugins();
        if active.is_empty() && self.plugin_warnings.is_empty() {
            return None;
        }

        let mut read_only = 0usize;
        let mut shell = 0usize;
        let mut network = 0usize;
        let mut unsafe_ = 0usize;
        for p in active {
            match p.effective_trust {
                TrustTier::ReadOnly => read_only += 1,
                TrustTier::Shell => shell += 1,
                TrustTier::Network => network += 1,
                TrustTier::Unsafe => unsafe_ += 1,
            }
        }

        let mut parts = Vec::new();
        if read_only > 0 {
            parts.push(format!("🔒{}", read_only));
        }
        if shell > 0 {
            parts.push(format!("⚡{}", shell));
        }
        if network > 0 {
            parts.push(format!("🌐{}", network));
        }
        if unsafe_ > 0 {
            parts.push(format!("☠️{}", unsafe_));
        }

        let rejected = self.plugin_warnings.len();
        if rejected > 0 {
            parts.push(format!("☠️{} blocked", rejected));
        }

        if parts.is_empty() {
            None
        } else {
            Some(parts.join(" "))
        }
    }

    /// Recursively scan a directory for SKILL.md files.
    fn load_from_dir(&mut self, dir: &Path) -> anyhow::Result<usize> {
        if !dir.is_dir() {
            return Ok(0);
        }
        let mut count = 0;
        let mut walker = ignore::WalkBuilder::new(dir)
            .max_depth(Some(3))
            .standard_filters(false) // don't ignore hidden dirs like .claude
            .build();
        let mut entries = Vec::new();
        for result in walker.by_ref() {
            match result {
                Ok(entry) => {
                    if entry.file_name() == "SKILL.md" {
                        entries.push(entry.path().to_path_buf());
                    }
                }
                Err(e) => {
                    tracing::warn!(error = %e, "failed to walk skill directory");
                }
            }
        }
        for path in entries {
            match load_skill_from_file(&path) {
                Ok(skill) => {
                    let name = skill.meta.name.clone();
                    let trigger = skill.meta.trigger.clone();
                    self.skills.insert(name.clone(), skill);
                    self.triggers.insert(trigger, name);
                    count += 1;
                }
                Err(e) => {
                    tracing::warn!(path = %path.display(), error = %e, "failed to load SKILL.md");
                }
            }
        }
        Ok(count)
    }

    /// Register a skill programmatically (without a SKILL.md file).
    pub fn register(&mut self, skill: Skill) {
        let name = skill.meta.name.clone();
        let trigger = skill.meta.trigger.clone();
        self.skills.insert(name.clone(), skill);
        self.triggers.insert(trigger, name);
    }

    /// Look up a skill by slash trigger (e.g., "/lint").
    pub fn get_by_trigger(&self, trigger: &str) -> Option<&Skill> {
        let name = self.triggers.get(trigger)?;
        self.skills.get(name)
    }

    /// Look up a skill by name.
    pub fn get_by_name(&self, name: &str) -> Option<&Skill> {
        self.skills.get(name)
    }

    /// Check if a slash trigger is registered.
    pub fn has_trigger(&self, trigger: &str) -> bool {
        self.triggers.contains_key(trigger)
    }

    /// Return all registered skills.
    pub fn all(&self) -> Vec<&Skill> {
        self.skills.values().collect()
    }

    /// Return all registered triggers.
    pub fn triggers(&self) -> Vec<&str> {
        self.triggers.keys().map(|s| s.as_str()).collect()
    }

    /// Number of registered skills.
    pub fn len(&self) -> usize {
        self.skills.len()
    }

    pub fn is_empty(&self) -> bool {
        self.skills.is_empty()
    }

    /// Remove a skill by name.
    pub fn remove(&mut self, name: &str) -> bool {
        if let Some(skill) = self.skills.remove(name) {
            self.triggers.remove(&skill.meta.trigger);
            true
        } else {
            false
        }
    }
}

/// Parse a SKILL.md file and return the skill.
///
/// Expected format:
/// ```markdown
/// ---
/// name: lint
/// description: Run clippy on the project
/// trigger: /lint
/// model: fast
/// ---
/// Body text here...
/// ```
pub fn load_skill_from_file(path: &Path) -> anyhow::Result<Skill> {
    let content = std::fs::read_to_string(path)?;
    parse_skill(
        &content,
        path.parent().unwrap_or(Path::new(".")).to_path_buf(),
    )
}

/// Parse SKILL.md content from a string.
pub fn parse_skill(content: &str, source_dir: PathBuf) -> anyhow::Result<Skill> {
    let content = content.trim();

    // Split on the frontmatter delimiter
    if !content.starts_with("---") {
        anyhow::bail!("SKILL.md must start with '---' frontmatter delimiter");
    }

    // `starts_with` was just verified, so slicing past the prefix is safe.
    let after_first = content["---".len()..].trim();
    let end_idx = after_first
        .find("\n---")
        .ok_or_else(|| anyhow::anyhow!("SKILL.md missing closing '---' delimiter"))?;

    let frontmatter_str = &after_first[..end_idx];
    let body = after_first[end_idx + 5..].trim().to_string(); // skip "\n---\n"

    let meta = parse_frontmatter(frontmatter_str)?;

    Ok(Skill {
        meta,
        prompt_body: body,
        source_dir,
    })
}

/// Parse YAML-like frontmatter (simple key: value pairs, no nesting).
fn parse_frontmatter(content: &str) -> anyhow::Result<SkillMeta> {
    let mut meta = SkillMeta::default();

    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        let colon_idx = match line.find(':') {
            Some(i) => i,
            None => continue,
        };

        let key = line[..colon_idx].trim();
        let value = line[colon_idx + 1..].trim().trim_matches('"');

        match key {
            "name" => meta.name = value.to_string(),
            "description" => meta.description = value.to_string(),
            "trigger" => meta.trigger = value.to_string(),
            "model" => meta.model = Some(value.to_string()),
            _ => {} // ignore unknown fields
        }
    }

    if meta.name.is_empty() {
        anyhow::bail!("SKILL.md missing required 'name' field in frontmatter");
    }
    if meta.trigger.is_empty() {
        anyhow::bail!("SKILL.md missing required 'trigger' field in frontmatter");
    }

    Ok(meta)
}

/// Default built-in skills.
pub fn builtin_skills() -> Vec<Skill> {
    vec![
        Skill {
            meta: SkillMeta {
                name: "help".into(),
                description: "Show available slash commands".into(),
                trigger: "/help".into(),
                model: None,
            },
            prompt_body: "List all available skills and their descriptions. \
                          Format as a bullet list with the trigger and description."
                .into(),
            source_dir: PathBuf::from("."),
        },
        Skill {
            meta: SkillMeta {
                name: "status".into(),
                description: "Show project status summary".into(),
                trigger: "/status".into(),
                model: Some("fast".into()),
            },
            prompt_body: "Summarize the current git status, recent changes, \
                          and any obvious issues in the project."
                .into(),
            source_dir: PathBuf::from("."),
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_frontmatter_basic() {
        let content = r#"---
name: lint
description: Run clippy
trigger: /lint
model: fast
---
Run cargo clippy and fix issues."#;

        let skill = parse_skill(content, PathBuf::from(".")).unwrap();
        assert_eq!(skill.meta.name, "lint");
        assert_eq!(skill.meta.description, "Run clippy");
        assert_eq!(skill.meta.trigger, "/lint");
        assert_eq!(skill.meta.model, Some("fast".into()));
        assert!(skill.prompt_body.contains("cargo clippy"));
    }

    #[test]
    fn test_parse_frontmatter_no_model() {
        let content = r#"---
name: explain
description: Explain code
trigger: /explain
---
Explain the selected code."#;

        let skill = parse_skill(content, PathBuf::from(".")).unwrap();
        assert_eq!(skill.meta.name, "explain");
        assert_eq!(skill.meta.trigger, "/explain");
        assert!(skill.meta.model.is_none());
    }

    #[test]
    fn test_missing_name_fails() {
        let content = r#"---
description: No name
trigger: /test
---
Body."#;

        let err = parse_skill(content, PathBuf::from("."));
        assert!(err.is_err());
        assert!(err.unwrap_err().to_string().contains("name"));
    }

    #[test]
    fn test_missing_trigger_fails() {
        let content = r#"---
name: test
description: No trigger
---
Body."#;

        let err = parse_skill(content, PathBuf::from("."));
        assert!(err.is_err());
        assert!(err.unwrap_err().to_string().contains("trigger"));
    }

    #[test]
    fn test_no_frontmatter_fails() {
        let content = "Just a plain markdown file with no frontmatter.";
        let err = parse_skill(content, PathBuf::from("."));
        assert!(err.is_err());
    }

    #[test]
    fn test_missing_closing_delimiter_fails() {
        let content = r#"---
name: test
trigger: /test
Body without closing delimiter."#;

        let err = parse_skill(content, PathBuf::from("."));
        assert!(err.is_err());
    }

    #[test]
    fn test_skill_registry_lookup() {
        let mut reg = SkillRegistry::new();

        let skill = Skill {
            meta: SkillMeta {
                name: "lint".into(),
                description: "Run clippy".into(),
                trigger: "/lint".into(),
                model: None,
            },
            prompt_body: "Run cargo clippy.".into(),
            source_dir: PathBuf::from("."),
        };
        reg.register(skill);

        assert!(reg.has_trigger("/lint"));
        assert!(!reg.has_trigger("/nope"));

        let found = reg.get_by_trigger("/lint");
        assert!(found.is_some());
        assert_eq!(found.unwrap().meta.name, "lint");

        let found2 = reg.get_by_name("lint");
        assert!(found2.is_some());
    }

    #[test]
    fn test_skill_registry_remove() {
        let mut reg = SkillRegistry::new();
        let skill = Skill {
            meta: SkillMeta {
                name: "temp".into(),
                description: "Temp skill".into(),
                trigger: "/temp".into(),
                model: None,
            },
            prompt_body: "temp".into(),
            source_dir: PathBuf::from("."),
        };
        reg.register(skill);
        assert_eq!(reg.len(), 1);
        assert!(reg.remove("temp"));
        assert_eq!(reg.len(), 0);
        assert!(!reg.has_trigger("/temp"));
    }

    #[test]
    fn test_render_prompt() {
        let skill = Skill {
            meta: SkillMeta {
                name: "test".into(),
                description: "Test".into(),
                trigger: "/test".into(),
                model: None,
            },
            prompt_body: "You are a testing assistant.".into(),
            source_dir: PathBuf::from("."),
        };
        let rendered = skill.render_prompt("run tests");
        assert!(rendered.contains("testing assistant"));
        assert!(rendered.contains("run tests"));
    }

    #[test]
    fn test_builtin_skills_loaded() {
        let skills = builtin_skills();
        assert!(skills.len() >= 2);
        let triggers: Vec<&str> = skills.iter().map(|s| s.meta.trigger.as_str()).collect();
        assert!(triggers.contains(&"/help"));
        assert!(triggers.contains(&"/status"));
    }

    #[test]
    fn test_frontmatter_with_quoted_values() {
        let content = r#"---
name: "explain"
description: "Explain code in detail"
trigger: "/explain"
---
Body."#;

        let skill = parse_skill(content, PathBuf::from(".")).unwrap();
        assert_eq!(skill.meta.name, "explain");
        assert_eq!(skill.meta.trigger, "/explain");
    }
}
