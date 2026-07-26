//! Client-side prompt-cache stem-reuse tracker.
//!
//! The Anthropic adapter marks the last two prefix messages and the
//! last system block with `cache_control: {type: "ephemeral"}` so the
//! provider can hit its KV-cache for the stable prefix (ADR-0027).
//! That is a *server-side* optimisation: the client still sends the
//! full content every turn (the API needs the bytes to compute the
//! cache key), so there is nothing to short-circuit on the wire.
//!
//! What the client *can* do is detect that the prefix is byte-for-byte
//! stable across turns and emit a `PlanReason::CacheStemReuse` metric
//! event so the operator can see stem reuse is happening. That is what
//! this module implements.
//!
//! The tracker hashes the prefix messages (system + tools + first N
//! turns) with `std::collections::hash_map::DefaultHasher` — no new
//! dependency. The hash is cheap (one pass over the message bytes) and
//! the comparison is a single `u64` eq.

use crate::shared::Message;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

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
}
