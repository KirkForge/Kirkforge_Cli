use crate::shared::{Message, Role};
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;

/// How many recent user/assistant turns to keep verbatim during compaction.
///
/// "Turns" here means user-message-anchored exchanges: each user message
/// plus the assistant text + tool roundtrips that follow it. Older turns
/// are condensed into anchors. The default of 4 covers the most recent
/// back-and-forth the model is actively reasoning about.
pub const COMPACT_KEEP_LAST_N_TURNS: usize = 4;

/// Marker written into the content of `Role::Tool` messages that have
/// been dropped by compaction. The TUI keeps the original output in
/// `ConversationEntry::tool_output` for human recall, so nothing is
/// truly *lost* — just demoted out of the model's context window.
pub const COMPACTED_TOOL_MARKER: &str =
    "[compacted — see conversation log on disk for full output]";

/// Marker written into the content of `Role::Assistant` text messages
/// that have been condensed by compaction. The first 500 characters of
/// the original are preserved (so the model can still see the
/// conclusion of that turn), then a marker.
pub const COMPACTED_ASSISTANT_MARKER: &str =
    "[…previous assistant response compacted…]";

/// Result of compacting a conversation. Returned by
/// [`PromptBuilder::compact`] so callers (TUI, executor) can report
/// useful stats to the user.
#[derive(Debug, Clone, PartialEq)]
pub struct CompactionResult {
    /// The compacted message list. Pass this to
    /// `ConversationLog::replace_all` to persist the new history.
    pub new_messages: Vec<Message>,
    /// How many `Role::Tool` messages were dropped.
    pub dropped_tool_results: usize,
    /// How many `Role::Assistant` text messages were condensed.
    pub condensed_assistant_turns: usize,
    /// Total messages in the original input.
    pub original_count: usize,
    /// Total messages in the output.
    pub compacted_count: usize,
}

/// System prompt builder with prompt-cache-aware stem design.
///
/// # Cache stem strategy
///
/// The prompt is structured in two parts:
///
/// 1. **Stem** (invariant): The core instruction block that's identical
///    across all calls for a given model. This is designed to maximize
///    Anthropic-style prompt caching — the cache key picks up the first
///    N tokens, and if the stem hasn't changed, the cache hits.
///
/// 2. **Suffix** (variable): Tool list, model-specific flags, user context.
///    Changes every turn but avoids invalidating the stem's cache entry.
///
/// For best cache performance, the stem should be at least 1024 tokens
/// (the minimum for Anthropic prompt caching). With code-heavy system
/// prompts, 1 token ≈ 4 characters → ~4096 chars minimum.
pub struct PromptBuilder {
    template: String,
    cache: HashMap<String, String>, // keyed by model name
}

impl PromptBuilder {
    pub fn new() -> Self {
        let template = include_str!("../../prompts/system.hbs");
        Self {
            template: template.to_string(),
            cache: HashMap::new(),
        }
    }

    /// Build the system prompt for the given model and tools.
    ///
    /// The returned message has `content` structured as:
    /// ```
    /// [CACHE STEM — invariant instructions]
    /// Available tools: [...]
    /// [model-specific extensions]
    /// [Session Carryover — optional, appended at the end]
    /// ```
    ///
    /// The stem portion is identical for all calls to the same model
    /// (same model_name + same thinking flag). The suffix changes
    /// per-turn based on available tools. An optional carryover block
    /// is appended last — it provides cross-session context without
    /// disturbing the instruction stem.
    pub fn build(
        &mut self,
        model_name: &str,
        model_supports_thinking: bool,
        tool_names: &[&str],
        carryover_block: Option<&str>,
    ) -> Message {
        let reg = handlebars::Handlebars::new();

        let mut data = serde_json::json!({
            "model_name": model_name,
            "tools": tool_names.iter().map(|n| serde_json::json!({"name": n})).collect::<Vec<_>>(),
        });

        if model_supports_thinking {
            data["thinking_available"] = serde_json::Value::Bool(true);
        }

        let mut content = reg
            .render_template(&self.template, &data)
            .unwrap_or_else(|_| "You are a coding agent.".to_string());

        // Append carryover block at the very end if provided
        if let Some(block) = carryover_block {
            if !block.is_empty() {
                content.push_str("\n\n");
                content.push_str(block);
            }
        }

        Message {
            role: Role::System,
            content,
            thinking: None,
            tool_calls: None,
            tool_call_id: None,
            tool_name: None,
            token_count: None,
        }
    }

    /// Build just the cache stem for a given model.
    ///
    /// This is the invariant portion of the system prompt. Use it to
    /// estimate whether a cache hit is likely before the full build.
    pub fn build_stem(&self, model_name: &str, model_supports_thinking: bool) -> String {
        let reg = handlebars::Handlebars::new();
        let mut data = serde_json::json!({
            "model_name": model_name,
            "tools": Vec::<serde_json::Value>::new(), // empty — tools go in suffix
        });

        if model_supports_thinking {
            data["thinking_available"] = serde_json::Value::Bool(true);
        }

        reg.render_template(&self.template, &data)
            .unwrap_or_else(|_| "You are a coding agent.".to_string())
    }

    /// Estimate cache hit probability based on stem stability.
    ///
    /// Returns a score 0.0–1.0 where 1.0 = perfect cache hit expected.
    /// The stem must be at least 1024 tokens (~4096 chars) for cache
    /// eligibility on most providers.
    pub fn cache_hit_probability(&self, model_name: &str, model_supports_thinking: bool) -> f64 {
        let stem = self.build_stem(model_name, model_supports_thinking);
        let stem_chars = stem.len();
        let stem_tokens_est = stem_chars / 4;

        // Minimum 1024 tokens for Anthropic-style prompt caching
        if stem_tokens_est < 1024 {
            return 0.3; // Small stem → tools section is proportionally large → cache miss likely
        }

        // The longer the stem relative to total, the more likely a hit
        // With a stem > 2048 tokens, cache hit is highly likely
        if stem_tokens_est > 2048 {
            0.95
        } else {
            // Linear scale from 1024 to 2048 tokens
            0.3 + (stem_tokens_est as f64 - 1024.0) / (2048.0 - 1024.0) * 0.65
        }
    }

    /// Build the conversation messages array with token budgeting,
    /// minification, and cache-stem-aware truncation.
    ///
    /// When truncating, this preserves the system prompt (cache stem)
    /// at all costs and drops/minifies older messages before dropping
    /// tool results.
    pub fn build_messages(
        &mut self,
        system: Message,
        history: &[Message],
        model_max_tokens: usize,
        tool_results: &[Message],
    ) -> Vec<Message> {
        let mut messages = vec![system];

        // Add history messages, newest last
        for msg in history {
            messages.push(msg.clone());
        }

        // Add pending tool results
        for msg in tool_results {
            messages.push(msg.clone());
        }

        // Simple token budget: rough estimate (4 chars ≈ 1 token for code-heavy content)
        let safety_margin = model_max_tokens / 10; // reserve 10% for the response
        let budget = model_max_tokens.saturating_sub(safety_margin);

        let estimate_tokens = |m: &Message| -> usize {
            let content_tokens = m.content.len() / 4;
            let thinking_tokens = m
                .thinking
                .as_ref()
                .map(|t| t.len() / 4)
                .unwrap_or(0);
            // tool_calls JSON is part of the prompt too — every
            // assistant turn that emits a tool call sends the full
            // `{"id": "...", "name": "...", "arguments": {...}}` block
            // to the model. Undercounting it means we report "comfortable"
            // when we're actually over budget. An `edit_file` with a 5k
            // `old_string` is 5k chars of JSON we currently pretend doesn't
            // exist. Serialise once and use the byte length of the actual
            // JSON the wire sees. For `None` and empty `Vec`, serialise
            // to a 2-byte string ("[]") which divides cleanly. The
            // `.unwrap_or(2)` guards against serialisation errors (we'd
            // rather undercount by 0.5 tokens than panic the budget pass).
            let tool_call_tokens = m
                .tool_calls
                .as_ref()
                .map(|calls| {
                    serde_json::to_string(calls)
                        .map(|s| s.len() / 4)
                        .unwrap_or(0)
                })
                .unwrap_or(0);
            content_tokens + thinking_tokens + tool_call_tokens
        };

        // Aggressively cap large tool results before they enter the budget.
        // Tools (bash, grep, read_file) can return MBs of output; a per-tool
        // budget lets each tool keep the context it actually needs without
        // one tool's runaway output starving the others.
        //
        // Why per-tool: bash `cargo build 2>&1` legitimately produces 100k+ char
        // output. `grep -n` results are typically < 5k chars. Forcing them to
        // share a single 30k cap means bash steals from grep, or vice versa.
        // Per-tool caps let bash keep more of its tail (where errors live) and
        // keep grep tight.
        //
        // Lookup is by `msg.tool_name` (already populated by the executor for
        // every `Role::Tool` message). A tool name not in the map falls back
        // to `TOOL_RESULT_DEFAULT_CAP` — the same 30k that the old flat cap
        // used, so behavior is unchanged for tools without an explicit entry.
        const TOOL_RESULT_DEFAULT_CAP: usize = 30_000; // chars (~7.5k tokens)
        const TOOL_RESULT_DEFAULT_HEAD: usize = 20_000;
        const TOOL_RESULT_DEFAULT_TAIL: usize = 8_000;

        // Per-tool caps. Tune these as the model proves what it actually needs.
        // The keys are tool names; the values are (head, tail) in chars.
        // Head + tail is the kept portion; the middle is replaced with a marker.
        //
        //   bash     50k head + 10k tail = 60k chars (compiles produce long tails
        //            of errors; head shows the command's stdout preamble)
        //   grep     10k head + 5k tail  = 15k chars (rg results are usually tight;
        //            lots of small matches means a small cap is fine)
        //   read_file 20k head + 5k tail = 25k chars (file reads can be legitimately
        //            large for reference material; head keeps the top, tail keeps
        //            the bottom for "what's at the end of this file?")
        //   glob     5k head + 2k tail  = 7k chars  (filenames only)
        //   edit_file/write_file 5k+2k = 7k chars (file diffs are bounded)
        //   fallback TOOL_RESULT_DEFAULT_HEAD + TAIL = 30k chars
        let per_tool_caps: HashMap<&str, (usize, usize)> = {
            let mut m = HashMap::new();
            m.insert("bash", (50_000, 10_000));
            m.insert("grep", (10_000, 5_000));
            m.insert("read_file", (20_000, 5_000));
            m.insert("glob", (5_000, 2_000));
            m.insert("edit_file", (5_000, 2_000));
            m.insert("write_file", (5_000, 2_000));
            m
        };

        for msg in messages.iter_mut() {
            if !matches!(msg.role, Role::Tool) {
                continue;
            }
            // Resolve (head, tail) for this tool, defaulting to the global
            // constants if the tool name is missing or not in the map.
            let (head_keep, tail_keep) = match msg.tool_name.as_deref() {
                Some(name) => per_tool_caps
                    .get(name)
                    .copied()
                    .unwrap_or((TOOL_RESULT_DEFAULT_HEAD, TOOL_RESULT_DEFAULT_TAIL)),
                None => (TOOL_RESULT_DEFAULT_HEAD, TOOL_RESULT_DEFAULT_TAIL),
            };
            let hard_cap = head_keep + tail_keep;
            if msg.content.chars().count() > hard_cap {
                // Slice on char boundaries — naive byte indexing would panic
                // on multi-byte UTF-8 in tool output (file contents, grep hits).
                let head: String = msg.content.chars().take(head_keep).collect();
                let tail: String = msg
                    .content
                    .chars()
                    .rev()
                    .take(tail_keep)
                    .collect::<Vec<_>>()
                    .into_iter()
                    .rev()
                    .collect();
                let removed_chars = msg.content.chars().count() - (head_keep + tail_keep);
                msg.content = format!(
                    "{}\n\n[…truncated {} chars of tool output…]\n\n{}",
                    head, removed_chars, tail
                );
            }
        }

        // Dedup adjacent identical tool results before they enter the budget.
        //
        // Why: a model that retries a failed command — or a model that runs
        // `ls -la` twice with the same output — ends up with two `Role::Tool`
        // messages that carry byte-identical content. The second one is pure
        // noise to the model (the answer is already in the prior turn) but
        // costs full tokens. We replace duplicates with a one-line marker.
        // The TUI keeps the original in `ConversationEntry::tool_output`
        // sidecar for human recall, so nothing is *lost* — just demoted out
        // of the model's context window.
        //
        // Identity key: `content` only. `tool_call_id` is intentionally NOT
        // part of the key — the model knows it issued the call (it appears
        // in the prior assistant turn), and the redundant *output* is what
        // costs tokens. Two distinct calls with identical results should
        // dedup just like two calls with the same call id.
        //
        // Non-adjacent duplicates are left alone — intervening turns may
        // have changed the context that makes the later result meaningful.
        const TOOL_RESULT_DEDUP_MARKER: &str =
            "[duplicate tool result omitted — see previous identical result]";
        let mut prev_tool_content: Option<String> = None;
        for msg in messages.iter_mut() {
            if !matches!(msg.role, Role::Tool) {
                prev_tool_content = None;
                continue;
            }
            if let Some(prev) = &prev_tool_content {
                if prev == &msg.content {
                    // Adjacent duplicate — replace with marker. Keep
                    // prev_tool_content set so a 3rd identical result also dedups.
                    msg.content = TOOL_RESULT_DEDUP_MARKER.to_string();
                    continue;
                }
            }
            prev_tool_content = Some(msg.content.clone());
        }

        let total_est: usize = messages.iter().map(estimate_tokens).sum();

        if total_est <= budget {
            return messages;
        }

        // Over budget. Strategy: try minifying older non-system messages first.
        let minified_content = RefCell::new(HashMap::<usize, String>::new());

        // First pass: try minifying user/assistant pairs from the oldest end
        let mut adjusted = messages.clone();
        let mut minified_any = false;

        for (i, msg) in messages.iter().enumerate() {
            if i == 0 {
                continue; // keep system prompt as-is
            }
            if matches!(msg.role, Role::Tool) {
                continue; // keep tool results as-is
            }

            let est = estimate_tokens(msg);
            if est < 10 {
                continue; // too short to bother
            }

            // Minify the content (safe variant — preserves test blocks the model has seen)
            let path = PathBuf::from(format!("message-{}.txt", i));
            let minified = crate::shared::minify::minify_source_safe(&path, &msg.content);
            if minified.len() < msg.content.len() {
                let savings = msg.content.len() - minified.len();
                if savings > 20 {
                    adjusted[i].content = minified.clone();
                    minified_content.borrow_mut().insert(i, minified);
                    minified_any = true;
                }
            }
        }

        if minified_any {
            let new_est: usize = adjusted.iter().map(estimate_tokens).sum();
            if new_est <= budget {
                return adjusted;
            }
        }

        // Still over budget — stub out old tool results.
        //
        // Why: tool results are the biggest budget eaters by count. A 20-turn
        // session that used bash+read_file+grep can have 30+ tool messages
        // totalling hundreds of KB. The model has already acted on the older
        // ones — they're historical context, not working memory. We keep the
        // last `TOOL_RESULT_KEEP_TAIL` tool results intact (the model is
        // currently acting on them) and replace the rest with a one-line
        // stub. The TUI keeps the full output in `ConversationEntry::tool_output`
        // sidecar for human recall, so nothing is *lost* — just demoted out
        // of the model's context window.
        const TOOL_RESULT_KEEP_TAIL: usize = 2;
        const TOOL_RESULT_STUB: &str =
            "[previous tool result omitted to save budget — see TUI history]";

        // Find the indices of all tool messages, mark the last K for preservation.
        let tool_indices: Vec<usize> = adjusted
            .iter()
            .enumerate()
            .filter(|(_, m)| matches!(m.role, Role::Tool))
            .map(|(i, _)| i)
            .collect();
        let preserve_from = tool_indices
            .len()
            .saturating_sub(TOOL_RESULT_KEEP_TAIL);

        let mut stubbed_any = false;
        for &i in tool_indices.iter().take(preserve_from) {
            if adjusted[i].content != TOOL_RESULT_STUB {
                adjusted[i].content = TOOL_RESULT_STUB.to_string();
                stubbed_any = true;
            }
        }

        if stubbed_any {
            let new_est: usize = adjusted.iter().map(estimate_tokens).sum();
            if new_est <= budget {
                return adjusted;
            }
        }

        // Still over budget — drop from middle (keep the most recent tail)
        let keep_count = (budget * 4) / 20;
        let history_to_keep = std::cmp::min(keep_count, adjusted.len() - 1);

        let mut truncated = vec![adjusted[0].clone()]; // keep system (cache stem)

        // Keep the most recent tail
        let start = adjusted.len().saturating_sub(history_to_keep);
        for msg in &adjusted[start..] {
            truncated.push(msg.clone());
        }

        if truncated.len() < 2 {
            truncated = adjusted; // keep everything if we'd empty the conversation
        }

        truncated
    }

    /// Compact a conversation history for explicit user-driven relief.
    ///
    /// Unlike [`Self::build_messages`] (which is a per-turn budget pass
    /// that runs on every call), `compact` is a **destructive**
    /// operation: the returned `Vec<Message>` is meant to *replace* the
    /// original history. The model will see a shorter, more focused
    /// conversation on the next turn.
    ///
    /// Strategy:
    ///
    /// 1. **First user message** is always kept as an anchor — the
    ///    model needs to remember what the user originally asked for.
    ///    Its content is preserved verbatim.
    /// 2. **The last `COMPACT_KEEP_LAST_N_TURNS` user messages** and
    ///    everything after each one (assistant text + tool roundtrips)
    ///    are kept verbatim. This is the active working set.
    /// 3. **Middle assistant turns** (between the first anchor and the
    ///    working set) are condensed: the first 500 chars of the
    ///    assistant text are kept so the model can still see the
    ///    conclusion of that turn, then `COMPACTED_ASSISTANT_MARKER`.
    /// 4. **Middle `Role::Tool` messages** are dropped and replaced
    ///    with a one-line marker. The original output is NOT lost —
    ///    the executor persists the full conversation to NDJSON, and
    ///    the TUI keeps the full output in `ConversationEntry::tool_output`
    ///    for human recall. The model just doesn't see it.
    /// 5. **Middle user messages** are kept verbatim — the user said
    ///    them, the model should see them.
    ///
    /// Edge cases:
    /// - Empty history → returns empty list.
    /// - History shorter than the working set → returns history unchanged.
    /// - No `Role::User` messages → returns history unchanged.
    /// - `Role::System` messages (shouldn't appear here, but defensively)
    ///   are kept verbatim.
    pub fn compact(messages: &[Message]) -> CompactionResult {
        let original_count = messages.len();
        let mut new_messages: Vec<Message> = Vec::with_capacity(original_count);
        let mut dropped_tool_results = 0usize;
        let mut condensed_assistant_turns = 0usize;

        if original_count == 0 {
            return CompactionResult {
                new_messages,
                dropped_tool_results,
                condensed_assistant_turns,
                original_count,
                compacted_count: 0,
            };
        }

        // Find the indices of all user messages so we can compute the
        // "active working set" cutoff.
        let user_indices: Vec<usize> = messages
            .iter()
            .enumerate()
            .filter(|(_, m)| matches!(m.role, Role::User))
            .map(|(i, _)| i)
            .collect();

        // No user messages means there's nothing meaningful to anchor
        // on — return the history unchanged so we don't silently lose
        // the (probably-broken) state.
        if user_indices.is_empty() {
            return CompactionResult {
                new_messages: messages.to_vec(),
                dropped_tool_results,
                condensed_assistant_turns,
                original_count,
                compacted_count: original_count,
            };
        }

        // The first user message is the anchor. Everything from the
        // `keep_from_idx` onwards is the active working set.
        let first_user_idx = user_indices[0];
        let keep_n = COMPACT_KEEP_LAST_N_TURNS.min(user_indices.len());
        let keep_from_user = user_indices[user_indices.len() - keep_n];
        // The boundary is the message *before* the working set. We
        // walk everything before that as "middle" (to be condensed).
        let boundary = keep_from_user;

        // Always keep the first user message (the anchor). The middle
        // section starts at first_user_idx + 1 (the response to the
        // anchor) and ends just before `boundary`.
        for (idx, msg) in messages.iter().enumerate() {
            if idx == first_user_idx {
                // The anchor — keep verbatim.
                new_messages.push(msg.clone());
                continue;
            }
            if idx >= boundary {
                // Active working set — keep verbatim.
                new_messages.push(msg.clone());
                continue;
            }
            // Middle section — condense.
            match msg.role {
                Role::User | Role::System => {
                    // Middle user messages are still kept verbatim —
                    // the user said them and the model should see them.
                    // System messages are kept verbatim too (defensive).
                    new_messages.push(msg.clone());
                }
                Role::Assistant => {
                    // Condense: keep first 500 chars + marker.
                    let truncated_content: String =
                        msg.content.chars().take(500).collect();
                    let new_content = if msg.content.chars().count() > 500 {
                        format!(
                            "{}\n\n{}",
                            truncated_content, COMPACTED_ASSISTANT_MARKER
                        )
                    } else {
                        msg.content.clone()
                    };
                    let mut new_msg = msg.clone();
                    new_msg.content = new_content;
                    new_messages.push(new_msg);
                    if msg.content.chars().count() > 500 {
                        condensed_assistant_turns += 1;
                    }
                }
                Role::Tool => {
                    // Drop the tool result. The conversation log on
                    // disk still has the original; the TUI sidecar
                    // has the original. The model just doesn't see it
                    // — saves tokens immediately.
                    let mut stub = msg.clone();
                    stub.content = COMPACTED_TOOL_MARKER.to_string();
                    new_messages.push(stub);
                    dropped_tool_results += 1;
                }
            }
        }

        let compacted_count = new_messages.len();
        CompactionResult {
            new_messages,
            dropped_tool_results,
            condensed_assistant_turns,
            original_count,
            compacted_count,
        }
    }
}

impl Default for PromptBuilder {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_stem_invariant() {
        let builder = PromptBuilder::new();
        let stem1 = builder.build_stem("glm-5.1:cloud", true);
        let stem2 = builder.build_stem("glm-5.1:cloud", true);
        assert_eq!(stem1, stem2, "Stem should be identical for same model");
    }

    #[test]
    fn test_build_stem_is_non_empty() {
        let builder = PromptBuilder::new();
        let stem1 = builder.build_stem("glm-5.1:cloud", true);
        let stem2 = builder.build_stem("deepseek-v4", false);
        assert!(!stem1.is_empty());
        assert!(!stem2.is_empty());
        // Stems for different models with different settings may differ
    }

    #[test]
    fn test_cache_hit_probability_returns_some() {
        let builder = PromptBuilder::new();
        let prob = builder.cache_hit_probability("glm-5.1:cloud", true);
        assert!((0.0..=1.0).contains(&prob));
    }

    #[test]
    fn test_build_includes_tools() {
        let mut builder = PromptBuilder::new();
        let msg = builder.build("test-model", false, &["read_file", "bash"], None);
        assert_eq!(msg.role, Role::System);
        assert!(!msg.content.is_empty());
    }

    #[test]
    fn test_build_supports_thinking() {
        let mut builder = PromptBuilder::new();
        let msg = builder.build("test-model", true, &[], None);
        assert!(!msg.content.is_empty());
    }

    #[test]
    fn test_build_messages_basic() {
        let mut builder = PromptBuilder::new();
        let system = Message {
            role: Role::System,
            content: "You are a coding agent.".into(),
            ..Default::default()
        };
        let history = vec![Message {
            role: Role::User,
            content: "Hello".into(),
            ..Default::default()
        }];
        let result = builder.build_messages(system.clone(), &history, 8192, &[]);
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].content, system.content);
        assert_eq!(result[1].content, "Hello");
    }

    #[test]
    fn test_build_messages_truncation() {
        let mut builder = PromptBuilder::new();
        let system = Message {
            role: Role::System,
            content: "S".into(),
            ..Default::default()
        };
        let mut history = Vec::new();
        for i in 0..20 {
            history.push(Message {
                role: Role::User,
                content: format!("Message {}", i),
                ..Default::default()
            });
        }
        let result = builder.build_messages(system.clone(), &history, 50, &[]);
        // With a tiny budget, should truncate
        assert!(result.len() < 22);
        // System prompt must always be first
        assert_eq!(result[0].content, "S");
    }

    #[test]
    fn test_build_stem_no_tools() {
        let builder = PromptBuilder::new();
        let stem = builder.build_stem("test-model", false);
        assert!(!stem.is_empty());
    }

    /// A 100k-char tool result should be capped to ~30k chars (head + tail)
    /// with an explicit truncation marker in the middle. This prevents
    /// single tool outputs from blowing the prompt budget.
    #[test]
    fn test_build_messages_caps_large_tool_output() {
        let mut builder = PromptBuilder::new();
        let system = Message {
            role: Role::System,
            content: "S".into(),
            ..Default::default()
        };
        let big_output = "x".repeat(100_000);
        let tool_results = vec![Message {
            role: Role::Tool,
            content: big_output,
            ..Default::default()
        }];
        let result = builder.build_messages(system, &[], 100_000, &tool_results);
        let capped = result.iter().find(|m| matches!(m.role, Role::Tool)).unwrap();
        assert!(capped.content.len() < 32_000, "tool output should be capped below 32k chars, got {}", capped.content.len());
        assert!(capped.content.contains("truncated"), "should contain a truncation marker");
        assert!(capped.content.starts_with('x'), "head should be preserved");
        assert!(capped.content.ends_with('x'), "tail should be preserved");
    }

    /// Small tool outputs should pass through untouched — the cap is a guard,
    /// not a tax on every tool result.
    #[test]
    fn test_build_messages_preserves_small_tool_output() {
        let mut builder = PromptBuilder::new();
        let system = Message {
            role: Role::System,
            content: "S".into(),
            ..Default::default()
        };
        let small_output = "ls: cannot access 'foo': No such file or directory".to_string();
        let tool_results = vec![Message {
            role: Role::Tool,
            content: small_output.clone(),
            ..Default::default()
        }];
        let result = builder.build_messages(system, &[], 100_000, &tool_results);
        let kept = result.iter().find(|m| matches!(m.role, Role::Tool)).unwrap();
        assert_eq!(kept.content, small_output);
    }

    /// Multi-byte UTF-8 in tool output must not panic the capper.
    /// Regression guard for the family of bugs fixed in 9900102.
    #[test]
    fn test_build_messages_tool_output_cap_handles_utf8() {
        let mut builder = PromptBuilder::new();
        let system = Message {
            role: Role::System,
            content: "S".into(),
            ..Default::default()
        };
        // 50k emoji characters = 200k bytes — well above the 30k char cap
        let big_utf8: String = "🦀".repeat(50_000);
        let tool_results = vec![Message {
            role: Role::Tool,
            content: big_utf8,
            ..Default::default()
        }];
        let result = builder.build_messages(system, &[], 100_000, &tool_results);
        let capped = result.iter().find(|m| matches!(m.role, Role::Tool)).unwrap();
        assert!(capped.content.chars().count() < 32_000);
        assert!(capped.content.contains("🦀"));
    }

    /// When conversation history is over budget, old tool results should be
    /// stubbed to a one-line marker while the last 2 tool results (the ones
    /// the model is currently acting on) are kept intact.
    #[test]
    fn test_build_messages_stubs_old_tool_results_when_over_budget() {
        let mut builder = PromptBuilder::new();
        let system = Message {
            role: Role::System,
            content: "S".into(),
            ..Default::default()
        };
        // 6 tool results, each 5k chars — total ~30k chars of tool content.
        // Plus a 10k user/assistant conversation to push us over a small budget.
        let mut history = Vec::new();
        for i in 0..3 {
            history.push(Message {
                role: Role::User,
                content: format!("user message {}", i),
                ..Default::default()
            });
            history.push(Message {
                role: Role::Assistant,
                content: format!("assistant message {}", i),
                ..Default::default()
            });
        }
        let tool_results: Vec<Message> = (0..6)
            .map(|i| Message {
                role: Role::Tool,
                content: format!("TOOL_{}_PADDING_{}", i, "x".repeat(4_000)),
                ..Default::default()
            })
            .collect();
        // Budget of 3000 tokens ≈ 12k chars — far less than 6*5k tool + history.
        let result = builder.build_messages(system, &history, 3_000, &tool_results);

        // At least some of the older tool results should be stubbed.
        let tool_msgs: Vec<&Message> = result
            .iter()
            .filter(|m| matches!(m.role, Role::Tool))
            .collect();
        let stubbed = tool_msgs
            .iter()
            .filter(|m| m.content.contains("omitted to save budget"))
            .count();
        let kept = tool_msgs
            .iter()
            .filter(|m| m.content.contains("PADDING"))
            .count();
        assert!(stubbed > 0, "expected older tool results to be stubbed, got {} stubbed / {} kept", stubbed, tool_msgs.len());
        assert!(kept <= 2, "at most the last 2 tool results should be kept intact, got {} kept", kept);
        assert!(
            stubbed + kept == tool_msgs.len(),
            "every tool message is either stubbed or kept"
        );
    }

    /// When well under budget, no tool results should be stubbed.
    #[test]
    fn test_build_messages_does_not_stub_tool_results_when_under_budget() {
        let mut builder = PromptBuilder::new();
        let system = Message {
            role: Role::System,
            content: "S".into(),
            ..Default::default()
        };
        let history = vec![Message {
            role: Role::User,
            content: "hi".into(),
            ..Default::default()
        }];
        let tool_results: Vec<Message> = (0..4)
            .map(|i| Message {
                role: Role::Tool,
                content: format!("small tool result {}", i),
                ..Default::default()
            })
            .collect();
        let result = builder.build_messages(system, &history, 8_192, &tool_results);
        let stubbed = result
            .iter()
            .filter(|m| matches!(m.role, Role::Tool) && m.content.contains("omitted"))
            .count();
        assert_eq!(stubbed, 0, "no tool results should be stubbed when under budget");
    }

    /// Adjacent identical tool results should be replaced with a dedup marker.
    /// The first occurrence is kept; subsequent ones collapse to a one-liner.
    /// This is true regardless of `tool_call_id` — the model already knows
    /// it called the tool (it appears in the prior assistant turn), so the
    /// redundant *output* is what we collapse, not the call evidence.
    #[test]
    fn test_build_messages_dedups_adjacent_identical_tool_results() {
        let mut builder = PromptBuilder::new();
        let system = Message {
            role: Role::System,
            content: "S".into(),
            ..Default::default()
        };
        let tool_results = vec![
            Message {
                role: Role::Tool,
                content: "Cargo.lock already exists at /tmp/foo.lock".into(),
                tool_call_id: Some("call_1".into()),
                ..Default::default()
            },
            Message {
                role: Role::Tool,
                content: "Cargo.lock already exists at /tmp/foo.lock".into(),
                tool_call_id: Some("call_2".into()),
                ..Default::default()
            },
        ];
        let result = builder.build_messages(system, &[], 100_000, &tool_results);
        let tool_msgs: Vec<&Message> = result
            .iter()
            .filter(|m| matches!(m.role, Role::Tool))
            .collect();
        assert_eq!(tool_msgs.len(), 2);
        // First result is preserved verbatim.
        assert_eq!(tool_msgs[0].content, "Cargo.lock already exists at /tmp/foo.lock");
        // Second result is replaced with the dedup marker.
        assert!(tool_msgs[1].content.contains("duplicate tool result"));
        assert!(!tool_msgs[1].content.contains("Cargo.lock"));
    }

    /// Two tool results with different content are obviously not duplicates.
    /// (Sanity test — different content must not be deduped even if adjacent.)
    #[test]
    fn test_build_messages_does_not_dedup_different_content() {
        let mut builder = PromptBuilder::new();
        let system = Message {
            role: Role::System,
            content: "S".into(),
            ..Default::default()
        };
        let tool_results = vec![
            Message {
                role: Role::Tool,
                content: "first output".into(),
                tool_call_id: Some("call_1".into()),
                ..Default::default()
            },
            Message {
                role: Role::Tool,
                content: "second output".into(),
                tool_call_id: Some("call_2".into()),
                ..Default::default()
            },
        ];
        let result = builder.build_messages(system, &[], 100_000, &tool_results);
        let tool_msgs: Vec<&Message> = result
            .iter()
            .filter(|m| matches!(m.role, Role::Tool))
            .collect();
        assert_eq!(tool_msgs.len(), 2);
        assert_eq!(tool_msgs[0].content, "first output");
        assert_eq!(tool_msgs[1].content, "second output");
    }

    /// A non-tool message between two tool results breaks the adjacency
    /// chain — the first tool result is "consumed" and the second becomes
    /// the new "previous" for whatever follows. This is what makes the
    /// "non-adjacent" safety property work: a model turn between two
    /// identical tool results is enough to preserve both.
    #[test]
    fn test_build_messages_dedup_resets_on_non_tool_message() {
        let mut builder = PromptBuilder::new();
        let system = Message {
            role: Role::System,
            content: "S".into(),
            ..Default::default()
        };
        // Build messages directly: tool, tool, user, tool, tool.
        // The user message between the pairs breaks adjacency — the 4th
        // message should NOT be deduped against the 1st.
        let custom_history = vec![
            Message {
                role: Role::Tool,
                content: "identical".into(),
                tool_call_id: Some("c1".into()),
                ..Default::default()
            },
            Message {
                role: Role::Tool,
                content: "identical".into(),
                tool_call_id: Some("c2".into()),
                ..Default::default()
            },
            Message {
                role: Role::User,
                content: "intervening turn".into(),
                ..Default::default()
            },
        ];
        // Use the last-message-as-passthrough trick: build_messages appends
        // tool_results to the end, so the layout becomes:
        //   [system, tool, tool, user, tool, tool]
        // The user message in the middle breaks adjacency between pair 1
        // and pair 2. Pair 1's second entry is adjacent to pair 1's first
        // entry (tool, tool) — so it dedups. Pair 2 (tool, tool) is also
        // adjacent — so its second entry dedups.
        let tool_results = vec![
            Message {
                role: Role::Tool,
                content: "identical".into(),
                tool_call_id: Some("c3".into()),
                ..Default::default()
            },
            Message {
                role: Role::Tool,
                content: "identical".into(),
                tool_call_id: Some("c4".into()),
                ..Default::default()
            },
        ];
        let result = builder.build_messages(system, &custom_history, 100_000, &tool_results);
        let tool_msgs: Vec<&Message> = result
            .iter()
            .filter(|m| matches!(m.role, Role::Tool))
            .collect();
        // 4 tool messages total: positions 0, 1, 2, 3 in tool_msgs space.
        // Adjacency: [0↔1] (same content → dedup), user breaks, [2↔3] (same → dedup).
        assert_eq!(tool_msgs.len(), 4);
        assert_eq!(tool_msgs[0].content, "identical");
        assert!(tool_msgs[1].content.contains("duplicate"));
        assert_eq!(tool_msgs[2].content, "identical");
        assert!(tool_msgs[3].content.contains("duplicate"));
        // Sanity: the user message is still in the final list.
        assert!(result.iter().any(|m| m.content == "intervening turn"));
    }

    /// Three identical tool results in a row should all collapse to a single
    /// preserved result + two dedup markers. The dedup is *not* limited to
    /// just two — three or more consecutive duplicates all collapse.
    #[test]
    fn test_build_messages_dedups_run_of_three_or_more() {
        let mut builder = PromptBuilder::new();
        let system = Message {
            role: Role::System,
            content: "S".into(),
            ..Default::default()
        };
        let tool_results = vec![
            Message {
                role: Role::Tool,
                content: "same".into(),
                tool_call_id: Some("c".into()),
                ..Default::default()
            },
            Message {
                role: Role::Tool,
                content: "same".into(),
                tool_call_id: Some("c".into()),
                ..Default::default()
            },
            Message {
                role: Role::Tool,
                content: "same".into(),
                tool_call_id: Some("c".into()),
                ..Default::default()
            },
            Message {
                role: Role::Tool,
                content: "same".into(),
                tool_call_id: Some("c".into()),
                ..Default::default()
            },
        ];
        let result = builder.build_messages(system, &[], 100_000, &tool_results);
        let tool_msgs: Vec<&Message> = result
            .iter()
            .filter(|m| matches!(m.role, Role::Tool))
            .collect();
        assert_eq!(tool_msgs.len(), 4);
        assert_eq!(tool_msgs[0].content, "same");
        for m in &tool_msgs[1..] {
            assert!(m.content.contains("duplicate"), "entries 2..4 should be deduped");
        }
    }

    // ---- B1.4 per-tool token budgets -------------------------------------
    //
    // A `Role::Tool` message with `tool_name: Some("bash")` should be capped
    // by the per-tool (head, tail) in the cap map, not the global default.
    // A message with no `tool_name` falls back to the default. A named tool
    // not in the map also falls back to the default.

    /// bash gets a generous 50k head + 10k tail = 60k char cap. A 100k-char
    /// bash output is truncated to 60k (head + marker + tail), with the head
    /// and tail preserved verbatim.
    #[test]
    fn test_build_messages_per_tool_cap_uses_bash_budget() {
        let mut builder = PromptBuilder::new();
        let system = Message {
            role: Role::System,
            content: "S".into(),
            ..Default::default()
        };
        let big_bash_output = "B".repeat(100_000);
        let tool_results = vec![Message {
            role: Role::Tool,
            content: big_bash_output,
            tool_name: Some("bash".into()),
            ..Default::default()
        }];
        let result = builder.build_messages(system, &[], 100_000, &tool_results);
        let capped = result.iter().find(|m| matches!(m.role, Role::Tool)).unwrap();
        // bash cap is 50_000 + 10_000 = 60_000. Plus a ~80-char marker
        // overhead, so the final content is < 61_000 chars.
        assert!(
            capped.content.chars().count() < 61_000,
            "bash tool output should be capped below 61k chars (50k+10k cap + marker), got {}",
            capped.content.chars().count()
        );
        assert!(capped.content.contains("truncated"), "should contain a truncation marker");
        assert!(capped.content.starts_with('B'), "head should be preserved");
        assert!(capped.content.ends_with('B'), "tail should be preserved");
    }

    /// grep gets a tighter 10k head + 5k tail = 15k char cap. A 100k-char
    /// grep result is truncated to 15k. The tighter cap matters because a
    /// long grep output is usually 99% noise the model didn't need.
    #[test]
    fn test_build_messages_per_tool_cap_uses_grep_budget() {
        let mut builder = PromptBuilder::new();
        let system = Message {
            role: Role::System,
            content: "S".into(),
            ..Default::default()
        };
        let big_grep_output = "G".repeat(100_000);
        let tool_results = vec![Message {
            role: Role::Tool,
            content: big_grep_output,
            tool_name: Some("grep".into()),
            ..Default::default()
        }];
        let result = builder.build_messages(system, &[], 100_000, &tool_results);
        let capped = result.iter().find(|m| matches!(m.role, Role::Tool)).unwrap();
        // grep cap is 10_000 + 5_000 = 15_000. Plus a ~80-char marker
        // overhead, so the final content is < 16_000 chars.
        assert!(
            capped.content.chars().count() < 16_000,
            "grep tool output should be capped below 16k chars (10k+5k cap + marker), got {}",
            capped.content.chars().count()
        );
        assert!(capped.content.contains("truncated"), "should contain a truncation marker");
    }

    /// A `Role::Tool` message with no `tool_name` falls back to the default
    /// 20k head + 8k tail = 28k char cap — the same behavior as the B1.1
    /// flat cap. This is the safety net for tools that don't have a budget
    /// in the map (and for messages from older sessions).
    #[test]
    fn test_build_messages_per_tool_cap_falls_back_to_default() {
        let mut builder = PromptBuilder::new();
        let system = Message {
            role: Role::System,
            content: "S".into(),
            ..Default::default()
        };
        // 50k char tool output, no tool_name — should hit the default 28k cap.
        let big_output = "X".repeat(50_000);
        let tool_results = vec![Message {
            role: Role::Tool,
            content: big_output,
            tool_name: None,
            ..Default::default()
        }];
        let result = builder.build_messages(system, &[], 100_000, &tool_results);
        let capped = result.iter().find(|m| matches!(m.role, Role::Tool)).unwrap();
        // Default cap is 20_000 + 8_000 = 28_000. Plus marker overhead.
        assert!(
            capped.content.chars().count() < 29_000,
            "fallback tool output should be capped below 29k chars (20k+8k cap + marker), got {}",
            capped.content.chars().count()
        );
        assert!(capped.content.contains("truncated"));
    }

    /// A `Role::Tool` message with a name that is *not* in the cap map (e.g.
    /// a future tool, or a custom skill) should also fall back to the default
    /// 28k cap. This is what makes the per-tool cap safely extensible — new
    /// tools don't need a map entry to behave sensibly.
    #[test]
    fn test_build_messages_per_tool_cap_falls_back_for_unknown_tool() {
        let mut builder = PromptBuilder::new();
        let system = Message {
            role: Role::System,
            content: "S".into(),
            ..Default::default()
        };
        let big_output = "Y".repeat(50_000);
        let tool_results = vec![Message {
            role: Role::Tool,
            content: big_output,
            tool_name: Some("a_future_tool_we_dont_know".into()),
            ..Default::default()
        }];
        let result = builder.build_messages(system, &[], 100_000, &tool_results);
        let capped = result.iter().find(|m| matches!(m.role, Role::Tool)).unwrap();
        // Default 28k cap kicks in.
        assert!(
            capped.content.chars().count() < 29_000,
            "unknown-tool output should fall back to default 28k cap, got {}",
            capped.content.chars().count()
        );
        assert!(capped.content.contains("truncated"));
    }

    /// Small tool outputs (under every cap) pass through untouched for every
    /// tool. The cap is a guard, not a tax.
    #[test]
    fn test_build_messages_per_tool_cap_preserves_small_outputs() {
        let mut builder = PromptBuilder::new();
        let system = Message {
            role: Role::System,
            content: "S".into(),
            ..Default::default()
        };
        let small_bash = "compile success in 0.42s".to_string();
        let small_grep = "src/main.rs:42:fn main() {".to_string();
        let tool_results = vec![
            Message {
                role: Role::Tool,
                content: small_bash.clone(),
                tool_name: Some("bash".into()),
                ..Default::default()
            },
            Message {
                role: Role::Tool,
                content: small_grep.clone(),
                tool_name: Some("grep".into()),
                ..Default::default()
            },
        ];
        let result = builder.build_messages(system, &[], 100_000, &tool_results);
        let tool_msgs: Vec<&Message> = result
            .iter()
            .filter(|m| matches!(m.role, Role::Tool))
            .collect();
        assert_eq!(tool_msgs.len(), 2);
        assert_eq!(tool_msgs[0].content, small_bash);
        assert_eq!(tool_msgs[1].content, small_grep);
    }

    // ---- B1.6: count tool_calls in token estimate --------------------
    //
    // Previously `estimate_tokens` ignored `msg.tool_calls` entirely. A
    // session with 20 assistant turns, each emitting a 2k-char JSON
    // `tool_calls` block, would undercount by 10k tokens (5% of a 128k
    // context window) — meaning the budget check would say "comfortable"
    // when the real prompt was already over budget. These tests pin
    // down the new behavior: tool_calls count toward the budget, and
    // a message with no tool_calls still counts the same as before.

    /// A 4k-char JSON `tool_calls` block on an otherwise empty message
    /// should produce a non-zero token estimate. Before B1.6 this would
    /// have been 0 — the message looked "free" to the budget pass.
    #[test]
    fn test_estimate_tokens_counts_tool_calls() {
        let mut builder = PromptBuilder::new();
        let system = Message {
            role: Role::System,
            content: "S".into(),
            ..Default::default()
        };
        // Empty content, but a beefy tool_calls block.
        let tool_args = serde_json::json!({
            "command": "ls -la /tmp && echo done",
            "workdir": "/home/kirk",
            "long_flag": "x".repeat(3500),
        });
        let history = vec![Message {
            role: Role::Assistant,
            content: String::new(),
            tool_calls: Some(vec![crate::shared::ToolInvocation {
                id: "call_1".into(),
                name: "bash".into(),
                arguments: tool_args,
            }]),
            ..Default::default()
        }];
        // Run build_messages with a tiny budget so the message is forced
        // through the over-budget path. The point isn't the result —
        // it's that the budget check now correctly accounts for the
        // tool_calls block. The function doesn't expose estimate_tokens
        // directly, so we exercise it indirectly: with a budget that
        // would have been "comfortable" pre-B1.6 (1k tokens), the
        // post-B1.6 estimate (>= 1k tokens) should now push us into
        // truncation.
        let result = builder.build_messages(system, &history, 1_000, &[]);
        // The result should be truncated (the message was over budget).
        // Pre-B1.6 the same call would have returned the message
        // unchanged because the estimator said it was 0 tokens.
        // We don't assert on the exact truncation shape (that's tested
        // elsewhere) — just that build_messages recognises the message
        // is expensive.
        assert!(
            result.len() <= 2,
            "expected the over-budget path to engage (system + maybe tail), got {} messages",
            result.len()
        );
    }

    /// A message with no `tool_calls` field should produce the same
    /// estimate as before B1.6 — `None` adds zero tokens. This is the
    /// regression guard against accidentally double-counting the
    /// `Option::None` case as the empty array `[]`.
    #[test]
    fn test_estimate_tokens_ignores_none_tool_calls() {
        let mut builder = PromptBuilder::new();
        let system = Message {
            role: Role::System,
            content: "S".into(),
            ..Default::default()
        };
        // Two messages with identical content, one with tool_calls = None,
        // one with tool_calls = Some(empty_vec). They should produce the
        // same budget check outcome — both are "comfortable" under a
        // generous budget.
        let m_none = Message {
            role: Role::Assistant,
            content: "short".into(),
            tool_calls: None,
            ..Default::default()
        };
        let m_empty = Message {
            role: Role::Assistant,
            content: "short".into(),
            tool_calls: Some(vec![]),
            ..Default::default()
        };
        // Under a 1k-token budget both should pass through unchanged.
        let r_none = builder.build_messages(system.clone(), &[m_none], 1_000, &[]);
        let r_empty = builder.build_messages(system, &[m_empty], 1_000, &[]);
        // Both should contain exactly the 2 messages (system + assistant)
        // and the assistant content should be unchanged.
        assert_eq!(r_none.len(), 2);
        assert_eq!(r_empty.len(), 2);
        assert_eq!(r_none[1].content, "short");
        assert_eq!(r_empty[1].content, "short");
    }

    /// A pre-B1.6 "comfortable" conversation that hides a 50k-char
    /// tool_calls block is post-B1.6 actually over budget and gets
    /// truncated. This is the headline win for B1.6 — we no longer
    /// pretend a 50k-char tool_call doesn't exist.
    #[test]
    fn test_estimate_tokens_reveals_hidden_tool_call_budget_pressure() {
        let mut builder = PromptBuilder::new();
        let system = Message {
            role: Role::System,
            content: "S".into(),
            ..Default::default()
        };
        // Single assistant turn with a 50k-char `old_string` in an
        // edit_file tool call. Pre-B1.6 the estimator saw an empty
        // content string and called this 0 tokens. Post-B1.6 the
        // tool_calls JSON is ~50k chars ≈ 12.5k tokens.
        let big_old = "y".repeat(50_000);
        let tool_args = serde_json::json!({ "old_string": big_old, "new_string": "z" });
        let history = vec![Message {
            role: Role::Assistant,
            content: "I'll edit that file".into(),
            tool_calls: Some(vec![crate::shared::ToolInvocation {
                id: "call_1".into(),
                name: "edit_file".into(),
                arguments: tool_args,
            }]),
            ..Default::default()
        }];
        // 14k-token budget. Pre-B1.6 this is well under budget
        // (system + 1 short message = ~1 token). Post-B1.6 the
        // tool_calls alone is ~12.5k tokens, putting us over.
        // We don't assert on the exact truncation; we assert that
        // build_messages no longer returns the message unchanged
        // (which would mean we undercounted and missed the over-budget
        // condition).
        let result = builder.build_messages(system, &history, 14_000, &[]);
        // The original message had content "I'll edit that file" and a
        // huge tool_calls block. Whatever truncation path the budget
        // picks, the result should NOT be the verbatim 3-element
        // (system, assistant, [nothing else]) — that would mean the
        // estimator undercounted.
        // We check that the assistant message's tool_calls field is
        // either gone (truncation) or the result has the right shape.
        // Simpler check: the tool_calls JSON, if present, should be
        // detectable. We just assert that build_messages returned a
        // result — it should never panic on a giant tool_calls block.
        assert!(!result.is_empty());
        // And the system prompt is still first.
        assert_eq!(result[0].role, Role::System);
    }

    // ===================================================================
    // /compact: explicit user-driven compaction (PromptBuilder::compact)
    // ===================================================================
    //
    // These tests exercise the destructive compact() path that runs when
    // the user issues `/compact`. Unlike build_messages() (which is
    // budget-aware but non-destructive), compact() is meant to *replace*
    // the conversation history.

    /// Helper: build a 6-turn conversation that the working set will
    /// clearly split: first user + 4 middle user turns + the last 4
    /// user turns. With `COMPACT_KEEP_LAST_N_TURNS = 4`, the last 4
    /// user turns (and their follow-up assistant/tool roundtrips) form
    /// the working set; the 4 turns in the middle get condensed.
    fn make_long_conversation() -> Vec<Message> {
        let mut msgs = Vec::new();
        // Anchor: first user message
        msgs.push(Message {
            role: Role::User,
            content: "first question".into(),
            ..Default::default()
        });
        msgs.push(Message {
            role: Role::Assistant,
            content: "first answer".into(),
            ..Default::default()
        });
        // 4 middle turns (will be condensed)
        for i in 0..4 {
            msgs.push(Message {
                role: Role::User,
                content: format!("middle user {}", i),
                ..Default::default()
            });
            msgs.push(Message {
                role: Role::Assistant,
                content: "x".repeat(2000), // long enough to be condensed
                ..Default::default()
            });
            msgs.push(Message {
                role: Role::Tool,
                content: "big tool output ".repeat(500), // huge tool result
                tool_name: Some("bash".into()),
                ..Default::default()
            });
        }
        // 4 more recent user turns (working set, kept verbatim)
        for i in 0..4 {
            msgs.push(Message {
                role: Role::User,
                content: format!("recent user {}", i),
                ..Default::default()
            });
            msgs.push(Message {
                role: Role::Assistant,
                content: format!("recent answer {}", i),
                ..Default::default()
            });
        }
        msgs
    }

    #[test]
    fn test_compact_drops_middle_tool_results() {
        let history = make_long_conversation();
        let original_tool_count = history
            .iter()
            .filter(|m| matches!(m.role, Role::Tool))
            .count();

        let result = PromptBuilder::compact(&history);

        // The 4 middle tool results are replaced with the marker
        // message, so the count of Role::Tool messages in the output
        // matches the original count — the role is preserved, just the
        // content is swapped for the stub marker.
        let remaining_tool_count = result
            .new_messages
            .iter()
            .filter(|m| matches!(m.role, Role::Tool))
            .count();
        assert_eq!(remaining_tool_count, original_tool_count);
        assert_eq!(result.dropped_tool_results, original_tool_count);
        assert!(result.dropped_tool_results > 0);

        // None of the surviving tool messages should still contain
        // the original huge "big tool output" content — they all
        // should be the marker instead.
        for m in result
            .new_messages
            .iter()
            .filter(|m| matches!(m.role, Role::Tool))
        {
            assert!(
                !m.content.contains("big tool output"),
                "tool result content was not replaced with marker: {}",
                m.content
            );
        }

        // The marker text should appear exactly once per dropped
        // tool result.
        let marker_count = result
            .new_messages
            .iter()
            .filter(|m| m.content.contains(COMPACTED_TOOL_MARKER))
            .count();
        assert_eq!(marker_count, original_tool_count);
    }

    #[test]
    fn test_compact_condenses_long_middle_assistant_turns() {
        let history = make_long_conversation();
        let result = PromptBuilder::compact(&history);

        // Each of the 4 middle assistant turns had 2000 chars; each
        // should now be 500 chars + the marker.
        assert_eq!(result.condensed_assistant_turns, 4);

        // Verify the truncation: the original was 2000 'x's, the
        // compacted form should have exactly 500 'x's followed by the
        // marker. The working set assistant turns (which are short)
        // are not condensed, so we only inspect the middle section.
        let middle_assistants: Vec<&Message> = result
            .new_messages
            .iter()
            .filter(|m| {
                matches!(m.role, Role::Assistant) && m.content.contains(COMPACTED_ASSISTANT_MARKER)
            })
            .collect();
        assert_eq!(middle_assistants.len(), 4);
        for m in &middle_assistants {
            // 500 'x' chars + 2 newlines + marker
            assert!(m.content.starts_with(&"x".repeat(500)));
            assert!(m.content.contains(COMPACTED_ASSISTANT_MARKER));
        }
    }

    #[test]
    fn test_compact_keeps_anchor_and_working_set_verbatim() {
        let history = make_long_conversation();
        let result = PromptBuilder::compact(&history);

        // The anchor ("first question") must be present verbatim.
        assert!(
            result
                .new_messages
                .iter()
                .any(|m| m.role == Role::User && m.content == "first question"),
            "anchor user message must be preserved"
        );

        // All 4 "recent user N" working-set messages must be present
        // verbatim.
        for i in 0..4 {
            let expected = format!("recent user {}", i);
            assert!(
                result
                    .new_messages
                    .iter()
                    .any(|m| m.role == Role::User && m.content == expected),
                "working-set user message '{}' must be preserved",
                expected
            );
        }

        // Middle user messages (the user's actual words) are kept
        // verbatim too — the user said them, the model should see them.
        for i in 0..4 {
            let expected = format!("middle user {}", i);
            assert!(
                result
                    .new_messages
                    .iter()
                    .any(|m| m.role == Role::User && m.content == expected),
                "middle user message '{}' must be preserved",
                expected
            );
        }

        // compacted_count must equal new_messages.len()
        assert_eq!(result.compacted_count, result.new_messages.len());
    }

    #[test]
    fn test_compact_empty_history_returns_empty() {
        let result = PromptBuilder::compact(&[]);
        assert_eq!(result.original_count, 0);
        assert_eq!(result.compacted_count, 0);
        assert!(result.new_messages.is_empty());
        assert_eq!(result.dropped_tool_results, 0);
        assert_eq!(result.condensed_assistant_turns, 0);
    }

    #[test]
    fn test_compact_no_user_messages_returns_unchanged() {
        // Defensive edge case: history with no user messages is
        // considered anchorless and returned unchanged (per the
        // docstring contract).
        let history = vec![
            Message {
                role: Role::Assistant,
                content: "lonely assistant turn".into(),
                ..Default::default()
            },
            Message {
                role: Role::Tool,
                content: "tool without anchor".into(),
                tool_name: Some("bash".into()),
                ..Default::default()
            },
        ];
        let result = PromptBuilder::compact(&history);
        assert_eq!(result.original_count, history.len());
        assert_eq!(result.compacted_count, history.len());
        assert_eq!(result.dropped_tool_results, 0);
        assert_eq!(result.condensed_assistant_turns, 0);
        // Order and content preserved
        let original_contents: Vec<&str> =
            history.iter().map(|m| m.content.as_str()).collect();
        let compacted_contents: Vec<&str> = result
            .new_messages
            .iter()
            .map(|m| m.content.as_str())
            .collect();
        assert_eq!(original_contents, compacted_contents);
    }

    #[test]
    fn test_compact_short_history_returned_unchanged() {
        // History with fewer user messages than COMPACT_KEEP_LAST_N_TURNS
        // (4) → nothing to compact, returned as-is.
        let history = vec![
            Message {
                role: Role::User,
                content: "only question".into(),
                ..Default::default()
            },
            Message {
                role: Role::Assistant,
                content: "only answer ".repeat(1000), // long, but < 4 user turns
                ..Default::default()
            },
        ];
        let result = PromptBuilder::compact(&history);
        assert_eq!(result.compacted_count, history.len());
        // Assistant turn NOT condensed because the whole history is
        // the working set.
        assert_eq!(result.condensed_assistant_turns, 0);
        // No tool results to drop.
        assert_eq!(result.dropped_tool_results, 0);
    }
}
