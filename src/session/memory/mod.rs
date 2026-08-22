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

pub(crate) mod extract;

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
/// `~/.local/share/kf-code/memory/`.
#[derive(Debug, Clone)]
pub struct MemoryStore {
    root: PathBuf,
    max_facts: usize,
    /// Jaccard similarity at which a new fact is treated as a near-duplicate
    /// of an existing one and the insert is skipped. `>= 1.0` disables the
    /// gate. See `with_dedup_threshold`.
    dedup_threshold: f64,
}

impl MemoryStore {
    /// Open (or create) the memory store at the given directory.
    pub fn open(root: PathBuf) -> std::io::Result<Self> {
        std::fs::create_dir_all(&root)?;
        Ok(Self {
            root,
            max_facts: 200,
            dedup_threshold: 0.85,
        })
    }

    /// Default store at `~/.local/share/kf-code/memory/`.
    pub fn default_store() -> std::io::Result<Self> {
        let data_dir = crate::session::data_dir().unwrap_or_else(|_| PathBuf::from(".kf-code"));
        Self::open(data_dir.join("memory"))
    }

    /// Set the maximum number of facts before eviction kicks in.
    pub fn with_max_facts(mut self, cap: usize) -> Self {
        self.max_facts = cap;
        self
    }

    /// Set the near-duplicate dedup threshold (Jaccard similarity over the
    /// description + body token sets). A new fact whose similarity to any
    /// existing fact is `>= threshold` is skipped (the existing fact is
    /// returned untouched). Default `0.85`; pass `>= 1.0` to disable.
    pub fn with_dedup_threshold(mut self, threshold: f64) -> Self {
        self.dedup_threshold = threshold;
        self
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

    /// The memory store's root directory.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Read all facts with an mtime cache (WO 38.9 item 4). If the
    /// memory directory's mtime hasn't changed since the cache was
    /// populated, returns the cached facts without re-reading from
    /// disk. Returns `None` when the cache is cold or stale — the
    /// caller should fall back to `all()` and update the cache.
    pub fn all_cached(
        &self,
        cache: &mut Option<(std::time::SystemTime, Vec<MemoryFact>)>,
    ) -> Option<Vec<MemoryFact>> {
        let current_mtime = std::fs::metadata(&self.root).ok()?.modified().ok()?;
        if let Some((cached_mtime, ref cached_facts)) = cache {
            if *cached_mtime == current_mtime {
                return Some(cached_facts.clone());
            }
        }
        None
    }

    /// Score all facts (from a pre-loaded list) against `context` using
    /// TF-IDF keyword matching, then return the top-N subset that fits
    /// inside `max_tokens`. This is the same logic as
    /// `select_for_context` but accepts a pre-loaded fact list so the
    /// caller can use an mtime-cached fact set (WO 38.9 item 4).
    pub fn select_for_context_from(
        &self,
        facts: &[MemoryFact],
        context: &str,
        max_tokens: usize,
        top_n: usize,
    ) -> Vec<MemoryFact> {
        if facts.is_empty() || context.is_empty() {
            return Vec::new();
        }

        let idf = compute_idf(facts);
        let query_terms = tokenize(context);
        if query_terms.is_empty() {
            return Vec::new();
        }

        let mut scored: Vec<(f64, &MemoryFact)> = facts
            .iter()
            .map(|fact| {
                let score = score_fact(fact, &query_terms, &idf);
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
            let est = crate::session::prompt::count_tokens(&line);
            if tokens_used + est > max_tokens && !selected.is_empty() {
                break;
            }
            tokens_used += est;
            selected.push(fact.clone());
        }

        selected
    }

    /// Get a single fact by name slug.
    pub fn get(&self, name: &str) -> Option<MemoryFact> {
        let path = self.path_for(name);
        self.read_one(&path)
    }

    /// Add or update a fact. Returns the saved fact.
    ///
    /// When the total number of facts exceeds `max_facts` (default 200),
    /// the oldest fact (first alphabetically) is evicted. ponytail: O(n)
    /// scan; fine for 200 entries.
    // ponytail: upsert overwrites on name match. The FNV hash suffix makes collisions extremely unlikely but not impossible. Upgrade: append turn number to slug for full disambiguation.
    //
    // Near-duplicate gate: before writing, the new fact's description + body
    // are compared (token-set Jaccard) against every existing fact. If the
    // best score is `>= dedup_threshold` (default 0.85) the insert is skipped
    // and the existing fact is returned. ponytail: lexical Jaccard misses
    // synonym-only paraphrases ("user prefers rust" vs "Kirk likes Rust"
    // share no tokens) — those still accumulate as distinct facts. Upgrade
    // path: real embeddings if lexical dedup under-performs in practice; do
    // NOT add an embedding-model dep here (WO 28.15 failure criteria).
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

        // Near-duplicate gate: skip if an existing fact is lexically
        // near-identical. Disabled when threshold >= 1.0.
        if self.dedup_threshold < 1.0 {
            let new_tokens = token_set(&fact.description, &fact.body);
            if !new_tokens.is_empty() {
                for existing in self.all() {
                    let existing_tokens = token_set(&existing.description, &existing.body);
                    if jaccard(&new_tokens, &existing_tokens) >= self.dedup_threshold {
                        tracing::trace!(
                            existing = %existing.name,
                            new = %fact.name,
                            threshold = self.dedup_threshold,
                            "memory dedup: skipped near-duplicate insert"
                        );
                        return Ok(existing);
                    }
                }
            }
        }

        self.write_one(&fact)?;

        let facts = self.all();
        if facts.len() > self.max_facts {
            if let Some(oldest) = facts.first() {
                let _ = self.delete(&oldest.name);
            }
        }

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
    /// reaches `max_tokens`. `top_n` caps how many facts are
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
            let est = crate::session::prompt::count_tokens(&line);
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
///
/// The closing `---` is only recognized when it appears on its own line, so
/// URLs or prose containing `---` inside a value do not prematurely end the
/// frontmatter.
pub fn parse_frontmatter(
    content: &str,
) -> Option<(std::collections::HashMap<String, String>, String)> {
    let trimmed = content.trim();
    let mut lines = trimmed.lines();

    // Opening delimiter must be the first non-empty line.
    let first = lines.by_ref().find(|l| !l.trim().is_empty())?;
    if first.trim() != "---" {
        return None;
    }

    let mut frontmatter_lines: Vec<&str> = Vec::new();
    let mut body_lines: Vec<&str> = Vec::new();
    let mut in_body = false;

    for line in lines {
        if !in_body && line.trim() == "---" {
            in_body = true;
            continue;
        }
        if in_body {
            body_lines.push(line);
        } else {
            frontmatter_lines.push(line);
        }
    }

    if !in_body {
        return None;
    }

    let frontmatter_text = frontmatter_lines.join("\n");
    let body = body_lines.join("\n").trim().to_string();

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
                if let Some((k, v)) = split_key_value(trimmed) {
                    metadata_lines.push(format!("{k}: {v}"));
                }
            } else {
                in_metadata = false;
                // Fall through: this line is a new top-level key
                if let Some((key, value)) = split_key_value(line.trim()) {
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
        if let Some((key, value)) = split_key_value(trimmed) {
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

    Some((map, body))
}

/// Split a simple `key: value` line at the first colon that is not part of
/// a `://` URL scheme separator. This prevents values such as
/// `https://example.com:8080/foo` from being truncated.
fn split_key_value(line: &str) -> Option<(String, String)> {
    for (i, _) in line.match_indices(':') {
        if line[i + 1..].starts_with("//") {
            continue;
        }
        let key = line[..i].trim().to_string();
        let value = line[i + 1..].trim().to_string();
        if key.is_empty() {
            return None;
        }
        return Some((key, value));
    }
    None
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

/// Build the dedup token set for a fact from its description + body.
fn token_set(desc: &str, body: &str) -> std::collections::HashSet<String> {
    tokenize(&format!("{desc} {body}")).into_iter().collect()
}

/// Jaccard similarity over two token sets: |A ∩ B| / |A ∪ B|.
/// Returns 0.0 for two empty sets (no signal to compare).
fn jaccard(a: &std::collections::HashSet<String>, b: &std::collections::HashSet<String>) -> f64 {
    if a.is_empty() && b.is_empty() {
        return 0.0;
    }
    let union = a.union(b).count();
    if union == 0 {
        return 0.0;
    }
    let inter = a.intersection(b).count() as f64;
    inter / union as f64
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
    fn test_max_facts_eviction() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().to_path_buf();
        std::mem::forget(tmp);
        // Disable dedup: this test exercises eviction, not dedup, and the
        // fixtures (single-digit indices dropped by tokenize) are lexically
        // near-identical by design.
        let store = MemoryStore::open(path)
            .unwrap()
            .with_max_facts(3)
            .with_dedup_threshold(1.0);

        for i in 0..5 {
            store
                .upsert(
                    &format!("fact-{i}"),
                    &format!("desc {i}"),
                    "body",
                    "project",
                )
                .unwrap();
        }

        let facts = store.all();
        assert_eq!(
            facts.len(),
            3,
            "should evict down to max_facts=3, got {}",
            facts.len()
        );
        assert_eq!(facts[0].name, "fact-2", "oldest evicted first");
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
        // Disable dedup: this test exercises the token-budget cap among
        // multiple matching facts, not dedup. The shared bodies are
        // near-identical by design.
        let store = temp_store().with_dedup_threshold(1.0);
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

    /// WO 38.9 item 4: all_cached returns None on a cold cache, then
    /// returns the facts on a second call when the directory mtime
    /// hasn't changed.
    #[test]
    fn test_all_cached_cold_then_warm() {
        let store = temp_store();
        store
            .upsert("fact-a", "desc a", "body a", "project")
            .unwrap();

        let mut cache: Option<(std::time::SystemTime, Vec<MemoryFact>)> = None;
        assert!(store.all_cached(&mut cache).is_none(), "cold cache → None");

        let facts = store.all();
        if let Ok(meta) = std::fs::metadata(store.root()) {
            if let Ok(mtime) = meta.modified() {
                cache = Some((mtime, facts.clone()));
            }
        }

        let cached = store.all_cached(&mut cache);
        assert!(cached.is_some(), "warm cache → Some");
        assert_eq!(cached.unwrap().len(), facts.len());
    }

    /// WO 38.9 item 4: select_for_context_from works with a pre-loaded
    /// fact list (same logic as select_for_context but without re-reading
    /// from disk).
    #[test]
    fn test_select_for_context_from_uses_provided_facts() {
        let store = temp_store().with_dedup_threshold(1.0);
        store
            .upsert(
                "anyhow",
                "Use anyhow for errors",
                "We use anyhow for errors",
                "feedback",
            )
            .unwrap();
        store
            .upsert(
                "ratatui",
                "TUI crate",
                "This project uses ratatui",
                "project",
            )
            .unwrap();

        let facts = store.all();
        let selected = store.select_for_context_from(&facts, "anyhow errors", 100, 10);
        assert!(
            !selected.is_empty(),
            "should find anyhow fact, got: {selected:?}"
        );
        assert_eq!(selected[0].name, "anyhow");
    }

    #[test]
    fn test_to_prompt_block_for_facts_subset() {
        // Disable dedup: single-char names + shared "body" are lexically
        // near-identical; this test renders a subset, not dedup.
        let store = temp_store().with_dedup_threshold(1.0);
        store.upsert("a", "desc a", "body", "project").unwrap();
        store.upsert("b", "desc b", "body", "project").unwrap();

        let facts = store.all();
        let block = store.to_prompt_block_for_facts(&facts[..1]);
        assert!(block.contains("a"));
        assert!(!block.contains("b"));
    }

    #[test]
    fn parse_frontmatter_ignores_delimiter_inside_url() {
        // Regression for C27: the old parser used rest.find("---"), so a URL
        // containing three dashes truncated the frontmatter.
        let content = "---\nname: foo\ndescription: https://example.com/a---b\n---\n\nbody";
        let (map, body) = parse_frontmatter(content).unwrap();
        assert_eq!(map.get("name").unwrap(), "foo");
        assert_eq!(map.get("description").unwrap(), "https://example.com/a---b");
        assert_eq!(body, "body");
    }

    #[test]
    fn parse_frontmatter_preserves_url_with_port() {
        // Regression for C27: split_once(':') used to truncate URLs that
        // contain a port number.
        let content = "---\nlink: https://example.com:8080/path\n---\n\nbody";
        let (map, body) = parse_frontmatter(content).unwrap();
        assert_eq!(map.get("link").unwrap(), "https://example.com:8080/path");
        assert_eq!(body, "body");
    }

    #[test]
    fn split_key_value_splits_simple_pair() {
        assert_eq!(
            split_key_value("name: test"),
            Some(("name".to_string(), "test".to_string()))
        );
    }

    #[test]
    fn split_key_value_trims_whitespace() {
        assert_eq!(
            split_key_value("  name  :   test value  "),
            Some(("name".to_string(), "test value".to_string()))
        );
    }

    #[test]
    fn split_key_value_skips_url_scheme_colon() {
        let (k, v) = split_key_value("link: https://example.com:8080/path").unwrap();
        assert_eq!(k, "link");
        assert_eq!(v, "https://example.com:8080/path");
    }

    #[test]
    fn split_key_value_returns_none_for_no_colon() {
        assert!(split_key_value("no colon here").is_none());
    }

    #[test]
    fn split_key_value_returns_none_for_empty_key() {
        assert!(split_key_value(": value").is_none());
        assert!(split_key_value("  : value").is_none());
    }

    #[test]
    fn split_key_value_empty_value_is_kept() {
        assert_eq!(
            split_key_value("key:"),
            Some(("key".to_string(), "".to_string()))
        );
    }

    #[test]
    fn tokenize_lowercases_and_splits_on_non_alphanumeric() {
        let toks = tokenize("Hello, World! Foo-bar");
        assert!(toks.contains(&"hello".to_string()));
        assert!(toks.contains(&"world".to_string()));
        assert!(toks.contains(&"foo".to_string()));
        assert!(toks.contains(&"bar".to_string()));
    }

    #[test]
    fn tokenize_drops_single_char_tokens() {
        let toks = tokenize("a b c xx yy");
        assert!(!toks.contains(&"a".to_string()));
        assert!(!toks.contains(&"b".to_string()));
        assert!(toks.contains(&"xx".to_string()));
        assert!(toks.contains(&"yy".to_string()));
    }

    #[test]
    fn tokenize_drops_stop_words() {
        let toks = tokenize("the and for with this that");
        assert!(
            toks.is_empty(),
            "stop words should be dropped, got {toks:?}"
        );
    }

    #[test]
    fn tokenize_empty_returns_empty() {
        assert!(tokenize("").is_empty());
        assert!(tokenize("!!! ,,, ...").is_empty());
    }

    #[test]
    fn token_set_dedups_repeated_terms() {
        let s = token_set("rust body body", "rust rust more");
        // Repeated terms collapse into one set member.
        assert!(s.contains("rust"));
        assert!(s.contains("body"));
        assert!(s.contains("more"));
    }

    #[test]
    fn jaccard_identical_sets_score_one() {
        let a = token_set("rust toolchain", "cargo workspace");
        let b = token_set("rust toolchain", "cargo workspace");
        assert_eq!(jaccard(&a, &b), 1.0);
    }

    #[test]
    fn jaccard_disjoint_sets_score_zero() {
        let a = token_set("rust", "");
        let b = token_set("python", "");
        assert_eq!(jaccard(&a, &b), 0.0);
    }

    #[test]
    fn jaccard_partial_overlap_between_zero_and_one() {
        let a = token_set("rust cargo", "");
        let b = token_set("rust python", "");
        // intersection {rust}=1, union {rust,cargo,python}=3 → 1/3.
        let score = jaccard(&a, &b);
        assert!(score > 0.0 && score < 1.0, "got {score}");
        assert!((score - 1.0 / 3.0).abs() < 1e-9, "got {score}");
    }

    #[test]
    fn jaccard_empty_empty_is_zero() {
        let empty = std::collections::HashSet::new();
        assert_eq!(jaccard(&empty, &empty), 0.0);
    }

    // WO 28.15 R3: a burst of near-duplicate inserts (same fact reworded)
    // collapses to a single entry; the dedup gate is neither a no-op nor
    // over-aggressive. Distinct names prove the gate (not name-overwrite)
    // is doing the collapsing: the bodies differ only in stop-words
    // ("with"/"using"/"via"), so token-set Jaccard >= 0.85.
    #[test]
    fn dedup_collapses_near_duplicate_rewordings() {
        let store = temp_store();
        let rewordings: &[(&str, &str, &str)] = &[
            (
                "pref-rust-a",
                "kirk prefers rust",
                "kirk prefers rust for systems programming with cargo",
            ),
            (
                "pref-rust-b",
                "kirk prefers rust",
                "kirk prefers rust for systems programming using cargo",
            ),
            (
                "pref-rust-c",
                "kirk prefers rust",
                "kirk prefers rust for systems programming via cargo",
            ),
        ];
        for (name, desc, body) in rewordings {
            store.upsert(name, desc, body, "user").unwrap();
        }
        let facts = store.all();
        assert_eq!(
            facts.len(),
            1,
            "near-duplicate rewordings should collapse to 1, got {}: {:?}",
            facts.len(),
            facts.iter().map(|f| &f.name).collect::<Vec<_>>()
        );
        // The first insert wins; later near-dups are skipped.
        assert_eq!(facts[0].name, "pref-rust-a");
    }

    // WO 28.15 R3: genuinely distinct facts all survive — the threshold is
    // not so aggressive that it merges unrelated memories (data loss).
    #[test]
    fn dedup_keeps_genuinely_distinct_facts() {
        let store = temp_store();
        let distinct = [
            ("rust toolchain", "We use rustup and cargo for builds"),
            ("kubuntu setup", "Kirk runs Kubuntu with KDE plasma"),
            ("api keys", "Keys live in the kf-code secrets file"),
            ("ratatui tui", "Terminal UI is built on ratatui widgets"),
            ("git workflow", "Feature branches merge into dev branch"),
        ];
        for (desc, body) in distinct {
            store.upsert(desc, desc, body, "project").unwrap();
        }
        let facts = store.all();
        assert_eq!(
            facts.len(),
            distinct.len(),
            "distinct facts should all survive ({}), got {}: {:?}",
            distinct.len(),
            facts.len(),
            facts.iter().map(|f| &f.name).collect::<Vec<_>>()
        );
    }

    // WO 28.15: disabling the gate (threshold >= 1.0) restores the old
    // accumulate-everything behaviour.
    #[test]
    fn dedup_disabled_when_threshold_at_or_above_one() {
        let store = temp_store().with_dedup_threshold(1.0);
        store
            .upsert("a", "rust toolchain", "cargo workspace", "user")
            .unwrap();
        store
            .upsert("b", "rust toolchain", "cargo workspace", "user")
            .unwrap();
        assert_eq!(store.all().len(), 2, "disabled dedup should keep both");
    }

    #[test]
    fn compute_idf_empty_corpus_returns_empty() {
        let idf = compute_idf(&[]);
        assert!(idf.is_empty());
    }

    #[test]
    fn compute_idf_singleton_corpus_has_zero_idf_for_sole_term() {
        // n=1, df=1 → idf = ln(1/(1+1)) = ln(0.5) < 0.
        let fact = MemoryFact {
            name: "alpha".into(),
            description: "alpha".into(),
            body: "alpha".into(),
            metadata: Default::default(),
        };
        let idf = compute_idf(&[fact]);
        let val = idf.get("alpha").copied().unwrap_or(0.0);
        assert!(val < 0.0, "ln(0.5) should be negative, got {val}");
    }

    #[test]
    fn compute_idf_counts_each_term_once_per_fact() {
        let fact = MemoryFact {
            name: "alpha alpha".into(),
            description: "alpha".into(),
            body: "alpha".into(),
            metadata: Default::default(),
        };
        let idf = compute_idf(&[fact]);
        // df should be 1 (counted once per fact despite repeats).
        let val = idf.get("alpha").copied().unwrap_or(0.0);
        assert!(
            val < 0.0,
            "repeated term in one doc must not inflate df, got {val}"
        );
    }

    fn make_fact(name: &str, desc: &str, body: &str) -> MemoryFact {
        MemoryFact {
            name: name.into(),
            description: desc.into(),
            body: body.into(),
            metadata: Default::default(),
        }
    }

    #[test]
    fn score_fact_zero_for_unmatched_query() {
        let fact = make_fact("alpha", "desc", "body");
        let idf = compute_idf(&[fact.clone()]);
        let score = score_fact(&fact, &["zzz".to_string()], &idf);
        assert_eq!(score, 0.0);
    }

    #[test]
    fn score_fact_boosts_exact_name_match() {
        let fact = make_fact("alpha", "desc", "body");
        let idf = compute_idf(&[fact.clone()]);
        let exact = score_fact(&fact, &["alpha".to_string()], &idf);
        let partial = score_fact(&fact, &["alph".to_string()], &idf);
        assert!(
            exact > partial,
            "exact name match should score higher than partial, {exact} vs {partial}"
        );
    }

    #[test]
    fn score_fact_boosts_partial_name_match_over_description_only() {
        let fact = make_fact("alpha", "contains alpha", "body");
        let idf = compute_idf(&[fact.clone()]);
        let name_hit = score_fact(&fact, &["alph".to_string()], &idf);
        let desc_only = score_fact(
            &make_fact("other", "alpha here", "body"),
            &["alpha".to_string()],
            &compute_idf(&[make_fact("other", "alpha here", "body")]),
        );
        // name contains + 5.0 (exact? no, contains → 2.0) vs desc contains → 1.0
        assert!(name_hit > 0.0);
        assert!(desc_only > 0.0);
    }

    #[test]
    fn sanitize_slug_delegates_to_slugify() {
        assert_eq!(sanitize_slug("My Setup Guide!"), "my-setup-guide");
        assert_eq!(sanitize_slug("simple"), "simple");
        assert_eq!(sanitize_slug("Rust -- Toolchain"), "rust-toolchain");
    }

    #[test]
    fn slugify_description_collapses_repeated_separators() {
        assert_eq!(slugify_description("a   b   c"), "a-b-c");
        assert_eq!(slugify_description("---leading"), "leading");
        assert_eq!(slugify_description("trailing---"), "trailing");
    }

    #[test]
    fn slugify_description_empty_returns_empty() {
        assert_eq!(slugify_description(""), "");
        assert_eq!(slugify_description("!!!"), "");
    }
}
