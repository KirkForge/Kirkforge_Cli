//! Post-turn fact extraction from user/assistant messages.
//!
// ponytail: heuristic keyword extraction, not LLM; MAX_FACTS_PER_TURN caps noise

use super::{slugify_description, MemoryFact};
use crate::shared::audit::scrub_free_text;
use std::collections::HashMap;

const MIN_FACT_LEN: usize = 20;
const MAX_FACTS_PER_TURN: usize = 3;

const CHAFF: &[&str] = &[
    "ok",
    "okay",
    "thanks",
    "thank you",
    "yes",
    "no",
    "continue",
    "sure",
    "done",
    "got it",
    "great",
    "cool",
    "nice",
    "sounds good",
    "right",
    "exactly",
    "perfect",
    "please continue",
    "go on",
    "proceed",
    "hi",
    "hello",
    "hey",
    "bye",
    "goodbye",
    "morning",
    "afternoon",
    "evening",
    "hmm",
    "um",
    "uh",
    "ah",
    "oh",
    "wow",
];

const USER_PREFS: &[&str] = &[
    "i prefer",
    "always use",
    "never use",
    "please use",
    "i like",
    "i don't like",
    "i hate",
    "i love",
    "make sure to",
    "don't use",
    "avoid using",
    "use this",
    "my preference is",
    "i always",
    "i never",
];

const CORRECTIONS: &[&str] = &[
    "that's wrong",
    "thats wrong",
    "actually",
    "no, the correct",
    "no the correct",
    "that's incorrect",
    "thats incorrect",
    "fix that",
    "that's not right",
    "thats not right",
    "you're wrong",
    "youre wrong",
    "wrong, it should",
    "incorrect, it should",
    "the right way is",
    "the correct way is",
];

fn is_chaff(s: &str) -> bool {
    let trimmed = s.trim().to_lowercase();
    CHAFF.iter().any(|c| trimmed == *c) || trimmed.len() < 4
}

fn sentence_bounds(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let bytes = text.as_bytes();
    for (i, &b) in bytes.iter().enumerate() {
        if b == b'.' || b == b'!' || b == b'?' {
            let end = i + 1;
            if end > start + 2 {
                out.push(&text[start..end]);
            }
            start = end;
        }
    }
    if start < bytes.len() && bytes.len() - start > 2 {
        let remaining = text[start..].trim_end();
        if remaining.len() > 2 {
            out.push(remaining);
        }
    }
    out
}

// Drop URL-, path-, and KEY=VALUE-shaped tokens so they never become
// filename material (WO 47.27: 'I prefer ANTHROPIC_API_KEY=sk-abc123'
// used to leak the key into a memory/ filename).
fn strip_slug_hazards(text: &str) -> String {
    text.split_whitespace()
        .filter(|tok| {
            let bare = tok.trim_matches(|c: char| !c.is_alphanumeric());
            !(bare.starts_with("http://")
                || bare.starts_with("https://")
                || bare.starts_with("www.")
                || bare.contains('/')
                || bare.contains('='))
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn make_slug(prefix: &str, text: &str) -> String {
    let safe = strip_slug_hazards(text);
    let slug_part = slugify_description(&safe[..safe.len().min(120)]);
    let hash = fnv1a_16(text);
    format!("{prefix}{slug_part}-{hash:04x}")
}

// Build a fact with secret-looking spans replaced by [REDACTED] in
// body/description. Same shapes as the audit-log scrubber so "what a
// secret looks like" stays single-sourced; the slug additionally strips
// URLs/paths (make_slug).
fn new_fact(prefix: &str, fact_text: &str, kind: &str) -> MemoryFact {
    let clean = scrub_free_text(fact_text);
    MemoryFact {
        name: make_slug(prefix, fact_text),
        description: clean[..clean.len().min(80)].to_string(),
        body: clean,
        metadata: HashMap::from([("type".into(), kind.into())]),
    }
}

fn fnv1a_16(data: &str) -> u16 {
    let mut hash: u32 = 0x811c9dc5;
    for byte in data.bytes() {
        hash ^= byte as u32;
        hash = hash.wrapping_mul(0x01000193);
    }
    (hash & 0xffff) as u16
}

fn extract_user_preferences(user_msg: &str) -> Vec<MemoryFact> {
    let lower = user_msg.to_lowercase();
    let mut facts = Vec::new();

    for pat in USER_PREFS {
        if let Some(idx) = lower.find(pat) {
            let fact_text = user_msg[idx..].trim();
            if fact_text.len() < MIN_FACT_LEN || is_chaff(fact_text) {
                continue;
            }
            facts.push(new_fact("user-pref-", fact_text, "user"));
        }
    }

    facts
}

fn extract_corrections(user_msg: &str) -> Vec<MemoryFact> {
    let lower = user_msg.to_lowercase();
    let mut facts = Vec::new();

    for pat in CORRECTIONS {
        if let Some(idx) = lower.find(pat) {
            let fact_text = user_msg[idx..].trim();
            if fact_text.len() < MIN_FACT_LEN || is_chaff(fact_text) {
                continue;
            }
            facts.push(new_fact("feedback-", fact_text, "feedback"));
        }
    }

    facts
}

fn extract_project_facts(assistant_msg: &str) -> Vec<MemoryFact> {
    let mut facts = Vec::new();
    let sentences = sentence_bounds(assistant_msg);

    let project_signals: &[&str] = &[
        "the project uses",
        "this repo uses",
        "this project",
        "the codebase",
        "the config file is",
        "the main entry",
        "the binary is",
        "the crate is",
        "located at",
        "defined in",
        "implemented in",
        "the module",
        "the struct",
        "the function",
        "the test suite",
        "the build system",
    ];

    for sent in &sentences {
        if sent.len() < MIN_FACT_LEN {
            continue;
        }
        let lower = sent.to_lowercase();
        if project_signals.iter().any(|sig| lower.contains(sig)) {
            facts.push(new_fact("project-", sent, "project"));
        }
    }

    facts
}

pub fn extract_facts(user_msg: &str, assistant_msg: &str) -> Vec<MemoryFact> {
    if is_chaff(user_msg) && is_chaff(assistant_msg) {
        return Vec::new();
    }

    let mut facts = Vec::new();
    facts.extend(extract_user_preferences(user_msg));
    facts.extend(extract_corrections(user_msg));
    facts.extend(extract_project_facts(assistant_msg));
    facts.truncate(MAX_FACTS_PER_TURN);
    facts
}

/// True if the user message contains preference or correction keywords,
/// meaning extraction should run regardless of turn-count rate limiting.
pub fn is_preference_like(msg: &str) -> bool {
    let lower = msg.to_lowercase();
    USER_PREFS.iter().any(|p| lower.contains(p)) || CORRECTIONS.iter().any(|p| lower.contains(p))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_user_preference_i_prefer() {
        let facts = extract_facts("I prefer tabs over spaces", "");
        assert!(!facts.is_empty());
        let f = &facts[0];
        assert_eq!(f.metadata.get("type").unwrap(), "user");
        assert!(f.body.contains("I prefer tabs"));
    }

    #[test]
    fn extracts_always_use() {
        let facts = extract_facts("always use anyhow for errors", "");
        assert!(!facts.is_empty());
        assert_eq!(facts[0].metadata.get("type").unwrap(), "user");
    }

    #[test]
    fn extracts_never_use() {
        let facts = extract_facts("never use unwrap in production code", "");
        assert!(!facts.is_empty());
        assert_eq!(facts[0].metadata.get("type").unwrap(), "user");
    }

    #[test]
    fn extracts_correction() {
        let facts = extract_facts("that's wrong, the correct approach is to use Result", "");
        assert!(!facts.is_empty());
        assert_eq!(facts[0].metadata.get("type").unwrap(), "feedback");
    }

    #[test]
    fn extracts_actually_correction() {
        let facts = extract_facts("actually, we should use tokio::spawn here", "");
        assert!(!facts.is_empty());
        assert_eq!(facts[0].metadata.get("type").unwrap(), "feedback");
    }

    #[test]
    fn extracts_project_fact_from_assistant() {
        let facts = extract_facts(
            "",
            "The project uses Rust with tokio for async runtime. The main entry point is src/main.rs.",
        );
        assert!(!facts.is_empty());
        assert_eq!(facts[0].metadata.get("type").unwrap(), "project");
    }

    #[test]
    fn skips_chaff() {
        let facts = extract_facts("ok", "thanks");
        assert!(facts.is_empty());
    }

    #[test]
    fn skips_short_facts() {
        let facts = extract_facts("I prefer X", "");
        assert!(
            facts.is_empty(),
            "facts < 20 chars should be skipped: {facts:?}"
        );
    }

    #[test]
    fn skips_greetings() {
        let facts = extract_facts("hello there", "hi");
        assert!(facts.is_empty());
    }

    #[test]
    fn no_facts_from_empty() {
        let facts = extract_facts("", "");
        assert!(facts.is_empty());
    }

    #[test]
    fn mixed_extraction() {
        let facts = extract_facts(
            "I prefer clippy strict mode. Also, the build system uses cargo.",
            "The codebase is structured as a workspace with multiple crates.",
        );
        assert!(facts
            .iter()
            .any(|f| f.metadata.get("type").unwrap() == "user"));
    }

    #[test]
    fn sentence_bounds_splits_on_period() {
        let sents = sentence_bounds("First sentence. Second sentence. Third.");
        assert_eq!(sents.len(), 3);
    }

    #[test]
    fn sentence_bounds_single_no_period() {
        let sents = sentence_bounds("Just one sentence here");
        assert_eq!(sents.len(), 1);
        assert_eq!(sents[0], "Just one sentence here");
    }

    #[test]
    fn slug_includes_hash_suffix() {
        let facts = extract_facts("I prefer tabs over spaces for indentation", "");
        assert!(!facts.is_empty());
        let name = &facts[0].name;
        assert!(
            name.contains('-'),
            "slug should contain hash suffix: {name}"
        );
        let parts: Vec<&str> = name.split('-').collect();
        let last = parts.last().unwrap();
        assert!(
            u16::from_str_radix(last, 16).is_ok(),
            "last segment should be hex hash: {name}"
        );
    }

    #[test]
    fn different_facts_different_slugs() {
        let f1 = extract_facts("I prefer tabs over spaces for indentation", "");
        let f2 = extract_facts("I prefer vim over emacs for editing code", "");
        assert!(!f1.is_empty());
        assert!(!f2.is_empty());
        assert_ne!(
            f1[0].name, f2[0].name,
            "different facts must not collide on slug"
        );
    }

    #[test]
    fn max_facts_per_turn_caps_output() {
        let mut big_assistant = String::new();
        for i in 0..10 {
            big_assistant.push_str(&format!("The project uses framework{i} for thing{i}. "));
        }
        let facts = extract_facts("I prefer rust over go for systems", &big_assistant);
        assert!(
            facts.len() <= MAX_FACTS_PER_TURN,
            "should cap at MAX_FACTS_PER_TURN={}, got {}: {:?}",
            MAX_FACTS_PER_TURN,
            facts.len(),
            facts
        );
    }

    #[test]
    fn is_preference_like_detects_pref() {
        assert!(is_preference_like("I prefer tabs over spaces"));
        assert!(!is_preference_like("hello there"));
    }

    #[test]
    fn is_preference_like_detects_correction() {
        assert!(is_preference_like("actually, we should use tokio"));
        assert!(is_preference_like("that's wrong, the fix is X"));
    }

    // ── WO 47.27: secrets must not reach filenames or fact bodies ──────

    #[test]
    fn slug_strips_key_value_url_and_path_spans() {
        let slug = make_slug(
            "user-pref-",
            "I prefer ANTHROPIC_API_KEY=sk-abc123 when coding",
        );
        assert!(
            !slug.contains("anthropic"),
            "KEY=VALUE leaked into slug: {slug}"
        );
        assert!(
            !slug.contains("abc123"),
            "secret value leaked into slug: {slug}"
        );

        let slug = make_slug(
            "user-pref-",
            "always use https://internal.example.com/v2 for the builds",
        );
        assert!(
            !slug.contains("internal"),
            "URL host leaked into slug: {slug}"
        );
        assert!(!slug.contains("example"), "URL leaked into slug: {slug}");

        let slug = make_slug("project-", "the config file is src/main.rs okay");
        assert!(!slug.contains("main"), "path leaked into slug: {slug}");
        assert!(!slug.contains("src"), "path leaked into slug: {slug}");
    }

    #[test]
    fn secret_redacted_from_fact_body_and_description() {
        let facts = extract_facts("I prefer ANTHROPIC_API_KEY=sk-abc123def for the client", "");
        assert!(!facts.is_empty());
        for f in &facts {
            assert!(
                !f.body.contains("sk-abc123def"),
                "body carries literal secret: {}",
                f.body
            );
            assert!(
                !f.description.contains("sk-abc123def"),
                "description carries literal secret: {}",
                f.description
            );
            assert!(
                f.body.contains("[REDACTED]"),
                "body not scrubbed: {}",
                f.body
            );
        }
        // The name is a FILENAME under memory/ — it must not carry the key.
        assert!(
            !facts[0].name.contains("anthropic"),
            "secret name reached filename slug: {}",
            facts[0].name
        );
    }

    #[test]
    fn non_secret_key_value_still_redacted_consistently() {
        // Non-credential env vars survive the scrubber untouched in the
        // body (only slug input drops ALL KEY=VALUE shapes).
        let facts = extract_facts("I prefer PATH=/usr/local/bin on this machine", "");
        assert!(!facts.is_empty());
        assert!(
            facts[0].body.contains("PATH=/usr/local/bin"),
            "non-secret env var should survive in body: {}",
            facts[0].body
        );
        assert!(
            !facts[0].name.contains("usr"),
            "path-shaped token should be stripped from slug: {}",
            facts[0].name
        );
    }

    // ── WO 47.27 mm-H21: nested pattern matches dedup to one insert ────
    // (added with the earliest-match dedup change)
}
