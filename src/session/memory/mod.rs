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
    pub fn default_store() -> std::io::Result<Self> {
        let data_dir = crate::session::data_dir().unwrap_or_else(|_| PathBuf::from(".kirkforge"));
        Self::open(data_dir.join("memory"))
    }

    /// Read all facts from disk, sorted by name.
    pub fn all(&self) -> Vec<MemoryFact> {
        let mut facts = Vec::new();
        if let Ok(entries) = std::fs::read_dir(&self.root) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().is_none_or(|e| e != "md") {
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
    pub fn upsert(
        &self,
        name: &str,
        description: &str,
        body: &str,
        meta_type: &str,
    ) -> std::io::Result<MemoryFact> {
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
        self.to_prompt_block_for_facts(&self.all())
    }

    /// Render a selected subset of facts as a prompt-insertion block.
    pub fn to_prompt_block_for_facts(&self, facts: &[MemoryFact]) -> String {
        if facts.is_empty() {
            return String::new();
        }

        let mut block = String::from("<!-- MEMORY: persisted facts from past sessions -->\n");
        for fact in facts {
            let mtype = fact.metadata.get("type").cloned().unwrap_or_default();
            block.push_str(&format!(
                "- [{}] {}: {}\n",
                mtype, fact.name, fact.description
            ));
        }

        block
    }

    /// Score all facts against `context` using TF-IDF-style keyword matching,
    /// then return the top-N subset that fits inside `max_tokens`.
    ///
    /// The score is purely lexical: terms from the context are matched against
    /// the name, description, and body of every fact. Inverse document
    /// frequency prevents ubiquitous words from drowning out rare, specific
    /// terms. Ties are broken by fact name for determinism.
    ///
    /// Facts are selected greedily by score until the estimated token count
    /// (chars / 4) reaches `max_tokens`. `top_n` caps how many facts are
    /// considered regardless of budget.
    pub fn select_for_context(
        &self,
        context: &str,
        max_tokens: usize,
        top_n: usize,
    ) -> Vec<MemoryFact> {
        let corpus = self.all();
        if corpus.is_empty() || context.is_empty() {
            return Vec::new();
        }

        let idf = compute_idf(&corpus);
        let query_terms = tokenize(context);
        if query_terms.is_empty() {
            return Vec::new();
        }

        let mut scored: Vec<(f64, MemoryFact)> = corpus
            .into_iter()
            .map(|fact| {
                let score = score_fact(&fact, &query_terms, &idf);
                (score, fact)
            })
            .collect();

        scored.sort_by(|a, b| {
            b.0.partial_cmp(&a.0)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.1.name.cmp(&b.1.name))
        });

        let mut selected = Vec::new();
        let mut tokens_used = 0usize;
        for (score, fact) in scored.into_iter().take(top_n) {
            if score <= 0.0 {
                break;
            }
            let line = format!(
                "- [{}] {}: {}\n",
                fact.metadata.get("type").cloned().unwrap_or_default(),
                fact.name,
                fact.description
            );
            let est = line.len() / 4;
            if tokens_used + est > max_tokens && !selected.is_empty() {
                break;
            }
            tokens_used += est;
            selected.push(fact);
        }

        selected
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
                    metadata_block.push_str(&format!("  {k}: {v}\n"));
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
pub fn parse_frontmatter(
    content: &str,
) -> Option<(std::collections::HashMap<String, String>, String)> {
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
            if let Some(indented) = line.strip_prefix("  ").or_else(|| line.strip_prefix('\t')) {
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

/// Tokenise free text into lowercase, alphanumeric terms.
///
/// Drops one-character tokens and a small set of English stop words so
/// they don't dominate TF-IDF scoring.
fn tokenize(text: &str) -> Vec<String> {
    const STOP_WORDS: &[&str] = &[
        "the", "and", "for", "with", "this", "that", "you", "are", "use", "using", "from", "have",
        "has", "had", "was", "will", "can", "should", "must", "may", "would", "could", "about",
        "into", "over", "such", "than", "only", "some", "any", "each", "all", "but", "not", "also",
    ];

    text.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|s| s.len() > 1)
        .map(|s| s.to_string())
        .filter(|s| !STOP_WORDS.contains(&s.as_str()))
        .collect()
}

/// Compute inverse document frequency for each term in the corpus.
fn compute_idf(corpus: &[MemoryFact]) -> std::collections::HashMap<String, f64> {
    let n = corpus.len() as f64;
    let mut doc_freq: std::collections::HashMap<String, usize> = std::collections::HashMap::new();

    for fact in corpus {
        let terms = tokenize(&format!("{} {} {}", fact.name, fact.description, fact.body));
        let mut seen = std::collections::HashSet::new();
        for term in terms {
            if seen.insert(term.clone()) {
                *doc_freq.entry(term).or_insert(0) += 1;
            }
        }
    }

    doc_freq
        .into_iter()
        .map(|(term, df)| {
            let idf = (n / (1.0 + df as f64)).ln();
            (term, idf)
        })
        .collect()
}

/// Score a single fact against the query terms.
fn score_fact(
    fact: &MemoryFact,
    query_terms: &[String],
    idf: &std::collections::HashMap<String, f64>,
) -> f64 {
    let fact_terms = tokenize(&format!("{} {} {}", fact.name, fact.description, fact.body));
    let mut term_freq: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for term in fact_terms {
        *term_freq.entry(term).or_insert(0) += 1;
    }

    let mut score = 0.0;
    for term in query_terms {
        if let Some(&idf_val) = idf.get(term) {
            let tf = term_freq.get(term).copied().unwrap_or(0) as f64;
            score += tf * idf_val;
        }
    }

    // Small boost for exact name/description matches so highly relevant
    // facts don't lose to longer bodies that happen to contain the term.
    let name_lower = fact.name.to_lowercase();
    let desc_lower = fact.description.to_lowercase();
    for term in query_terms {
        if name_lower == term.as_str() {
            score += 5.0;
        } else if name_lower.contains(term) {
            score += 2.0;
        } else if desc_lower.contains(term) {
            score += 1.0;
        }
    }

    score
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
    fn test_open_fails_when_path_is_file() {
        let tmp = tempfile::tempdir().unwrap();
        let file_path = tmp.path().join("not-a-dir");
        std::fs::write(&file_path, "x").unwrap();
        // Opening a store at a file path should fail because create_dir_all
        // cannot turn a file into a directory.
        assert!(MemoryStore::open(file_path).is_err());
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
        store.upsert("test-fact", "v1", "body v1", "user").unwrap();
        store.upsert("test-fact", "v2", "body v2", "user").unwrap();

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
        store.upsert("fact1", "Something", "body", "user").unwrap();
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
        assert!(block.contains("[project]"), "block: {block}");
        assert!(block.contains("user"), "block: {block}");
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
        assert!(
            result.is_some(),
            "parse_frontmatter returned None for valid input"
        );
        let (map, body) = result.unwrap();
        assert_eq!(map.get("name").unwrap(), "test");
        assert_eq!(map.get("description").unwrap(), "desc");
        let meta_val = map.get("metadata").expect("metadata key should exist");
        assert_eq!(meta_val, "type: user", "metadata value mismatch: {map:?}");
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

    #[test]
    fn test_select_for_context_returns_relevant_fact() {
        let store = temp_store();
        store
            .upsert(
                "anyhow",
                "Use anyhow",
                "We use anyhow for errors, never unwrap in production.",
                "feedback",
            )
            .unwrap();
        store
            .upsert(
                "ratatui",
                "TUI crate",
                "This project uses ratatui for the terminal UI.",
                "project",
            )
            .unwrap();

        let selected =
            store.select_for_context("How should I handle errors in this repo?", 100, 10);
        assert!(!selected.is_empty(), "expected at least one fact");
        assert_eq!(
            selected[0].name, "anyhow",
            "expected anyhow fact first, got: {selected:?}"
        );
        assert!(
            !selected.iter().any(|f| f.name == "ratatui"),
            "irrelevant fact should not be selected"
        );
    }

    #[test]
    fn test_select_for_context_respects_top_n() {
        let store = temp_store();
        for i in 0..5 {
            store
                .upsert(
                    &format!("fact-{i}"),
                    &format!("description {i}"),
                    &format!("body {i} contains unique token xyzzy{i}"),
                    "project",
                )
                .unwrap();
        }

        let selected = store.select_for_context("xyzzy2 token", 500, 1);
        assert_eq!(selected.len(), 1, "top_n=1 should return one fact");
        assert_eq!(selected[0].name, "fact-2");
    }

    #[test]
    fn test_select_for_context_respects_token_budget() {
        let store = temp_store();
        for i in 0..5 {
            let body = if i < 3 {
                "body contains common token alpha"
            } else {
                "body contains other token beta"
            };
            store
                .upsert(
                    &format!("fact-{i}"),
                    &format!("description {i}"),
                    body,
                    "project",
                )
                .unwrap();
        }

        // Only the first three facts share "common", so they score > 0.
        // Each line is ~45 chars / 4 = ~11 tokens. Budget 15 should allow
        // the first fact only.
        let selected = store.select_for_context("common", 15, 10);
        assert_eq!(
            selected.len(),
            1,
            "budget should cap selection, got: {:?}",
            selected.len()
        );
    }

    #[test]
    fn test_select_for_context_empty_context_returns_nothing() {
        let store = temp_store();
        store.upsert("fact", "desc", "body", "project").unwrap();
        let selected = store.select_for_context("", 100, 10);
        assert!(selected.is_empty());
    }

    #[test]
    fn test_to_prompt_block_for_facts_subset() {
        let store = temp_store();
        store.upsert("a", "desc a", "body", "project").unwrap();
        store.upsert("b", "desc b", "body", "project").unwrap();

        let facts = store.all();
        let block = store.to_prompt_block_for_facts(&facts[..1]);
        assert!(block.contains("a"));
        assert!(!block.contains("b"));
    }
}
