//! Persistent semantic memory system.
//!
//! Stores factual knowledge about the user, project, and past interactions
//! as markdown files with YAML frontmatter. Injected into the system prompt
//! so the model "remembers" across sessions.
//!
//! # File format
//!
//! Each memory is one `.md` file in the memory directory:
//!
//! ```markdown
//! ---
//! name: user_profile
//! description: Kirk's development environment and preferences
//! metadata:
//!   type: user | project | feedback | reference
//! ---
//!
//! The fact content goes here — one or more paragraphs.
//! **Why:** reasons. **How to apply:** practical guidance.
//! ```
//!
//! The `name` field serves as a unique slug. The `description` is indexed
//! for search. The `type` metadata controls where and how the fact is
//! injected into prompts.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// A single memory fact with frontmatter metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryFact {
    /// Unique slug (kebab-case).
    pub name: String,
    /// One-line summary for search indexing.
    pub description: String,
    /// The full fact body.
    pub body: String,
    /// Metadata key-value pairs from frontmatter.
    pub metadata: std::collections::HashMap<String, String>,
}

/// The on-disk memory store. Files live in
/// `~/.local/share/kirkforge/memory/`.
#[derive(Debug, Clone)]
pub struct MemoryStore {
    root: PathBuf,
}

impl MemoryStore {
    /// Open (or create) the memory store at the given directory.
    pub fn open(root: PathBuf) -> std::io::Result<Self> {
        std::fs::create_dir_all(&root)?;
        Ok(Self { root })
    }

    /// Default store at `~/.local/share/kirkforge/memory/`.
    pub fn default_store() -> Self {
        let data_dir = crate::session::data_dir()
            .unwrap_or_else(|_| PathBuf::from(".kirkforge"));
        Self::open(data_dir.join("memory")).expect("memory store directory")
    }

    /// Read all facts from disk, sorted by name.
    pub fn all(&self) -> Vec<MemoryFact> {
        let mut facts = Vec::new();
        if let Ok(entries) = std::fs::read_dir(&self.root) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().map_or(true, |e| e != "md") {
                    continue;
                }
                if let Some(fact) = self.read_one(&path) {
                    facts.push(fact);
                }
            }
        }
        facts.sort_by(|a, b| a.name.cmp(&b.name));
        facts
    }

    /// Get a single fact by name slug.
    pub fn get(&self, name: &str) -> Option<MemoryFact> {
        let path = self.path_for(name);
        self.read_one(&path)
    }

    /// Add or update a fact. Returns the saved fact.
    pub fn upsert(&self, name: &str, description: &str, body: &str, meta_type: &str) -> std::io::Result<MemoryFact> {
        let mut metadata = std::collections::HashMap::new();
        metadata.insert("type".into(), meta_type.to_string());

        let fact = MemoryFact {
            name: name.to_string(),
            description: description.to_string(),
            body: body.to_string(),
            metadata,
        };

        self.write_one(&fact)?;
        Ok(fact)
    }

    /// Delete a fact by name. Returns true if it existed.
    pub fn delete(&self, name: &str) -> std::io::Result<bool> {
        let path = self.path_for(name);
        if path.exists() {
            std::fs::remove_file(&path)?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    /// Search facts by keyword in name and description.
    /// Returns facts sorted by relevance (exact name match first, then
    /// description substring matches).
    pub fn search(&self, query: &str) -> Vec<MemoryFact> {
        let query_lower = query.to_lowercase();
        let mut scored: Vec<(i32, MemoryFact)> = self
            .all()
            .into_iter()
            .filter_map(|f| {
                let name_lower = f.name.to_lowercase();
                let desc_lower = f.description.to_lowercase();

                let score: i32;
                if name_lower == query_lower {
                    score = 100;
                } else if name_lower.contains(&query_lower) {
                    score = 50;
                } else if desc_lower.contains(&query_lower) {
                    score = 25;
                } else {
                    return None;
                }
                Some((score, f))
            })
            .collect();

        scored.sort_by_key(|(s, _)| -(*s));
        scored.into_iter().map(|(_, f)| f).collect()
    }

    /// Render all facts as a prompt-insertion block.
    ///
    /// Returns an empty string when there are no facts, so the caller can
    /// skip adding `<memory>` tags entirely.
    pub fn to_prompt_block(&self) -> String {
        let facts = self.all();
        if facts.is_empty() {
            return String::new();
        }

        // Deduplicate by type category for compact injection
        let mut block = String::from(
            "<!-- MEMORY: persisted facts from past sessions -->\n",
        );

        let mut seen_types = std::collections::HashSet::new();
        for fact in &facts {
            seen_types.insert(fact.metadata.get("type").cloned().unwrap_or_default());
        }

        for fact in &facts {
            let mtype = fact.metadata.get("type").cloned().unwrap_or_default();
            block.push_str(&format!("- [{}] {}: {}\n", mtype, fact.name, fact.description));
        }

        block
    }

    /// Build MEMORY.md index file.
    ///
    /// Writes `MEMORY.md` as a one-line-per-fact index so the model can
    /// quickly see what's stored without reading every file.
    pub fn write_index(&self) -> std::io::Result<()> {
        let facts = self.all();
        let mut content = String::from("# Memory Index\n\n");
        for fact in &facts {
            content.push_str(&format!(
                "- [{}]({}.md) — {}\n",
                fact.description, fact.name, fact.name
            ));
        }
        std::fs::write(self.root.join("MEMORY.md"), content)
    }

    // --- internal helpers ---

    fn path_for(&self, name: &str) -> PathBuf {
        self.root.join(format!("{}.md", sanitize_slug(name)))
    }

    fn read_one(&self, path: &Path) -> Option<MemoryFact> {
        let content = std::fs::read_to_string(path).ok()?;
        let (frontmatter, body) = parse_frontmatter(&content)?;

        let name = frontmatter.get("name").cloned().unwrap_or_else(|| {
            path.file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("unknown")
                .to_string()
        });
        let description = frontmatter
            .get("description")
            .cloned()
            .unwrap_or_else(|| name.clone());

        let mut metadata = std::collections::HashMap::new();
        if let Some(meta_str) = frontmatter.get("metadata") {
            // Try to parse as simple inline map. Frontmatter YAML can
            // embed metadata as a block — we do a shallow parse.
            for line in meta_str.lines() {
                let line = line.trim();
                if let Some((k, v)) = line.split_once(':') {
                    metadata.insert(k.trim().to_string(), v.trim().to_string());
                }
            }
        }

        Some(MemoryFact {
            name,
            description,
            body,
            metadata,
        })
    }

    fn write_one(&self, fact: &MemoryFact) -> std::io::Result<()> {
        let path = self.path_for(&fact.name);

        let mut metadata_block = String::new();
        if !fact.metadata.is_empty() {
            metadata_block.push_str("metadata:\n");
            let mut keys: Vec<&String> = fact.metadata.keys().collect();
            keys.sort();
            for k in keys {
                if let Some(v) = fact.metadata.get(k) {
                    metadata_block.push_str(&format!("  {}: {}\n", k, v));
                }
            }
        }

        let frontmatter = format!(
            "---\nname: {}\ndescription: {}\n{}\n---\n\n{}",
            fact.name,
            fact.description,
            metadata_block.trim_end(),
            fact.body
        );

        std::fs::write(&path, frontmatter)
    }
}

/// Parse YAML frontmatter from a markdown document.
///
/// Returns `(frontmatter_map, body_text)` or `None` if no valid frontmatter
/// is found. Handles the subset of YAML used by memory files (simple
/// key: value pairs and one level of nested `metadata:` block).
pub fn parse_frontmatter(content: &str) -> Option<(std::collections::HashMap<String, String>, String)> {
    let content = content.trim();
    if !content.starts_with("---") {
        return None;
    }

    let rest = &content[3..];
    let end = rest.find("---")?;

    let frontmatter_text = &rest[..end].trim();
    let body = &rest[end + 3..];

    let mut map = std::collections::HashMap::new();
    let mut in_metadata = false;
    let mut metadata_lines = Vec::new();

    for line in frontmatter_text.lines() {
        // Skip completely empty lines
        if line.trim().is_empty() {
            continue;
        }

        // Handle metadata sub-keys: lines indented with 2 spaces or a tab
        if in_metadata {
            if let Some(indented) = line.strip_prefix("  ")
                .or_else(|| line.strip_prefix('\t'))
            {
                let trimmed = indented.trim();
                if let Some((k, v)) = trimmed.split_once(':') {
                    metadata_lines.push(format!("{}: {}", k.trim(), v.trim()));
                }
            } else {
                in_metadata = false;
                // Fall through: this line is a new top-level key
                if let Some((key, value)) = line.trim().split_once(':') {
                    let key = key.trim().to_string();
                    let value = value.trim().to_string();
                    if key == "metadata" && value.is_empty() {
                        in_metadata = true;
                        continue;
                    }
                    map.insert(key, value);
                }
            }
            continue;
        }

        let trimmed = line.trim();
        if let Some((key, value)) = trimmed.split_once(':') {
            let key = key.trim().to_string();
            let value = value.trim().to_string();
            if key == "metadata" && value.is_empty() {
                in_metadata = true;
                continue;
            }
            map.insert(key, value);
        }
    }

    if !metadata_lines.is_empty() {
        map.insert("metadata".into(), metadata_lines.join("\n"));
    }

    Some((map, body.trim().to_string()))
}

/// Convert a description to a kebab-case slug.
pub fn slugify_description(desc: &str) -> String {
    desc.to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '-' })
        .collect::<String>()
        .split('-')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("-")
}

/// Sanitize a name for use as a filename slug.
fn sanitize_slug(name: &str) -> String {
    slugify_description(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_store() -> MemoryStore {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().to_path_buf();
        std::mem::forget(tmp); // keep the dir alive — cleaned on process exit
        MemoryStore::open(path).unwrap()
    }

    #[test]
    fn test_crud_cycle() {
        let store = temp_store();
        store
            .upsert("test-fact", "A test fact", "The body content.", "user")
            .unwrap();

        let facts = store.all();
        assert_eq!(facts.len(), 1);
        assert_eq!(facts[0].name, "test-fact");
        assert_eq!(facts[0].body, "The body content.");

        let found = store.get("test-fact").unwrap();
        assert_eq!(found.description, "A test fact");

        let deleted = store.delete("test-fact").unwrap();
        assert!(deleted);
        assert!(store.get("test-fact").is_none());
    }

    #[test]
    fn test_upsert_overwrites() {
        let store = temp_store();
        store
            .upsert("test-fact", "v1", "body v1", "user")
            .unwrap();
        store
            .upsert("test-fact", "v2", "body v2", "user")
            .unwrap();

        let facts = store.all();
        assert_eq!(facts.len(), 1);
        assert_eq!(facts[0].description, "v2");
        assert_eq!(facts[0].body, "body v2");
    }

    #[test]
    fn test_search_finds_by_name() {
        let store = temp_store();
        store
            .upsert("setup-notes", "Machine setup", "content", "project")
            .unwrap();
        store
            .upsert("user-profile", "User info", "content", "user")
            .unwrap();

        let results = store.search("setup");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].name, "setup-notes");
    }

    #[test]
    fn test_search_finds_by_description() {
        let store = temp_store();
        store
            .upsert("fact1", "Kubuntu setup guide", "body", "project")
            .unwrap();
        store
            .upsert("fact2", "Rust toolchain", "body", "reference")
            .unwrap();

        let results = store.search("kubuntu");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].name, "fact1");
    }

    #[test]
    fn test_search_is_case_insensitive() {
        let store = temp_store();
        store
            .upsert("api-keys", "External API keys", "body", "reference")
            .unwrap();

        let results = store.search("API");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].name, "api-keys");
    }

    #[test]
    fn test_search_no_match_returns_empty() {
        let store = temp_store();
        store
            .upsert("fact1", "Something", "body", "user")
            .unwrap();
        assert!(store.search("nonexistent").is_empty());
    }

    #[test]
    fn test_to_prompt_block_empty() {
        let store = temp_store();
        assert_eq!(store.to_prompt_block(), "");
    }

    #[test]
    fn test_to_prompt_block_renders_facts() {
        let store = temp_store();
        store
            .upsert("setup", "Setup guide", "content", "project")
            .unwrap();
        store
            .upsert("user", "User profile", "content", "user")
            .unwrap();

        // Verify the facts were stored and can be read back
        let facts = store.all();
        assert_eq!(facts.len(), 2);
        assert_eq!(facts[0].name, "setup");
        assert_eq!(facts[1].name, "user");

        let block = store.to_prompt_block();
        assert!(block.contains("[project]"), "block: {}", block);
        assert!(block.contains("user"), "block: {}", block);
    }

    #[test]
    fn test_parse_frontmatter() {
        let input = "---\nname: test\nkey: value\n---\nBody text here.";
        let (map, body) = parse_frontmatter(input).unwrap();
        assert_eq!(map.get("name").unwrap(), "test");
        assert_eq!(map.get("key").unwrap(), "value");
        assert_eq!(body, "Body text here.");
    }

    #[test]
    fn test_parse_frontmatter_with_nested_metadata() {
        let input = "---\nname: test\ndescription: desc\nmetadata:\n  type: user\n---\n\nbody";
        let result = parse_frontmatter(input);
        assert!(result.is_some(), "parse_frontmatter returned None for valid input");
        let (map, body) = result.unwrap();
        assert_eq!(map.get("name").unwrap(), "test");
        assert_eq!(map.get("description").unwrap(), "desc");
        let meta_val = map.get("metadata").expect("metadata key should exist");
        assert_eq!(meta_val, "type: user", "metadata value mismatch: {:?}", map);
        assert_eq!(body.trim(), "body");
    }

    #[test]
    fn test_parse_frontmatter_no_frontmatter() {
        let input = "Just a plain markdown file.";
        assert!(parse_frontmatter(input).is_none());
    }

    #[test]
    fn test_slugify_description() {
        assert_eq!(slugify_description("My Setup Guide!"), "my-setup-guide");
        assert_eq!(slugify_description("Rust -- Toolchain"), "rust-toolchain");
        assert_eq!(slugify_description("simple"), "simple");
    }

    #[test]
    fn test_delete_nonexistent() {
        let store = temp_store();
        assert!(!store.delete("nope").unwrap());
    }

    #[test]
    fn test_metadata_roundtrip() {
        let store = temp_store();
        store.upsert("test-fact", "desc", "body", "user").unwrap();
        let facts = store.all();
        assert_eq!(facts.len(), 1);
        let mtype = facts[0].metadata.get("type").cloned().unwrap_or_default();
        assert_eq!(mtype, "user", "metadata: {:?}", facts[0].metadata);
    }
}
