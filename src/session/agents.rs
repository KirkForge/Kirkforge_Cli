//! Dynamic agent loader for `.claude/agents/*.md` (WO 39.3).
//!
//! Claude agent files are markdown with YAML-like frontmatter (the same
//! shape as `.claude/skills/SKILL.md`) and a body that is the agent's
//! system prompt:
//!
//! ```markdown
//! ---
//! name: code-reviewer
//! description: Reviews code for bugs and style
//! tools: Read, Grep, Glob, Bash
//! model: claude-sonnet-4
//! ---
//! You are a senior code reviewer...
//! ```
//!
//! The loader reuses the frontmatter split convention from
//! [`crate::session::skills`], then maps Claude tool names to kf-code
//! native tool names via [`CLAUDE_TOOL_ALIASES`] before the allowlist
//! is applied in `task_spawner`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// A loaded Claude agent definition.
#[derive(Debug, Clone)]
pub struct AgentDef {
    /// Short name (the frontmatter `name`, also the `task` persona arg).
    pub name: String,
    /// One-line description shown in the `task` tool listing.
    pub description: String,
    /// System prompt (the markdown body after the frontmatter).
    pub system_prompt: String,
    /// Claude tool names from the frontmatter `tools` field, parsed
    /// from a comma-list. Translated to native names at filter time.
    pub tools: Vec<String>,
    /// Optional per-agent model override (frontmatter `model`).
    pub model: Option<String>,
}

/// Claude → kf-code tool-name alias table.
///
/// Applied to agent `tools` frontmatter and command allowed-tools lists.
/// Unknown names pass through unchanged (forward-compat with new Claude
/// tool names — a future Claude tool with no native equivalent is
/// dropped at the allowlist filter, not here).
// ponytail: const table, not a HashMap — 12 entries, linear scan is
// faster than a hashmap lookup at this size and avoids the const-HashMap
// dance. Add `Agent` → `task` as well so both Claude agent invocations
// map to the single subagent tool. upgrade path: if the table grows
// past ~30, swap to phf::Map.
pub const CLAUDE_TOOL_ALIASES: &[(&str, &str)] = &[
    ("Read", "read_file"),
    ("Write", "write_file"),
    ("Edit", "edit_file"),
    ("MultiEdit", "edit_file"),
    ("Bash", "bash"),
    ("Glob", "glob"),
    ("Grep", "grep"),
    ("WebFetch", "web_fetch"),
    ("WebSearch", "web_search"),
    ("NotebookEdit", "notebook_edit"),
    ("TodoWrite", "todo"),
    ("Task", "task"),
    ("Agent", "task"),
];

/// Translate a single Claude tool name to its kf-code native name.
/// Unknown names pass through unchanged.
pub fn alias_for(claude_name: &str) -> &str {
    CLAUDE_TOOL_ALIASES
        .iter()
        .find(|(c, _)| *c == claude_name)
        .map(|(_, n)| *n)
        .unwrap_or(claude_name)
}

/// Translate a list of Claude tool names to native names, deduped and
/// preserving first-seen order (so `Edit, MultiEdit` → `["edit_file"]`).
pub fn translate_tool_list(claude_names: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for n in claude_names {
        let native = alias_for(n.trim()).to_string();
        if !out.contains(&native) {
            out.push(native);
        }
    }
    out
}

/// The system-prompt suffix appended whenever any Claude artifact is
/// loaded, so the model's prose references to "use Read" map to the
/// native tool name. Prose references can't be rewritten reliably
/// (the model may say "read the file" without meaning the tool), so
/// we append a mapping paragraph instead of rewriting.
pub fn claude_alias_suffix() -> String {
    let pairs: Vec<String> = CLAUDE_TOOL_ALIASES
        .iter()
        .map(|(c, n)| format!("{c}={n}"))
        .collect();
    format!(
        "\n\n## Tool-name aliases\n\
         You are running under kf-code, which uses native tool names. \
         Claude tool names in your instructions map as follows: \
         {}. When you are told to use a Claude tool name, call the \
         corresponding native tool.",
        pairs.join(", ")
    )
}

/// Registry of loaded agents, indexed by the frontmatter `name`.
#[derive(Debug, Clone, Default)]
pub struct AgentRegistry {
    agents: HashMap<String, AgentDef>,
}

impl AgentRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Load every `.md` file under `dir`. Files without valid
    /// frontmatter are skipped with a `tracing::warn!` (not a hard
    /// error — one bad agent file must not break the session).
    ///
    /// Trust gate: when `trust_workspace` is false AND `dir` is not
    /// under the canonical data directory, the load is refused. The
    /// workspace `.claude/agents/` is model-writable in-session, so a
    /// dropped agent file can widen a subagent's toolset — the same
    /// threat model as workspace plugins (ADR-057 / WO 39.3 spec item 5).
    /// The operator opts in via `plugin_trust_workspace = true`.
    pub fn load_from_dir(
        &mut self,
        dir: &Path,
        trust_workspace: bool,
        data_dir: Option<&Path>,
    ) -> usize {
        if !trust_workspace {
            let is_data = data_dir.is_some_and(|d| dir.starts_with(d));
            if !is_data {
                tracing::warn!(
                    dir = %dir.display(),
                    "agent registry: workspace agent dir refused (plugin_trust_workspace=false); \
                     set plugin_trust_workspace=true to load workspace agents"
                );
                return 0;
            }
        }
        if !dir.is_dir() {
            return 0;
        }
        let mut count = 0;
        let walker = ignore::WalkBuilder::new(dir)
            .max_depth(Some(3))
            .standard_filters(false)
            .build();
        for result in walker {
            match result {
                Ok(entry) => {
                    if !entry.file_type().is_some_and(|t| t.is_file()) {
                        continue;
                    }
                    let path = entry.path();
                    if path.extension().is_none_or(|e| e != "md") {
                        continue;
                    }
                    match load_agent_file(path) {
                        Ok(agent) => {
                            self.agents.insert(agent.name.clone(), agent);
                            count += 1;
                        }
                        Err(e) => {
                            tracing::warn!(
                                path = %path.display(),
                                error = %e,
                                "failed to load agent file"
                            );
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!(error = %e, "failed to walk agents directory");
                }
            }
        }
        count
    }

    /// Register a programmatically-constructed agent (tests, builtins).
    pub fn register(&mut self, agent: AgentDef) {
        self.agents.insert(agent.name.clone(), agent);
    }

    /// Look up an agent by name (the `task` persona arg).
    pub fn get(&self, name: &str) -> Option<&AgentDef> {
        self.agents.get(name)
    }

    /// All registered agents, for the `task` tool description.
    pub fn all(&self) -> Vec<&AgentDef> {
        self.agents_sorted()
    }

    fn agents_sorted(&self) -> Vec<&AgentDef> {
        let mut v: Vec<&AgentDef> = self.agents.values().collect();
        v.sort_by(|a, b| a.name.cmp(&b.name));
        v
    }

    /// Number of registered agents.
    pub fn len(&self) -> usize {
        self.agents.len()
    }

    pub fn is_empty(&self) -> bool {
        self.agents.is_empty()
    }

    /// Compose the `task` tool description suffix listing discovered
    /// agents, or the empty string when none are registered.
    pub fn description_suffix(&self) -> String {
        if self.is_empty() {
            return String::new();
        }
        let lines: Vec<String> = self
            .agents_sorted()
            .iter()
            .map(|a| format!("  - {}: {}", a.name, a.description))
            .collect();
        format!("\n\nDiscovered agents (persona=NAME): {}", lines.join("; "))
    }
}

/// Parse one `.claude/agents/*.md` file into an [`AgentDef`].
pub fn load_agent_file(path: &Path) -> anyhow::Result<AgentDef> {
    let content = std::fs::read_to_string(path)?;
    parse_agent(&content)
}

/// Parse agent markdown content (frontmatter + body) into an [`AgentDef`].
///
/// Frontmatter keys: `name` (required), `description`, `tools`
/// (comma-list), `model`. The body after the closing `---` is the
/// system prompt.
pub fn parse_agent(content: &str) -> anyhow::Result<AgentDef> {
    let content = content.trim();
    if !content.starts_with("---") {
        anyhow::bail!("agent file must start with '---' frontmatter delimiter");
    }
    let after_first = content["---".len()..].trim();
    let end_idx = after_first
        .find("\n---")
        .ok_or_else(|| anyhow::anyhow!("agent file missing closing '---' delimiter"))?;
    let frontmatter_str = &after_first[..end_idx];
    let body = after_first[end_idx + "\n---".len()..].trim().to_string();

    let mut name = String::new();
    let mut description = String::new();
    let mut tools: Vec<String> = Vec::new();
    let mut model: Option<String> = None;

    for line in frontmatter_str.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some(colon_idx) = line.find(':') else {
            continue;
        };
        let key = line[..colon_idx].trim();
        let value = line[colon_idx + 1..].trim().trim_matches('"');
        match key {
            "name" => name = value.to_string(),
            "description" => description = value.to_string(),
            "tools" => {
                tools = value
                    .split([',', ' '])
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect();
            }
            "model" => model = Some(value.to_string()),
            _ => {}
        }
    }

    if name.is_empty() {
        anyhow::bail!("agent file missing required 'name' field in frontmatter");
    }

    Ok(AgentDef {
        name,
        description,
        system_prompt: body,
        tools,
        model,
    })
}

/// Build the subagent prompt for a known agent: the agent's system
/// prompt as preamble, the alias suffix, then the user task.
///
/// This extends the `build_task_prompt` seam (WO 39.3): the caller
/// checks the registry first, and for a hit routes here instead of the
/// hardcoded persona match.
pub fn build_agent_prompt(agent: &AgentDef, task: &str) -> String {
    let suffix = claude_alias_suffix();
    format!(
        "{system_prompt}{suffix}\n\nTask: {task}",
        system_prompt = agent.system_prompt,
        suffix = suffix,
    )
}

/// Shared, lazily-loaded registry handle.
///
/// `all_tools` and `InProcessTaskSpawner` both need the registry; a
/// single `OnceLock` keeps them consistent and avoids re-reading the
/// directory on every `task` call. The load is best-effort — a missing
/// or empty `.claude/agents/` yields an empty registry, not an error.
static GLOBAL_REGISTRY: std::sync::OnceLock<Arc<AgentRegistry>> = std::sync::OnceLock::new();

/// Load the global registry from `.claude/agents` (workspace) once.
/// Returns an empty registry if the dir is absent or refused by the
/// trust gate.
pub fn global_registry(trust_workspace: bool) -> Arc<AgentRegistry> {
    GLOBAL_REGISTRY
        .get_or_init(|| {
            let mut reg = AgentRegistry::new();
            let dir = PathBuf::from(".claude/agents");
            reg.load_from_dir(&dir, trust_workspace, None);
            Arc::new(reg)
        })
        .clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agent_md(body: &str) -> String {
        format!("---\n{body}\n---\nYou are a test agent.")
    }

    #[test]
    fn parse_agent_basic() {
        let content =
            agent_md("name: reviewer\ndescription: reviews code\ntools: Read, Grep\nmodel: fast");
        let a = parse_agent(&content).unwrap();
        assert_eq!(a.name, "reviewer");
        assert_eq!(a.description, "reviews code");
        assert_eq!(a.tools, vec!["Read", "Grep"]);
        assert_eq!(a.model.as_deref(), Some("fast"));
        assert!(a.system_prompt.contains("test agent"));
    }

    #[test]
    fn parse_agent_missing_name_fails() {
        let content = agent_md("description: no name here");
        assert!(parse_agent(&content).is_err());
    }

    #[test]
    fn parse_agent_no_frontmatter_fails() {
        assert!(parse_agent("just body").is_err());
    }

    #[test]
    fn parse_agent_missing_closing_delimiter_fails() {
        let bad = "---\nname: x\nBody without close.";
        assert!(parse_agent(bad).is_err());
    }

    #[test]
    fn parse_agent_empty_tools_yields_empty_vec() {
        let content = agent_md("name: x");
        let a = parse_agent(&content).unwrap();
        assert!(a.tools.is_empty());
    }

    #[test]
    fn alias_for_maps_known_names() {
        assert_eq!(alias_for("Read"), "read_file");
        assert_eq!(alias_for("Bash"), "bash");
        assert_eq!(alias_for("Task"), "task");
        assert_eq!(alias_for("Agent"), "task");
    }

    #[test]
    fn alias_for_passes_unknown_through() {
        assert_eq!(alias_for("future_tool"), "future_tool");
        assert_eq!(alias_for(""), "");
    }

    #[test]
    fn translate_tool_list_dedupes_edit_multiedit() {
        let names = vec![
            "Edit".to_string(),
            "MultiEdit".to_string(),
            "Read".to_string(),
        ];
        let out = translate_tool_list(&names);
        assert_eq!(out, vec!["edit_file", "read_file"]);
    }

    #[test]
    fn translate_tool_list_task_passthrough() {
        let out = translate_tool_list(&["Task".to_string()]);
        assert_eq!(out, vec!["task"]);
    }

    #[test]
    fn translate_tool_list_unknown_passes_through() {
        let out = translate_tool_list(&["MysteryTool".to_string()]);
        assert_eq!(out, vec!["MysteryTool"]);
    }

    #[test]
    fn claude_alias_suffix_lists_all_pairs() {
        let s = claude_alias_suffix();
        assert!(s.contains("Read=read_file"));
        assert!(s.contains("Bash=bash"));
        assert!(s.contains("Task=task"));
        assert!(s.contains("Tool-name aliases"));
    }

    #[test]
    fn build_agent_prompt_prepends_system_prompt_and_suffix() {
        let a = AgentDef {
            name: "x".into(),
            description: "d".into(),
            system_prompt: "You are a senior reviewer.".into(),
            tools: vec!["Read".into()],
            model: None,
        };
        let p = build_agent_prompt(&a, "review this PR");
        assert!(p.contains("senior reviewer"));
        assert!(p.contains("review this PR"));
        assert!(p.contains("Tool-name aliases"));
    }

    #[test]
    fn registry_get_returns_registered_agent() {
        let mut reg = AgentRegistry::new();
        reg.register(AgentDef {
            name: "rev".into(),
            description: "d".into(),
            system_prompt: "body".into(),
            tools: vec![],
            model: None,
        });
        assert!(reg.get("rev").is_some());
        assert!(reg.get("nope").is_none());
        assert_eq!(reg.len(), 1);
    }

    #[test]
    fn registry_description_suffix_empty_when_no_agents() {
        let reg = AgentRegistry::new();
        assert!(reg.description_suffix().is_empty());
    }

    #[test]
    fn registry_description_suffix_lists_agents() {
        let mut reg = AgentRegistry::new();
        reg.register(AgentDef {
            name: "beta".into(),
            description: "beta agent".into(),
            system_prompt: "b".into(),
            tools: vec![],
            model: None,
        });
        reg.register(AgentDef {
            name: "alpha".into(),
            description: "alpha agent".into(),
            system_prompt: "a".into(),
            tools: vec![],
            model: None,
        });
        let s = reg.description_suffix();
        assert!(s.contains("alpha: alpha agent"));
        assert!(s.contains("beta: beta agent"));
        // sorted: alpha precedes beta
        assert!(s.find("alpha").unwrap() < s.find("beta").unwrap());
    }

    #[test]
    fn load_from_dir_refuses_workspace_when_trust_off() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.md"), agent_md("name: a")).unwrap();
        let mut reg = AgentRegistry::new();
        let n = reg.load_from_dir(dir.path(), false, None);
        assert_eq!(n, 0);
        assert!(reg.is_empty());
    }

    #[test]
    fn load_from_dir_loads_workspace_when_trust_on() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.md"), agent_md("name: a\ntools: Read")).unwrap();
        let mut reg = AgentRegistry::new();
        let n = reg.load_from_dir(dir.path(), true, None);
        assert_eq!(n, 1);
        assert!(reg.get("a").is_some());
    }

    #[test]
    fn load_from_dir_loads_data_dir_even_when_trust_off() {
        let data = tempfile::tempdir().unwrap();
        let agents_subdir = data.path().join("agents");
        std::fs::create_dir_all(&agents_subdir).unwrap();
        std::fs::write(agents_subdir.join("b.md"), agent_md("name: b\ntools: Grep")).unwrap();
        let mut reg = AgentRegistry::new();
        let n = reg.load_from_dir(&agents_subdir, false, Some(data.path()));
        assert_eq!(n, 1, "data-dir agents load regardless of trust_workspace");
        assert!(reg.get("b").is_some());
    }

    #[test]
    fn load_from_dir_missing_dir_is_zero_not_error() {
        let mut reg = AgentRegistry::new();
        let n = reg.load_from_dir(Path::new("/nonexistent/agents"), true, None);
        assert_eq!(n, 0);
    }

    #[test]
    fn load_from_dir_skips_non_md_files() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.md"), agent_md("name: a")).unwrap();
        std::fs::write(dir.path().join("notes.txt"), "ignore me").unwrap();
        std::fs::write(dir.path().join("README.md"), "# not an agent").unwrap();
        let mut reg = AgentRegistry::new();
        let n = reg.load_from_dir(dir.path(), true, None);
        // a.md loads; README.md fails frontmatter parse (no `name`)
        assert_eq!(n, 1);
        assert!(reg.get("a").is_some());
    }

    #[test]
    fn load_from_dir_skips_invalid_frontmatter_warns_not_errors() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("good.md"), agent_md("name: good")).unwrap();
        std::fs::write(dir.path().join("bad.md"), "no frontmatter at all").unwrap();
        let mut reg = AgentRegistry::new();
        let n = reg.load_from_dir(dir.path(), true, None);
        assert_eq!(n, 1, "good loads, bad skipped");
        assert!(reg.get("good").is_some());
    }

    #[test]
    fn alias_table_covers_all_spec_entries() {
        let spec: &[&str] = &[
            "Read",
            "Write",
            "Edit",
            "MultiEdit",
            "Bash",
            "Glob",
            "Grep",
            "WebFetch",
            "WebSearch",
            "NotebookEdit",
            "TodoWrite",
            "Task",
            "Agent",
        ];
        for name in spec {
            assert!(
                CLAUDE_TOOL_ALIASES.iter().any(|(c, _)| c == name),
                "spec alias {name} missing from table"
            );
        }
    }

    #[test]
    fn global_registry_returns_same_handle() {
        // Two calls return the same Arc (OnceLock dedupes).
        let a = global_registry(true);
        let b = global_registry(true);
        assert!(Arc::ptr_eq(&a, &b));
    }
}
