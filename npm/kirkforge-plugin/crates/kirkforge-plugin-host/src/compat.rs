//! Compatibility loader for legacy `.claude/skills/` directories.
//!
//! Each directory containing a `SKILL.md` is treated as a read-only plugin
//! with a single skill capability. The skill trigger is derived from the
//! directory name; the prompt is the content of `SKILL.md`.
//!
//! This lets existing skill directories continue to work after the move to
//! the plugin registry.

use kirkforge_plugin::{Capability, LoadedPlugin, PluginManifest, TrustTier};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Load a skill directory as a read-only plugin.
///
/// Returns `None` if the directory has no `SKILL.md`.
pub fn load_skill_dir(path: &Path) -> anyhow::Result<Option<LoadedPlugin>> {
    let skill_file = path.join("SKILL.md");
    if !skill_file.exists() {
        return Ok(None);
    }

    let trigger = path
        .file_name()
        .and_then(|n| n.to_str())
        .map(|n| format!("/{n}"))
        .unwrap_or_else(|| "/skill".into());

    let prompt = std::fs::read_to_string(&skill_file)
        .map_err(|e| anyhow::anyhow!("cannot read {skill_file:?}: {e}"))?;

    let manifest = PluginManifest {
        name: trigger.trim_start_matches('/').to_string(),
        version: "0.1.0".into(),
        description: format!("Legacy skill from {}", path.display()),
        trust: TrustTier::ReadOnly,
        capabilities: vec![Capability::Skill {
            trigger: trigger.clone(),
            prompt: prompt.clone(),
            skill_file: Some(PathBuf::from("SKILL.md")),
            model_hint: None,
        }],
        public_key: None,
        metadata: HashMap::new(),
    };

    let mut skill_prompts = HashMap::new();
    skill_prompts.insert(trigger, prompt);

    Ok(Some(LoadedPlugin {
        manifest,
        root: path.to_path_buf(),
        skill_prompts,
        hooks: Vec::new(),
        verifiers: Vec::new(),
        tools: Vec::new(),
    }))
}

/// Load every skill directory under `skills_dir`.
pub fn load_skills_dir(skills_dir: &Path) -> anyhow::Result<Vec<LoadedPlugin>> {
    let mut out = Vec::new();
    if !skills_dir.exists() {
        return Ok(out);
    }

    for entry in std::fs::read_dir(skills_dir)
        .map_err(|e| anyhow::anyhow!("cannot read {}: {}", skills_dir.display(), e))?
        .flatten()
    {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        if path
            .file_name()
            .and_then(|n| n.to_str())
            .map(|n| n.starts_with('.'))
            .unwrap_or(false)
        {
            continue;
        }
        if let Some(plugin) = load_skill_dir(&path)? {
            out.push(plugin);
        }
    }

    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use kirkforge_plugin::Plugin;

    #[test]
    fn load_legacy_skill() {
        let tmp = tempfile::tempdir().unwrap();
        let skill_dir = tmp.path().join("my-skill");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(skill_dir.join("SKILL.md"), "Do the thing.").unwrap();

        let plugin = load_skill_dir(&skill_dir).unwrap().unwrap();
        assert_eq!(plugin.manifest.name, "my-skill");
        assert_eq!(plugin.manifest.trust, TrustTier::ReadOnly);
        assert_eq!(
            plugin.skill_prompt("/my-skill", ""),
            Some("Do the thing.".into())
        );
    }

    #[test]
    fn missing_skill_file_returns_none() {
        let tmp = tempfile::tempdir().unwrap();
        let skill_dir = tmp.path().join("empty");
        std::fs::create_dir_all(&skill_dir).unwrap();
        assert!(load_skill_dir(&skill_dir).unwrap().is_none());
    }

    #[test]
    fn load_skills_dir_iterates() {
        let tmp = tempfile::tempdir().unwrap();
        let skills = tmp.path().join("skills");
        for name in ["a", "b"] {
            let dir = skills.join(name);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("SKILL.md"), "prompt").unwrap();
        }

        let loaded = load_skills_dir(&skills).unwrap();
        assert_eq!(loaded.len(), 2);
    }
}
