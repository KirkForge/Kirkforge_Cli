//! Shared cached context stem + prompt-cache enforcement.
//!
//! vix maximises Anthropic prompt-cache reuse by (a) appending the
//! thread's shared `contextSystemBlocks()` (CLAUDE.md/AGENTS.md +
//! skills metadata) to *every* workflow step's sub-agent system prompt
//! so the stem is byte-identical across phases, and (b) injecting the
//! top-N frequently-accessed file bodies into the cached stem from
//! SQLite access stats so the model stops re-reading the same files.
//!
//! This module flips the `CacheStemTracker` from metric-only to
//! *enforcement*: `shared_context_stem(state)` produces a
//! `Vec<Message>` (project instructions + tool catalog + top-N files)
//! that every sub-agent / workflow step system prompt prepends. The
//! tracker's existing hash check becomes an assertion that the stem
//! hash is constant across phases; a drift in dev is a loud failure.

use crate::shared::Message;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::PathBuf;

/// Default number of top files to inject into the stem.
pub const DEFAULT_TOP_N_FILES: usize = 10;

/// Build the shared context stem that every sub-agent / workflow step
/// system prompt must prepend.
///
/// The stem is:
/// 1. Project instructions (the system prompt text) — byte-identical
///    across all phases so Anthropic's prompt cache hits on every turn.
/// 2. Tool catalog (tool names) — also stable across the session.
/// 3. Top-N frequently-accessed file bodies (minified) — hot files
///    that the model would otherwise re-read every turn.
///
/// The stem must be byte-identical across phases. Any per-phase
/// variation (e.g. a phase index in the prompt) breaks the cache.
/// Put phase-specific content *after* the shared stem.
pub fn shared_context_stem(
    system_text: &str,
    tool_names: &[&str],
    top_files: &[(PathBuf, String)],
) -> Vec<Message> {
    let mut content = system_text.to_string();

    // Append tool catalog as a stable block. This is part of the stem
    // because the tool set doesn't change across phases in a session.
    if !tool_names.is_empty() {
        content.push_str("\n\nAvailable tools: ");
        content.push_str(&tool_names.join(", "));
        content.push('.');
    }

    // Append top-N file bodies. These are minified file contents from
    // the access tracker — the hottest files the model re-reads every
    // turn. Injecting them into the stem means Anthropic caches them
    // as part of the stable prefix, eliminating redundant re-reads.
    if !top_files.is_empty() {
        content.push_str("\n\nFrequently accessed files:\n");
        for (path, body) in top_files {
            content.push_str(&format!("--- {} ---\n{}\n", path.display(), body));
        }
    }

    vec![Message {
        role: crate::shared::Role::System,
        content,
        content_parts: None,
        thinking: None,
        tool_calls: None,
        tool_call_id: None,
        tool_name: None,
        token_count: None,
    }]
}

/// Records the hash of the prefix messages sent in the prior turn and
/// reports whether the current turn's prefix matches.
///
/// The tracker holds a single `Option<u64>`: the last recorded prefix
/// hash. `is_stable` returns `true` when the current prefix hashes to
/// the same value as the one recorded by the previous
/// `record_prefix_hash` call. The first call always reports `false`
/// (there is no prior turn to compare against).
///
/// Thread-safety: the tracker is `Send + Sync` but **not** lock-free.
/// Callers that share one tracker across turns (the executor) must
/// hold it behind a `&mut` or a `Mutex`; the executor's
/// `prompt_builder` is already behind `&mut self`, so this matches the
/// existing pattern.
#[derive(Debug, Default, Clone)]
pub struct CacheStemTracker {
    last_hash: Option<u64>,
}

impl CacheStemTracker {
    pub fn new() -> Self {
        Self { last_hash: None }
    }

    /// Hash the prefix messages and record the hash for the next
    /// `is_stable` comparison. Returns the hash that was recorded.
    ///
    /// The "prefix" is the stable stem of the conversation: the
    /// system message, the tool definitions (represented implicitly by
    /// the system message's tool list), and the first `prefix_len`
    /// non-system messages. The trailing user turn is excluded by
    /// convention — the caller passes `prefix_len = messages.len() - 1`
    /// (or similar) to leave the volatile tail out of the hash.
    pub fn record_prefix_hash(&mut self, messages: &[Message], prefix_len: usize) -> u64 {
        let hash = hash_prefix(messages, prefix_len);
        self.last_hash = Some(hash);
        hash
    }

    /// Hash the prefix messages and return `true` if the hash matches
    /// the one recorded by the previous `record_prefix_hash` call.
    ///
    /// Does **not** mutate the tracker — call `record_prefix_hash`
    /// afterwards to advance the recorded hash for the next turn.
    pub fn is_stable(&self, messages: &[Message], prefix_len: usize) -> bool {
        match self.last_hash {
            Some(prev) => hash_prefix(messages, prefix_len) == prev,
            None => false,
        }
    }

    /// The hash recorded by the most recent `record_prefix_hash` call,
    /// or `None` if `record_prefix_hash` has not been called yet.
    pub fn last_hash(&self) -> Option<u64> {
        self.last_hash
    }

    /// Assert that the stem (first N messages) is stable across two
    /// sub-agent spawns. Used in dev builds to verify that the shared
    /// context stem is byte-identical across workflow phases. In
    /// release builds this is a no-op; in debug builds it panics on
    /// drift.
    pub fn assert_stem_stable(&self, messages: &[Message], prefix_len: usize, context: &str) {
        if cfg!(debug_assertions) {
            if let Some(prev) = self.last_hash {
                let current = hash_prefix(messages, prefix_len);
                assert_eq!(
                    current, prev,
                    "Cache stem drift detected ({context}): the shared context stem must be \
                     byte-identical across sub-agent phases. Phase-specific content must go \
                     *after* the shared stem."
                );
            }
        }
    }
}

/// Hash the first `prefix_len` messages of `messages`. Each message is
/// serialised to its canonical JSON form (the same form the on-disk
/// NDJSON log uses) and the resulting bytes are fed to the hasher. This
/// means any semantic change to a message's role, content, thinking,
/// tool calls, tool name, or tool call id invalidates the stem, while
/// fields that do not affect the provider's cache key (e.g.
/// `token_count`, which is a local estimate) are excluded by
/// `skip_serializing_if`.
fn hash_prefix(messages: &[Message], prefix_len: usize) -> u64 {
    let mut hasher = DefaultHasher::new();
    let end = messages.len().min(prefix_len);
    for m in messages.iter().take(end) {
        // Serialise the message and hash the bytes. Serialisation
        // failures are treated as a constant empty string so a
        // non-serialisable message never panics the hasher; this
        // would only happen for a `Message` that violates its own
        // `Serialize` impl, which is a bug elsewhere.
        let bytes = serde_json::to_vec(m).unwrap_or_default();
        bytes.hash(&mut hasher);
    }
    hasher.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shared::Role;

    fn user(content: &str) -> Message {
        Message {
            role: Role::User,
            content: content.into(),
            ..Default::default()
        }
    }

    fn assistant(content: &str) -> Message {
        Message {
            role: Role::Assistant,
            content: content.into(),
            ..Default::default()
        }
    }

    fn system(content: &str) -> Message {
        Message {
            role: Role::System,
            content: content.into(),
            ..Default::default()
        }
    }

    /// Build a 5-turn conversation one turn at a time and confirm the
    /// prefix hash is stable across turns 2-5 (the tracker reports
    /// `is_stable=true` for turns 2-5) when the prefix is the stable
    /// system-only stem. Turn 1 has no prior hash, so `is_stable` is
    /// `false`. A change to the system message breaks stability.
    #[test]
    fn test_prefix_hash_stable_across_five_turns() {
        let mut tracker = CacheStemTracker::new();

        // System + tools stem is stable across all turns.
        let sys = system("You are a coding agent. Tools: read_file, bash.");

        // Realistic stable-stem scenario: the system message never
        // changes, so once we record its hash, every subsequent turn
        // is stable. The prefix_len = 1 means "just the system
        // message" — the trailing user/assistant turns are the
        // volatile tail.
        let fixed_prefix = 1;

        // Turn 1: [sys, user1]. No prior hash → is_stable=false.
        let turn1 = vec![sys.clone(), user("hello")];
        assert!(
            !tracker.is_stable(&turn1, fixed_prefix),
            "turn 1 should not be stable (no prior hash)"
        );
        tracker.record_prefix_hash(&turn1, fixed_prefix);

        // Turn 2: prefix [sys] is identical to turn 1's prefix.
        let turn2 = vec![
            sys.clone(),
            user("hello"),
            assistant("hi there"),
            user("what is 2+2?"),
        ];
        assert!(
            tracker.is_stable(&turn2, fixed_prefix),
            "turn 2 with fixed system-only prefix should be stable"
        );
        tracker.record_prefix_hash(&turn2, fixed_prefix);

        // Turn 3
        let turn3 = vec![
            sys.clone(),
            user("hello"),
            assistant("hi there"),
            user("what is 2+2?"),
            assistant("4"),
            user("thanks!"),
        ];
        assert!(
            tracker.is_stable(&turn3, fixed_prefix),
            "turn 3 with fixed system-only prefix should be stable"
        );
        tracker.record_prefix_hash(&turn3, fixed_prefix);

        // Turn 4
        let turn4 = vec![
            sys.clone(),
            user("hello"),
            assistant("hi there"),
            user("what is 2+2?"),
            assistant("4"),
            user("thanks!"),
            assistant("you're welcome"),
            user("now what?"),
        ];
        assert!(
            tracker.is_stable(&turn4, fixed_prefix),
            "turn 4 with fixed system-only prefix should be stable"
        );
        tracker.record_prefix_hash(&turn4, fixed_prefix);

        // Turn 5
        let turn5 = vec![
            sys.clone(),
            user("hello"),
            assistant("hi there"),
            user("what is 2+2?"),
            assistant("4"),
            user("thanks!"),
            assistant("you're welcome"),
            user("now what?"),
            assistant("let's see"),
            user("ok"),
        ];
        assert!(
            tracker.is_stable(&turn5, fixed_prefix),
            "turn 5 with fixed system-only prefix should be stable"
        );

        // Sanity: if the system message changes, stability breaks.
        let mut turn5_broken = turn5.clone();
        turn5_broken[0] = system("DIFFERENT system prompt");
        assert!(
            !tracker.is_stable(&turn5_broken, fixed_prefix),
            "changing the system message must break stability"
        );
    }

    /// `is_stable` on a fresh tracker (no `record_prefix_hash` call)
    /// returns `false` for any input.
    #[test]
    fn test_fresh_tracker_is_not_stable() {
        let tracker = CacheStemTracker::new();
        let msgs = vec![system("s"), user("u")];
        assert!(!tracker.is_stable(&msgs, msgs.len()));
        assert!(tracker.last_hash().is_none());
    }

    /// `record_prefix_hash` with `prefix_len = 0` hashes nothing and is
    /// stable across any message list (the empty prefix is trivially
    /// constant). This is the degenerate case but it should not panic.
    #[test]
    fn test_empty_prefix_is_trivially_stable() {
        let mut tracker = CacheStemTracker::new();
        let msgs = vec![system("s"), user("u")];
        let h1 = tracker.record_prefix_hash(&msgs, 0);
        let h2 = tracker.record_prefix_hash(&[system("other")], 0);
        assert_eq!(h1, h2, "empty prefix hashes must be equal");
        assert!(tracker.is_stable(&msgs, 0));
    }

    /// `prefix_len` larger than the message list is clamped to the
    /// list length (no panic).
    #[test]
    fn test_prefix_len_clamped_to_message_count() {
        let mut tracker = CacheStemTracker::new();
        let msgs = vec![system("s"), user("u")];
        let h = tracker.record_prefix_hash(&msgs, 100);
        assert_eq!(h, hash_prefix(&msgs, 100));
        assert!(tracker.is_stable(&msgs, 100));
    }

    /// The hash is deterministic: the same prefix produces the same
    /// hash across separate tracker instances.
    #[test]
    fn test_hash_is_deterministic_across_instances() {
        let msgs = vec![system("s"), user("u"), assistant("a")];
        let h1 = {
            let mut t = CacheStemTracker::new();
            t.record_prefix_hash(&msgs, msgs.len())
        };
        let h2 = {
            let mut t = CacheStemTracker::new();
            t.record_prefix_hash(&msgs, msgs.len())
        };
        assert_eq!(h1, h2);
    }

    /// A change in any contributing field (role, content, thinking,
    /// tool_calls, tool_name, tool_call_id) invalidates the hash.
    #[test]
    fn test_semantic_change_invalidates_hash() {
        let base = Message {
            role: Role::Assistant,
            content: "c".into(),
            thinking: Some("t".into()),
            tool_calls: Some(vec![crate::shared::ToolInvocation {
                id: "id1".into(),
                name: "bash".into(),
                arguments: serde_json::json!({"x": 1}),
            }]),
            tool_name: Some("bash".into()),
            tool_call_id: Some("cid".into()),
            ..Default::default()
        };
        let msgs = vec![base.clone()];
        let mut t = CacheStemTracker::new();
        t.record_prefix_hash(&msgs, msgs.len());

        // role change
        let mut m = base.clone();
        m.role = Role::User;
        assert!(!t.is_stable(&[m], 1), "role change must invalidate");

        // content change
        let mut m = base.clone();
        m.content = "other".into();
        assert!(!t.is_stable(&[m], 1), "content change must invalidate");

        // thinking change
        let mut m = base.clone();
        m.thinking = Some("other".into());
        assert!(!t.is_stable(&[m], 1), "thinking change must invalidate");

        // tool_calls change
        let mut m = base.clone();
        m.tool_calls = None;
        assert!(!t.is_stable(&[m], 1), "tool_calls change must invalidate");

        // tool_name change
        let mut m = base.clone();
        m.tool_name = Some("read_file".into());
        assert!(!t.is_stable(&[m], 1), "tool_name change must invalidate");

        // tool_call_id change
        let mut m = base.clone();
        m.tool_call_id = Some("other".into());
        assert!(!t.is_stable(&[m], 1), "tool_call_id change must invalidate");
    }

    // ── shared_context_stem tests ────────────────────────────────────

    /// shared_context_stem produces a single system message with the
    /// project instructions, tool catalog, and top-N files.
    #[test]
    fn test_shared_context_stem_includes_system_text() {
        let stem = shared_context_stem("You are a coding agent.", &[], &[]);
        assert_eq!(stem.len(), 1);
        assert_eq!(stem[0].role, Role::System);
        assert!(stem[0].content.contains("You are a coding agent."));
    }

    #[test]
    fn test_shared_context_stem_appends_tool_catalog() {
        let stem = shared_context_stem("sys", &["read_file", "bash"], &[]);
        assert!(stem[0]
            .content
            .contains("Available tools: read_file, bash."));
    }

    #[test]
    fn test_shared_context_stem_appends_top_files() {
        let top_files = vec![
            (PathBuf::from("src/main.rs"), "fn main() {}".to_string()),
            (PathBuf::from("src/lib.rs"), "pub fn lib() {}".to_string()),
        ];
        let stem = shared_context_stem("sys", &[], &top_files);
        assert!(stem[0].content.contains("src/main.rs"));
        assert!(stem[0].content.contains("fn main()"));
        assert!(stem[0].content.contains("src/lib.rs"));
        assert!(stem[0].content.contains("pub fn lib()"));
    }

    /// The stem is byte-identical when built with the same inputs —
    /// the core invariant for Anthropic prompt-cache reuse.
    #[test]
    fn test_shared_context_stem_is_byte_identical_across_calls() {
        let top_files = vec![(PathBuf::from("src/main.rs"), "fn main() {}".to_string())];
        let stem1 = shared_context_stem("sys", &["bash"], &top_files);
        let stem2 = shared_context_stem("sys", &["bash"], &top_files);
        assert_eq!(stem1[0].content, stem2[0].content);
    }

    /// Different top-N file lists produce different stems.
    #[test]
    fn test_shared_context_stem_differs_with_different_files() {
        let stem1 = shared_context_stem(
            "sys",
            &["bash"],
            &[(PathBuf::from("a.rs"), "content a".to_string())],
        );
        let stem2 = shared_context_stem(
            "sys",
            &["bash"],
            &[(PathBuf::from("b.rs"), "content b".to_string())],
        );
        assert_ne!(stem1[0].content, stem2[0].content);
    }

    /// assert_stem_stable does not panic when the stem is stable.
    #[test]
    fn test_assert_stem_stable_no_panic_on_stable() {
        let mut tracker = CacheStemTracker::new();
        let msgs = vec![system("sys"), user("u")];
        tracker.record_prefix_hash(&msgs, 1);
        // Same prefix — should not panic in debug builds.
        tracker.assert_stem_stable(&msgs, 1, "test stable");
    }

    /// Two sub-agent spawns with the same shared stem produce
    /// identical stem hashes — verified by the tracker.
    #[test]
    fn test_stem_hash_identical_across_two_spawns() {
        let mut tracker = CacheStemTracker::new();

        // Phase 1: sub-agent spawn with shared stem
        let stem1 = shared_context_stem(
            "You are a coding agent.",
            &["read_file", "bash", "edit_file"],
            &[(PathBuf::from("src/lib.rs"), "pub fn lib() {}".to_string())],
        );
        let mut messages1 = stem1.clone();
        messages1.push(Message {
            role: Role::User,
            content: "Phase 1 task".into(),
            ..Default::default()
        });
        let hash1 = tracker.record_prefix_hash(&messages1, stem1.len());

        // Phase 2: sub-agent spawn with same shared stem (different task)
        let stem2 = shared_context_stem(
            "You are a coding agent.",
            &["read_file", "bash", "edit_file"],
            &[(PathBuf::from("src/lib.rs"), "pub fn lib() {}".to_string())],
        );
        let mut messages2 = stem2.clone();
        messages2.push(Message {
            role: Role::User,
            content: "Phase 2 task".into(),
            ..Default::default()
        });
        let hash2 = tracker.record_prefix_hash(&messages2, stem2.len());

        // The stem (prefix) hashes must be identical — this is the
        // core Anthropic prompt-cache invariant.
        assert_eq!(
            hash1, hash2,
            "stem hash must be identical across phases for cache reuse"
        );
    }

    /// Top-N files appear in the stem and are stable across turns.
    #[test]
    fn test_top_n_files_appear_in_stem_and_are_stable_across_turns() {
        let top_files = vec![
            (PathBuf::from("src/main.rs"), "fn main() {}".to_string()),
            (PathBuf::from("src/lib.rs"), "pub fn lib() {}".to_string()),
        ];
        let stem1 = shared_context_stem("sys", &["bash"], &top_files);
        let stem2 = shared_context_stem("sys", &["bash"], &top_files);
        // Same files → same stem content.
        assert_eq!(stem1[0].content, stem2[0].content);
        // Files are present.
        assert!(stem1[0].content.contains("src/main.rs"));
        assert!(stem1[0].content.contains("src/lib.rs"));
    }
}
