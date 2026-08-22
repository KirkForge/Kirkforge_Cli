pub mod cache_stem;
pub(crate) mod compaction;
pub(crate) mod microcompaction;
pub mod summarizer;

use crate::shared::metrics::{record, MetricEvent, PlanDecisionKind};
use crate::shared::{Message, Role};
use std::collections::HashMap;
use std::path::PathBuf;

pub use cache_stem::{shared_context_stem, CacheStemTracker, DEFAULT_TOP_N_FILES};
pub use compaction::CompactRequest;
pub(crate) use compaction::{compact_to_budget, estimate_tokens};

use kf_context_index::ContextIndex;

/// Token counter — cl100k_base via `kf_budget_core::estimate_tokens`.
/// The `budget` feature is in `default`, so this is always available
/// in release builds.
pub(crate) fn count_tokens(s: &str) -> usize {
    kf_budget_core::estimate_tokens(s)
}

/// Estimate the token count of a single message (content + thinking +
/// tool_calls JSON). When `msg.token_count` is `Some`, the cached value
/// is returned directly — avoiding a full BPE pass. This is the shared
/// implementation used by `ConversationLog::append` and the per-module
/// `estimate_message_tokens` copies.
pub(crate) fn estimate_message_tokens(m: &Message) -> usize {
    if let Some(c) = m.token_count {
        return c;
    }
    let content_tokens = count_tokens(&m.content);
    let thinking_tokens = m.thinking.as_ref().map(|t| count_tokens(t)).unwrap_or(0);
    let tool_call_tokens = m
        .tool_calls
        .as_ref()
        .map(|calls| {
            serde_json::to_string(calls)
                .map(|s| count_tokens(&s))
                .unwrap_or(0)
        })
        .unwrap_or(0);
    content_tokens + thinking_tokens + tool_call_tokens
}

/// Number of trailing messages kept verbatim by automatic microcompaction.
/// Mirrors `Config::preserve_recent_messages` semantics: the live user turn
/// and the most recent assistant turn stay intact so the model does not lose
/// the immediate thread.
const DEFAULT_MICROCOMPACT_KEEP_TAIL: usize = 5;

pub struct PromptBuilder {
    template: String,
    /// When `Some`, replaces the base template entirely. Set from the
    /// `--system` CLI flag (or future config knob). `None` means "use
    /// `prompts/system.hbs`" — the historical behavior.
    ///
    /// This was the source of GPT 5.5's review finding #2 ("--system is
    /// accepted but ignored"). The flag used to be parsed, logged, and
    /// dropped on the floor; this field is where the value actually
    /// lives now.
    system_override: Option<String>,
    /// Cached system-prompt stem produced by `build_stable()` on the
    /// first call and reused when the stable inputs (model name, tool
    /// list, system override) have not changed. Volatile content
    /// (carryover, memory facts) is NOT in the stem — it goes through
    /// `build_dynamic_suffix()` so the stem cache key is stable across
    /// turns and the provider KV cache actually hits (WO 38.9 item 6).
    cached_system: Option<(SystemStemKey, Message)>,
    /// Optional repo-graph context index for injecting relevant symbols
    /// into the system prompt before every turn.
    context_index: Option<ContextIndex>,
    /// Mtime cache for the memory store directory (WO 38.9 item 4).
    /// When the directory mtime hasn't changed, the parsed facts are
    /// reused without re-reading from disk. The cache stores the
    /// directory's last-modified time and the facts parsed at that
    /// time. An mtime of `None` means "cache cold — read on next use".
    memory_cache: Option<(
        std::time::SystemTime,
        Vec<crate::session::memory::MemoryFact>,
    )>,
}

/// Hashable key for the memoised system prompt stem.
///
/// Only STABLE fields are included: model name, thinking support, tool
/// list, and the system override. Volatile fields (carryover block,
/// memory context, memory facts) are NOT part of the key — they go
/// through `build_dynamic_suffix()` so the stem is byte-identical
/// across turns (WO 38.9 item 6).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct SystemStemKey {
    model_name: String,
    model_supports_thinking: bool,
    tool_names: Vec<String>,
    memory_enabled: bool,
    system_override_hash: Option<u64>,
}

impl SystemStemKey {
    fn new(
        model_name: &str,
        model_supports_thinking: bool,
        tool_names: &[&str],
        memory_enabled: bool,
        system_override: Option<&str>,
    ) -> Self {
        Self {
            model_name: model_name.to_string(),
            model_supports_thinking,
            tool_names: tool_names.iter().map(|s| s.to_string()).collect(),
            memory_enabled,
            system_override_hash: system_override.map(|s| {
                use std::collections::hash_map::DefaultHasher;
                use std::hash::{Hash, Hasher};
                let mut hasher = DefaultHasher::new();
                s.hash(&mut hasher);
                hasher.finish()
            }),
        }
    }
}

impl PromptBuilder {
    pub fn new() -> Self {
        let template = include_str!("../../../prompts/system.hbs");
        Self {
            template: template.to_string(),
            system_override: None,
            cached_system: None,
            context_index: None,
            memory_cache: None,
        }
    }

    /// Attach a repo-graph context index for injecting relevant symbols
    /// into the system prompt before every turn.
    pub fn with_context_index(mut self, idx: ContextIndex) -> Self {
        self.context_index = Some(idx);
        self
    }

    /// Install a full system-prompt override. The next `build()` call
    /// will return a single system message with this content instead
    /// of rendering the base template. Pass `None` (or call
    /// `clear_system_override`) to revert to the template.
    ///
    /// This is a **full** override, not an append: if the operator
    /// wants the base safety scaffolding, they need to embed it in
    /// their override. The trade-off is predictability — the operator
    /// sees exactly the prompt they're running with, no hidden
    /// behavior.
    pub fn set_system_override(&mut self, override_prompt: Option<String>) {
        self.system_override = override_prompt;
    }

    /// Returns the current system override, if any.
    pub fn system_override(&self) -> Option<&str> {
        self.system_override.as_deref()
    }

    // reason: build params mirror the old signature for backward compat;
    // volatile params are passed through to build_dynamic_suffix.
    #[allow(clippy::too_many_arguments)]
    pub fn build(
        &mut self,
        model_name: &str,
        model_supports_thinking: bool,
        tool_names: &[&str],
        _carryover_block: Option<&str>,
        _memory_context: Option<&str>,
        memory_enabled: bool,
        _memory_max_tokens: usize,
        _memory_top_n: usize,
    ) -> Message {
        self.build_stable(
            model_name,
            model_supports_thinking,
            tool_names,
            memory_enabled,
        )
    }

    /// Build the STABLE system-prompt stem (template + tools + `remember`
    /// instruction when memory is enabled). Volatile content (carryover,
    /// memory facts) is NOT included — it goes through
    /// [`build_dynamic_suffix`] so the stem is byte-identical across
    /// turns and the provider KV cache hits (WO 38.9 item 6).
    ///
    /// The stem is memoised by `SystemStemKey` — a cache hit returns the
    /// same `Message` object so the content hash is stable.
    fn build_stable(
        &mut self,
        model_name: &str,
        model_supports_thinking: bool,
        tool_names: &[&str],
        memory_enabled: bool,
    ) -> Message {
        let key = SystemStemKey::new(
            model_name,
            model_supports_thinking,
            tool_names,
            memory_enabled,
            self.system_override.as_deref(),
        );
        if let Some((ref cached_key, ref cached_msg)) = self.cached_system {
            if cached_key == &key {
                return cached_msg.clone();
            }
        }

        let mut content = if let Some(ref ovr) = self.system_override {
            ovr.clone()
        } else {
            let reg = handlebars::Handlebars::new();

            let mut data = serde_json::json!({
                "model_name": model_name,
                "tools": tool_names.iter().map(|n| serde_json::json!({"name": n})).collect::<Vec<_>>(),
            });

            if model_supports_thinking {
                data["thinking_available"] = serde_json::Value::Bool(true);
            }

            reg.render_template(&self.template, &data)
                .unwrap_or_else(|_| "You are a coding agent.".to_string())
        };

        // The `remember` tool instruction is stable — it depends only on
        // `memory_enabled`, which doesn't change per turn. Keep it in the
        // stem so it's cached. The volatile memory FACTS block goes in
        // build_dynamic_suffix.
        if memory_enabled {
            content.push_str("\n\nYou have a `remember` tool to store important facts about the user's preferences, project conventions, and recurring patterns. Use it when the user explicitly states a preference or when you observe a pattern worth remembering. Stored facts are available in future sessions.");
        }

        let msg = Message {
            role: Role::System,
            content,
            content_parts: None,
            thinking: None,
            tool_calls: None,
            tool_call_id: None,
            tool_name: None,
            token_count: None,
        };
        self.cached_system = Some((key, msg.clone()));
        msg
    }

    /// Build the volatile (per-turn) suffix: carryover block + memory
    /// facts block. This content changes every turn (carryover updates,
    /// memory context shifts), so it must NOT be in the cached stem.
    /// The caller appends this as a separate system message AFTER the
    /// stable stem so `CacheStemTracker` (prefix_len=1) sees a stable
    /// prefix (WO 38.9 item 6).
    ///
    /// Returns `None` when there's no volatile content (no carryover,
    /// memory disabled, or no facts selected).
    // reason: params mirror build's volatile subset; heterogeneous types.
    #[allow(clippy::too_many_arguments)]
    pub fn build_dynamic_suffix(
        &mut self,
        carryover_block: Option<&str>,
        memory_context: Option<&str>,
        memory_enabled: bool,
        memory_max_tokens: usize,
        memory_top_n: usize,
    ) -> Option<Message> {
        let mut content = String::new();

        if let Some(block) = carryover_block {
            if !block.is_empty() {
                content.push_str(block);
            }
        }

        if memory_enabled {
            let memory_block =
                self.load_memory_block(memory_context, memory_max_tokens, memory_top_n);
            if !memory_block.is_empty() {
                if !content.is_empty() {
                    content.push_str("\n\n");
                }
                content.push_str("<memory>\n");
                content.push_str(&memory_block);
                content.push_str("\n</memory>");
            }
        }

        if content.is_empty() {
            return None;
        }

        Some(Message {
            role: Role::System,
            content,
            content_parts: None,
            thinking: None,
            tool_calls: None,
            tool_call_id: None,
            tool_name: None,
            token_count: None,
        })
    }

    /// Load the memory facts block, using an mtime cache to avoid
    /// re-reading from disk every turn when the memory directory hasn't
    /// changed (WO 38.9 item 4).
    fn load_memory_block(
        &mut self,
        memory_context: Option<&str>,
        memory_max_tokens: usize,
        memory_top_n: usize,
    ) -> String {
        let store = match crate::session::memory::MemoryStore::default_store() {
            Ok(store) => store,
            Err(e) => {
                tracing::warn!(error = %e, "could not load memory store; skipping memory injection");
                return String::new();
            }
        };

        // WO 38.9 item 4: mtime-cache the memory directory. If the
        // directory mtime hasn't changed since the last read, reuse
        // the cached facts instead of re-reading + re-parsing every
        // `.md` file on every turn.
        let facts = match store.all_cached(&mut self.memory_cache) {
            Some(facts) => facts,
            None => {
                let facts = store.all();
                if let Ok(meta) = std::fs::metadata(store.root()) {
                    if let Ok(mtime) = meta.modified() {
                        self.memory_cache = Some((mtime, facts.clone()));
                    }
                }
                facts
            }
        };

        if let Some(ctx) = memory_context.filter(|s| !s.is_empty()) {
            let selected =
                store.select_for_context_from(&facts, ctx, memory_max_tokens, memory_top_n);
            for fact in &selected {
                let reason = format!("query='{}' matched memory '{}'", ctx, fact.name);
                record(MetricEvent::PlanReason {
                    decision_kind: PlanDecisionKind::MemoryRetrieve,
                    reason,
                    related_id: Some(fact.name.clone()),
                    confidence: 1.0,
                });
            }
            store.to_prompt_block_for_facts(&selected)
        } else {
            store.to_prompt_block_for_facts(&facts)
        }
    }

    /// Estimate the token size of the stable prompt-cache stem.
    ///
    /// The stem is the system prompt *without* the dynamic carryover and
    /// memory blocks, because those can change across turns. Providers
    /// cache the exact bytes sent; a changing carryover block would
    /// invalidate the cache. The returned value is an estimate used for
    /// status-bar telemetry and cache-hit verification.
    pub fn estimate_stem_tokens(
        &self,
        model_name: &str,
        model_supports_thinking: bool,
        tool_names: &[&str],
    ) -> usize {
        let stem = self.build_stem(model_name, model_supports_thinking);
        let tool_json = serde_json::to_string(
            &tool_names
                .iter()
                .map(|n| serde_json::json!({"name": n}))
                .collect::<Vec<_>>(),
        )
        .unwrap_or_default();
        count_tokens(&format!("{stem}{tool_json}"))
    }

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

    pub fn cache_hit_probability(&self, model_name: &str, model_supports_thinking: bool) -> f64 {
        let stem = self.build_stem(model_name, model_supports_thinking);
        let stem_tokens_est = count_tokens(&stem);

        if stem_tokens_est < 1024 {
            return 0.3; // Small stem → tools section is proportionally large → cache miss likely
        }

        if stem_tokens_est > 2048 {
            0.95
        } else {
            0.3 + (stem_tokens_est as f64 - 1024.0) / (2048.0 - 1024.0) * 0.65
        }
    }

    /// Build the system message with top-N frequently-accessed file
    /// bodies injected into the cached stem.
    ///
    /// This is the WO 17.5 shared cached context mechanism: the
    /// frequently-accessed files (from the read gate's access stats)
    /// are minified and appended to the system prompt as a stable block.
    /// Because they're part of the shared stem, Anthropic's prompt cache
    /// covers them — the model stops re-reading the same files every turn.
    ///
    /// `top_files` is a list of `(path, content)` pairs, sorted by
    /// access count descending. The content strings are already
    /// minified by the caller (using WO 17.4's minifier).
    #[allow(clippy::too_many_arguments)]
    pub fn build_with_top_files(
        &mut self,
        model_name: &str,
        model_supports_thinking: bool,
        tool_names: &[&str],
        carryover_block: Option<&str>,
        memory_context: Option<&str>,
        memory_enabled: bool,
        memory_max_tokens: usize,
        memory_top_n: usize,
        top_files: &[(PathBuf, String)],
    ) -> Message {
        let mut msg = self.build(
            model_name,
            model_supports_thinking,
            tool_names,
            carryover_block,
            memory_context,
            memory_enabled,
            memory_max_tokens,
            memory_top_n,
        );

        // Inject top-N file bodies into the system message. These go
        // after the existing content so they're part of the stem that
        // Anthropic caches. The content is already minified.
        if !top_files.is_empty() {
            let mut file_block = String::from("\n\nFrequently accessed files:\n");
            for (path, body) in top_files {
                file_block.push_str(&format!("--- {} ---\n{}\n", path.display(), body));
            }
            msg.content.push_str(&file_block);

            // Invalidate cached system since the content changed.
            self.cached_system = None;
        }

        msg
    }

    pub fn build_messages(
        &mut self,
        system: Message,
        history: &[Message],
        model_max_tokens: usize,
        tool_results: &[Message],
    ) -> Vec<Message> {
        self.build_messages_with_compaction(
            system,
            history,
            model_max_tokens,
            tool_results,
            false,
            0.5,
        )
    }

    /// Build the full message list with configurable compaction.
    ///
    /// When `use_llm` is true, microcompaction tries the LLM summarizer when
    /// the heuristic drops more than `drop_threshold` fraction of content.
    /// When `use_llm` is false (the default), the heuristic summary is always
    /// used. This is the WO 17.5 wiring point for `compaction.use_llm`.
    pub fn build_messages_with_compaction(
        &mut self,
        system: Message,
        history: &[Message],
        model_max_tokens: usize,
        tool_results: &[Message],
        use_llm: bool,
        drop_threshold: f64,
    ) -> Vec<Message> {
        let mut messages = Self::assemble_messages(system, history, tool_results);

        // Image attach — when the most-recent user turn follows a
        // `read_image` tool result, splice the image content part
        // onto the user message so the model actually sees the
        // attachment inline (OpenAI vision / Ollama `images`).
        Self::attach_pending_image(&mut messages);

        Self::truncate_tool_results(&mut messages);

        Self::dedup_adjacent_tool_results(&mut messages);

        // Inject relevant symbols from the repo-graph context index.
        // WO 38.9 item 6: insert as a SEPARATE system message after the
        // stem (messages[0]) so the stable prefix hash (prefix_len=1)
        // is not invalidated by per-query symbol changes.
        if let Some(ref idx) = self.context_index {
            let query = history
                .iter()
                .rev()
                .find(|m| m.role == Role::User)
                .map(|m| m.content.as_str())
                .unwrap_or("");
            if !query.is_empty() {
                let relevant = idx.retrieve_hybrid(query, 10);
                if !relevant.is_empty() && !messages.is_empty() {
                    let mut content = String::from("<relevant_symbols>\n");
                    for result in &relevant {
                        let sym = &result.symbol;
                        content.push_str(&format!(
                            "{} {} at {}:{}",
                            match sym.kind {
                                kf_context_index::SymbolKind::Function => "fn",
                                kf_context_index::SymbolKind::Struct => "struct",
                                kf_context_index::SymbolKind::Enum => "enum",
                                kf_context_index::SymbolKind::Impl => "impl",
                                kf_context_index::SymbolKind::Module => "mod",
                                kf_context_index::SymbolKind::Use => "use",
                                kf_context_index::SymbolKind::Class => "class",
                                kf_context_index::SymbolKind::Interface => "interface",
                                kf_context_index::SymbolKind::TypeAlias => "type",
                            },
                            sym.name,
                            sym.file.display(),
                            sym.line,
                        ));
                        if !result.imported_by.is_empty() {
                            content.push_str(" (imported by: ");
                            // ponytail: cap fan-out at 10 — prompt-size
                            // ceiling; any index pathology must not be
                            // able to blow the system prompt to MBs.
                            for (i, imp) in result.imported_by.iter().take(10).enumerate() {
                                if i > 0 {
                                    content.push_str(", ");
                                }
                                content.push_str(&imp.display().to_string());
                            }
                            if result.imported_by.len() > 10 {
                                content.push_str(&format!(
                                    ", +{} more",
                                    result.imported_by.len() - 10
                                ));
                            }
                            content.push(')');
                        }
                        if !result.called_by.is_empty() {
                            content.push_str(" (called by: ");
                            for (i, cs) in result.called_by.iter().take(10).enumerate() {
                                if i > 0 {
                                    content.push_str(", ");
                                }
                                content.push_str(&format!(
                                    "{}() at {}:{}",
                                    cs.caller_name,
                                    cs.caller_file.display(),
                                    cs.line
                                ));
                            }
                            if result.called_by.len() > 10 {
                                content
                                    .push_str(&format!(", +{} more", result.called_by.len() - 10));
                            }
                            content.push(')');
                        }
                        if let Some(ref doc) = sym.doc {
                            content.push_str(&format!(" // {doc}"));
                        }
                        content.push('\n');
                    }
                    content.push_str("</relevant_symbols>");
                    let sym_msg = Message {
                        role: Role::System,
                        content,
                        content_parts: None,
                        thinking: None,
                        tool_calls: None,
                        tool_call_id: None,
                        tool_name: None,
                        token_count: None,
                    };
                    // Insert after the stem (index 0), before the history.
                    messages.insert(1, sym_msg);
                }
            }
        }

        let budget = model_max_tokens.saturating_sub(model_max_tokens / 10);
        if Self::estimated_tokens(&messages) <= budget {
            return messages;
        }

        let mut adjusted = messages.clone();

        // Microcompaction: before more aggressive truncation, summarize
        // the oldest non-anchor messages into a single compact system
        // message while preserving the last few turns verbatim. This is
        // P3-6's middle-compression strategy — distinct from the `/compact`
        // log rewrite because it happens on the fly at request-build time.
        if let Some(result) = microcompaction::maybe_microcompact(
            &messages,
            budget,
            DEFAULT_MICROCOMPACT_KEEP_TAIL,
            use_llm,
            drop_threshold,
        ) {
            if result.tokens_after <= budget {
                return result.messages;
            }
            // Even if not under budget, continue with the compacted form
            // so the later fallback truncation has less to chew through.
            adjusted = result.messages;
        }

        if Self::minify_old_messages(&messages, &mut adjusted)
            && Self::estimated_tokens(&adjusted) <= budget
        {
            return adjusted;
        }

        if Self::stub_old_tool_results(&mut adjusted) && Self::estimated_tokens(&adjusted) <= budget
        {
            return adjusted;
        }

        Self::truncate_to_budget(&adjusted, budget)
    }

    /// Splice the image from a just-preceding `read_image` tool
    /// result onto the next user message, so the model sees the
    /// attachment in the right slot.
    ///
    /// Pattern: the conversation has
    /// `[…, Role::Tool{tool_name=read_image, content_parts=[Image{…}]}, Role::User{…}]`
    /// and we want to mutate the `User` message in place so its
    /// `content_parts` includes the image (prepended before any
    /// existing text parts). This is the "user attached a screenshot
    /// and is now asking about it" UX.
    ///
    /// Rules:
    /// 1. The most-recent user message must have empty or no
    ///    `content_parts` (don't overwrite an already-attached image).
    /// 2. The tool message immediately preceding it must be from
    ///    `read_image` with a non-empty `content_parts` list.
    /// 3. The splice is in-place on the `messages` slice; no new
    ///    messages are inserted. The conversation log itself is
    ///    *not* mutated — the image is attached on the way out to
    ///    the model, not persisted in the on-disk log. (Replaying
    ///    the log through `assemble_messages` again would re-run
    ///    the splice, so the persistence story is fine.)
    fn attach_pending_image(messages: &mut [Message]) {
        if messages.len() < 2 {
            return;
        }
        // Find the most-recent user message and the message before it.
        let last_idx = messages.len() - 1;
        if messages[last_idx].role != Role::User {
            return; // no user turn at the tail — nothing to attach to
        }
        let tool_idx = last_idx - 1;
        let tool_msg = &messages[tool_idx];
        if tool_msg.role != Role::Tool {
            return;
        }
        if tool_msg.tool_name.as_deref() != Some("read_image") {
            return;
        }
        let image_part = match tool_msg
            .content_parts
            .as_ref()
            .and_then(|parts| parts.first())
        {
            Some(part @ crate::shared::ContentPart::Image { .. }) => part.clone(),
            _ => return, // read_image emitted no image — bail
        };

        // Splice the image onto the user message. Prepend (so it
        // visually leads the message), or replace the existing
        // content_parts if the model already sent some.
        let user_msg = &mut messages[last_idx];
        let mut new_parts: Vec<crate::shared::ContentPart> = Vec::with_capacity(2);
        new_parts.push(image_part);
        match user_msg.content_parts.take() {
            Some(existing) => new_parts.extend(existing),
            None => {
                // No parts — synthesise a Text part from the
                // existing `content` so the user message text is
                // still in the parts list, alongside the image.
                if !user_msg.content.is_empty() {
                    new_parts.push(crate::shared::ContentPart::Text {
                        text: user_msg.content.clone(),
                    });
                }
            }
        }
        user_msg.content_parts = Some(new_parts);
    }

    fn assemble_messages(
        system: Message,
        history: &[Message],
        tool_results: &[Message],
    ) -> Vec<Message> {
        let mut messages = Vec::with_capacity(1 + history.len() + tool_results.len());
        messages.push(system);
        for msg in history {
            messages.push(msg.clone());
        }
        for msg in tool_results {
            messages.push(msg.clone());
        }
        messages
    }

    fn truncate_tool_results(messages: &mut [Message]) {
        const TOOL_RESULT_DEFAULT_HEAD: usize = 20_000;
        const TOOL_RESULT_DEFAULT_TAIL: usize = 8_000;

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

            let (head_keep, tail_keep) = match msg.tool_name.as_deref() {
                Some(name) => per_tool_caps
                    .get(name)
                    .copied()
                    .unwrap_or((TOOL_RESULT_DEFAULT_HEAD, TOOL_RESULT_DEFAULT_TAIL)),
                None => (TOOL_RESULT_DEFAULT_HEAD, TOOL_RESULT_DEFAULT_TAIL),
            };
            let hard_cap = head_keep + tail_keep;
            if msg.content.chars().count() > hard_cap {
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
                    "{head}\n\n[…truncated {removed_chars} chars of tool output…]\n\n{tail}"
                );
                msg.token_count = None;
            }
        }
    }

    fn dedup_adjacent_tool_results(messages: &mut [Message]) {
        const TOOL_RESULT_DEDUP_MARKER: &str =
            "[duplicate tool result omitted — see previous identical result]";
        const TOOL_RESULT_UNCHANGED_MARKER: &str =
            "[unchanged from previous identical tool result]";
        type DedupKey = (String, Option<String>, Option<String>);
        let mut prev_tool: Option<(DedupKey, usize)> = None;
        for msg in messages.iter_mut() {
            if !matches!(msg.role, Role::Tool) {
                prev_tool = None;
                continue;
            }
            let key = (
                msg.content.clone(),
                msg.tool_name.clone(),
                msg.tool_call_id.clone(),
            );
            if let Some((prev, seen)) = prev_tool.as_ref() {
                if prev == &key {
                    // Third and subsequent identical tool results in a row
                    // are collapsed to an "unchanged" marker. The first is
                    // kept full, the second is deduplicated to the existing
                    // marker, and from the third on we emit a compact
                    // unchanged note.
                    msg.content = if *seen >= 2 {
                        TOOL_RESULT_UNCHANGED_MARKER.to_string()
                    } else {
                        TOOL_RESULT_DEDUP_MARKER.to_string()
                    };
                    msg.token_count = None;
                    prev_tool = Some((key, *seen + 1));
                    continue;
                }
            }
            prev_tool = Some((key, 1));
        }
    }

    fn estimated_tokens(messages: &[Message]) -> usize {
        messages.iter().map(Self::estimate_message_tokens).sum()
    }

    fn estimate_message_tokens(m: &Message) -> usize {
        crate::session::prompt::estimate_message_tokens(m)
    }

    fn minify_old_messages(messages: &[Message], adjusted: &mut [Message]) -> bool {
        let mut minified_any = false;
        for (i, msg) in messages.iter().enumerate() {
            if i == 0 {
                continue; // keep system prompt as-is
            }
            if matches!(msg.role, Role::Tool) {
                continue; // keep tool results as-is
            }

            let est = Self::estimate_message_tokens(msg);
            if est < 10 {
                continue; // too short to bother
            }

            // The path is synthetic; the extension drives language-aware
            // minification. Using `.txt` for everything hit the catch-all arm
            // and made this step a no-op, so pick an extension from any
            // markdown code-fence language tag (or a Rust fallback for
            // un-fenced source blocks).
            let ext = synthetic_extension_for(&msg.content);
            let path = PathBuf::from(format!("message-{i}.{ext}"));
            let minified = crate::shared::minify::minify_source_safe(&path, &msg.content);
            if minified.len() < msg.content.len() {
                let savings = msg.content.len() - minified.len();
                if savings > 20 {
                    adjusted[i].content = minified;
                    adjusted[i].token_count = None;
                    minified_any = true;
                }
            }
        }
        minified_any
    }

    fn stub_old_tool_results(messages: &mut [Message]) -> bool {
        const TOOL_RESULT_KEEP_TAIL: usize = 2;
        const TOOL_RESULT_STUB: &str =
            "[previous tool result omitted to save budget — see TUI history]";

        let tool_indices: Vec<usize> = messages
            .iter()
            .enumerate()
            .filter(|(_, m)| matches!(m.role, Role::Tool))
            .map(|(i, _)| i)
            .collect();
        let preserve_from = tool_indices.len().saturating_sub(TOOL_RESULT_KEEP_TAIL);

        let mut stubbed_any = false;
        for &i in tool_indices.iter().take(preserve_from) {
            if messages[i].content != TOOL_RESULT_STUB {
                messages[i].content = TOOL_RESULT_STUB.to_string();
                messages[i].token_count = None;
                stubbed_any = true;
            }
        }
        stubbed_any
    }

    fn truncate_to_budget(messages: &[Message], budget: usize) -> Vec<Message> {
        if messages.is_empty() {
            return messages.to_vec();
        }
        let keep_count = (budget * 4) / 20;
        let history_to_keep = std::cmp::min(keep_count, messages.len() - 1);

        let mut truncated = vec![messages[0].clone()]; // keep system (cache stem)

        let start = messages.len().saturating_sub(history_to_keep);
        for msg in &messages[start..] {
            truncated.push(msg.clone());
        }

        if truncated.len() < 2 {
            return messages.to_vec();
        }
        truncated
    }
}

impl Default for PromptBuilder {
    fn default() -> Self {
        Self::new()
    }
}

/// Pick a synthetic file extension for [`PromptBuilder::minify_old_messages`]
/// so the language-aware minifier actually runs on code blocks instead of
/// hitting the `.txt` catch-all.
fn synthetic_extension_for(content: &str) -> &'static str {
    for (idx, _) in content.match_indices("```") {
        let after = &content[idx + 3..];
        let tag = after.lines().next().unwrap_or("").trim().to_lowercase();
        let ext = match tag.as_str() {
            "rs" | "rust" => "rs",
            "py" | "python" => "py",
            "js" | "javascript" => "js",
            "ts" | "typescript" => "ts",
            "jsx" => "jsx",
            "tsx" => "tsx",
            "go" => "go",
            "c" => "c",
            "cpp" | "c++" => "cpp",
            "java" => "java",
            "rb" | "ruby" => "rb",
            "sh" | "bash" | "zsh" => "sh",
            "md" | "markdown" => "md",
            "json" => "json",
            "yaml" | "yml" => "yaml",
            "toml" => "toml",
            _ => continue,
        };
        return ext;
    }
    // No recognized fence; if the block looks like Rust source, treat it as such.
    if content.contains("fn ") && content.contains('{') {
        return "rs";
    }
    "txt"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_dedup_keys_on_tool_name_and_call_id() {
        let mk = |tool_name: &str, tool_call_id: &str, content: &str| Message {
            role: Role::Tool,
            content: content.to_string(),
            content_parts: None,
            thinking: None,
            tool_calls: None,
            tool_call_id: Some(tool_call_id.to_string()),
            tool_name: Some(tool_name.to_string()),
            token_count: None,
        };
        // Same tool, same content, different call IDs: must NOT be deduplicated.
        let mut msgs = vec![
            mk("read_file", "call_1", "same"),
            mk("read_file", "call_2", "same"),
        ];
        PromptBuilder::dedup_adjacent_tool_results(&mut msgs);
        assert_eq!(
            msgs[1].content, "same",
            "different call ids must survive dedup"
        );

        // Different tools, same content: must NOT be deduplicated.
        let mut msgs = vec![
            mk("read_file", "call_1", "same"),
            mk("grep", "call_2", "same"),
        ];
        PromptBuilder::dedup_adjacent_tool_results(&mut msgs);
        assert_eq!(
            msgs[1].content, "same",
            "different tools must survive dedup"
        );

        // Same tool, same call id, same content: still deduplicated.
        let mut msgs = vec![
            mk("read_file", "call_1", "same"),
            mk("read_file", "call_1", "same"),
        ];
        PromptBuilder::dedup_adjacent_tool_results(&mut msgs);
        assert_eq!(
            msgs[1].content, "[duplicate tool result omitted — see previous identical result]",
            "identical tool+call_id+content still deduplicated"
        );
    }

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
    }

    #[test]
    fn test_cache_hit_probability_returns_some() {
        let builder = PromptBuilder::new();
        let prob = builder.cache_hit_probability("glm-5.1:cloud", true);
        assert!((0.0..=1.0).contains(&prob));
    }

    #[test]
    fn test_truncate_to_budget_empty_context_no_panic() {
        let empty: Vec<Message> = Vec::new();
        assert!(PromptBuilder::truncate_to_budget(&empty, 0).is_empty());

        let single = vec![Message {
            role: Role::System,
            content: "sys".to_string(),
            content_parts: None,
            thinking: None,
            tool_calls: None,
            tool_call_id: None,
            tool_name: None,
            token_count: None,
        }];
        let out = PromptBuilder::truncate_to_budget(&single, 0);
        assert_eq!(out.len(), 1, "single system message survives zero budget");
    }

    #[test]
    fn test_build_includes_tools() {
        let mut builder = PromptBuilder::new();
        let msg = builder.build(
            "test-model",
            false,
            &["read_file", "bash"],
            None,
            None,
            false,
            0,
            0,
        );
        assert_eq!(msg.role, Role::System);
        assert!(!msg.content.is_empty());
    }

    /// WO 38.9 item 6: the stable stem must NOT include carryover or
    /// memory facts — they go in build_dynamic_suffix. Two builds with
    /// different carryover/memory_context must produce the same stem.
    #[test]
    fn test_stem_stable_across_volatile_changes() {
        let mut builder = PromptBuilder::new();
        let stem_a = builder.build(
            "test-model",
            false,
            &["bash"],
            Some("carryover v1"),
            Some("ctx v1"),
            true,
            500,
            10,
        );
        let stem_b = builder.build(
            "test-model",
            false,
            &["bash"],
            Some("carryover v2"),
            Some("ctx v2"),
            true,
            500,
            10,
        );
        assert_eq!(
            stem_a.content, stem_b.content,
            "stem must be identical despite different carryover/memory_context"
        );
    }

    /// WO 38.9 item 6: build_dynamic_suffix returns the volatile block
    /// (carryover + memory). When carryover is present, it's in the
    /// suffix, not the stem.
    #[test]
    fn test_dynamic_suffix_contains_carryover() {
        let mut builder = PromptBuilder::new();
        let suffix = builder.build_dynamic_suffix(Some("carryover text"), None, false, 0, 0);
        assert!(suffix.is_some(), "suffix should exist with carryover");
        assert!(
            suffix.as_ref().unwrap().content.contains("carryover text"),
            "suffix should contain the carryover block"
        );
    }

    /// WO 38.9 item 6: when there's no carryover and memory is disabled,
    /// build_dynamic_suffix returns None (no volatile content).
    #[test]
    fn test_dynamic_suffix_none_when_no_volatile_content() {
        let mut builder = PromptBuilder::new();
        let suffix = builder.build_dynamic_suffix(None, None, false, 0, 0);
        assert!(suffix.is_none(), "no volatile content → no suffix");
    }

    /// WO 38.9 item 6: the stem must NOT contain carryover or memory
    /// blocks, even when they're passed to build().
    #[test]
    fn test_stem_excludes_carryover_and_memory() {
        let mut builder = PromptBuilder::new();
        let stem = builder.build(
            "test-model",
            false,
            &["bash"],
            Some("CARRYOVER_MARKER"),
            None,
            false,
            0,
            0,
        );
        assert!(
            !stem.content.contains("CARRYOVER_MARKER"),
            "stem must not contain carryover content"
        );
    }

    #[test]
    fn test_build_prompt_requires_validation_and_no_artifact_injection() {
        let mut builder = PromptBuilder::new();
        let msg = builder.build("test-model", false, &[], None, None, false, 0, 0);
        assert!(
            msg.content.contains("run the project's build/test command"),
            "system prompt should instruct the agent to validate edits"
        );
        assert!(
            msg.content.contains("tool-output artifact"),
            "system prompt should forbid tool-output artifact directories"
        );
        assert!(
            msg.content.contains(".gitignore"),
            "system prompt should forbid .gitignore edits"
        );
    }

    #[test]
    fn test_remember_instruction_injected_when_memory_enabled() {
        let mut builder = PromptBuilder::new();
        let msg = builder.build("test-model", false, &[], None, None, true, 500, 10);
        assert!(
            msg.content.contains("remember"),
            "system prompt should include remember tool instruction when memory is enabled"
        );
    }

    #[test]
    fn test_remember_instruction_absent_when_memory_disabled() {
        let mut builder = PromptBuilder::new();
        let msg = builder.build("test-model", false, &[], None, None, false, 0, 0);
        assert!(
            !msg.content.contains("remember"),
            "system prompt should not include remember tool instruction when memory is disabled"
        );
    }

    #[test]
    fn test_minify_old_messages_reduces_code_tokens() {
        // Build a budget-busting history that contains a Rust code block.
        // The `.txt` catch-all used to skip minification; the fix picks the
        // extension from the code-fence language tag so comments/blank lines
        // are stripped and the token count drops.
        let mut builder = PromptBuilder::new();
        builder.set_system_override(Some("sys".to_string()));

        let block = "```rs\nfn main() {\n    // this comment adds tokens\n    let x = 1;\n\n    println!(\"hello\");\n}\n```\n";
        let long_code = block.repeat(10);
        let original_len = long_code.len();

        let history = vec![Message {
            role: Role::Assistant,
            content: long_code,
            content_parts: None,
            thinking: None,
            tool_calls: None,
            tool_call_id: None,
            tool_name: None,
            token_count: None,
        }];

        let messages = builder.build_messages(
            Message {
                role: Role::System,
                content: "sys".to_string(),
                content_parts: None,
                thinking: None,
                tool_calls: None,
                tool_call_id: None,
                tool_name: None,
                token_count: None,
            },
            &history,
            50,
            &[],
        );

        let assistant_content = messages
            .iter()
            .find(|m| matches!(m.role, Role::Assistant))
            .map(|m| m.content.clone())
            .unwrap_or_default();

        assert!(
            assistant_content.len() < original_len,
            "minify_old_messages should reduce the assistant code block"
        );
        assert!(
            assistant_content.contains("fn main"),
            "minified code should still contain executable content"
        );
        assert!(
            !assistant_content.contains("this comment adds tokens"),
            "minified code should strip comments"
        );
    }

    #[test]
    fn test_build_supports_thinking() {
        let mut builder = PromptBuilder::new();
        let msg = builder.build("test-model", true, &[], None, None, false, 0, 0);
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
                content: format!("Message {i}"),
                ..Default::default()
            });
        }
        let result = builder.build_messages(system.clone(), &history, 50, &[]);

        assert!(result.len() < 22);

        assert_eq!(result[0].content, "S");
    }

    #[test]
    fn test_build_stem_no_tools() {
        let builder = PromptBuilder::new();
        let stem = builder.build_stem("test-model", false);
        assert!(!stem.is_empty());
    }

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
        let capped = result
            .iter()
            .find(|m| matches!(m.role, Role::Tool))
            .unwrap();
        assert!(
            capped.content.len() < 32_000,
            "tool output should be capped below 32k chars, got {}",
            capped.content.len()
        );
        assert!(
            capped.content.contains("truncated"),
            "should contain a truncation marker"
        );
        assert!(capped.content.starts_with('x'), "head should be preserved");
        assert!(capped.content.ends_with('x'), "tail should be preserved");
    }

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
        let kept = result
            .iter()
            .find(|m| matches!(m.role, Role::Tool))
            .unwrap();
        assert_eq!(kept.content, small_output);
    }

    #[test]
    fn test_build_messages_tool_output_cap_handles_utf8() {
        let mut builder = PromptBuilder::new();
        let system = Message {
            role: Role::System,
            content: "S".into(),
            ..Default::default()
        };

        let big_utf8: String = "🦀".repeat(50_000);
        let tool_results = vec![Message {
            role: Role::Tool,
            content: big_utf8,
            ..Default::default()
        }];
        let result = builder.build_messages(system, &[], 100_000, &tool_results);
        let capped = result
            .iter()
            .find(|m| matches!(m.role, Role::Tool))
            .unwrap();
        assert!(capped.content.chars().count() < 32_000);
        assert!(capped.content.contains("🦀"));
    }

    // WO 27.2-R2: un-ignored after tightening the budget so the stub
    // branch actually fires. Today's build_messages runs microcompaction
    // first; with the original 3_000 budget, microcompaction's result
    // (5 trailing tool msgs × ~500 tokens + summary) snuck under budget
    // and the stub branch was unreachable. Lower budget forces the
    // stub branch to handle the overflow.
    #[test]
    fn test_build_messages_stubs_old_tool_results_when_over_budget() {
        let mut builder = PromptBuilder::new();
        let system = Message {
            role: Role::System,
            content: "S".into(),
            ..Default::default()
        };

        let mut history = Vec::new();
        for i in 0..3 {
            history.push(Message {
                role: Role::User,
                content: format!("user message {i}"),
                ..Default::default()
            });
            history.push(Message {
                role: Role::Assistant,
                content: format!("assistant message {i}"),
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

        let result = builder.build_messages(system, &history, 2_000, &tool_results);

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
        assert!(
            stubbed > 0,
            "expected older tool results to be stubbed, got {} stubbed / {} kept",
            stubbed,
            tool_msgs.len()
        );
        assert!(
            kept <= 2,
            "at most the last 2 tool results should be kept intact, got {kept} kept"
        );
        assert!(
            stubbed + kept == tool_msgs.len(),
            "every tool message is either stubbed or kept"
        );
    }

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
                content: format!("small tool result {i}"),
                ..Default::default()
            })
            .collect();
        let result = builder.build_messages(system, &history, 8_192, &tool_results);
        let stubbed = result
            .iter()
            .filter(|m| matches!(m.role, Role::Tool) && m.content.contains("omitted"))
            .count();
        assert_eq!(
            stubbed, 0,
            "no tool results should be stubbed when under budget"
        );
    }

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
                tool_name: Some("bash".into()),
                ..Default::default()
            },
            Message {
                role: Role::Tool,
                content: "Cargo.lock already exists at /tmp/foo.lock".into(),
                tool_call_id: Some("call_1".into()),
                tool_name: Some("bash".into()),
                ..Default::default()
            },
        ];
        let result = builder.build_messages(system, &[], 100_000, &tool_results);
        let tool_msgs: Vec<&Message> = result
            .iter()
            .filter(|m| matches!(m.role, Role::Tool))
            .collect();
        assert_eq!(tool_msgs.len(), 2);

        assert_eq!(
            tool_msgs[0].content,
            "Cargo.lock already exists at /tmp/foo.lock"
        );

        assert!(tool_msgs[1].content.contains("duplicate tool result"));
        assert!(!tool_msgs[1].content.contains("Cargo.lock"));
    }

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

    #[test]
    fn test_build_messages_dedup_resets_on_non_tool_message() {
        let mut builder = PromptBuilder::new();
        let system = Message {
            role: Role::System,
            content: "S".into(),
            ..Default::default()
        };

        let custom_history = vec![
            Message {
                role: Role::Tool,
                content: "identical".into(),
                tool_call_id: Some("c1".into()),
                tool_name: Some("bash".into()),
                ..Default::default()
            },
            Message {
                role: Role::Tool,
                content: "identical".into(),
                tool_call_id: Some("c1".into()),
                tool_name: Some("bash".into()),
                ..Default::default()
            },
            Message {
                role: Role::User,
                content: "intervening turn".into(),
                ..Default::default()
            },
        ];

        let tool_results = vec![
            Message {
                role: Role::Tool,
                content: "identical".into(),
                tool_call_id: Some("c3".into()),
                tool_name: Some("bash".into()),
                ..Default::default()
            },
            Message {
                role: Role::Tool,
                content: "identical".into(),
                tool_call_id: Some("c3".into()),
                tool_name: Some("bash".into()),
                ..Default::default()
            },
        ];
        let result = builder.build_messages(system, &custom_history, 100_000, &tool_results);
        let tool_msgs: Vec<&Message> = result
            .iter()
            .filter(|m| matches!(m.role, Role::Tool))
            .collect();

        assert_eq!(tool_msgs.len(), 4);
        assert_eq!(tool_msgs[0].content, "identical");
        assert!(tool_msgs[1].content.contains("duplicate"));
        assert_eq!(tool_msgs[2].content, "identical");
        assert!(tool_msgs[3].content.contains("duplicate"));

        assert!(result.iter().any(|m| m.content == "intervening turn"));
    }

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
        assert!(
            tool_msgs[1].content.contains("duplicate"),
            "entry 1 should be deduped"
        );
        assert!(
            tool_msgs[2].content.contains("unchanged"),
            "entry 2 should be unchanged marker"
        );
        assert!(
            tool_msgs[3].content.contains("unchanged"),
            "entry 3 should be unchanged marker"
        );
    }

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
        let capped = result
            .iter()
            .find(|m| matches!(m.role, Role::Tool))
            .unwrap();

        assert!(
            capped.content.chars().count() < 61_000,
            "bash tool output should be capped below 61k chars (50k+10k cap + marker), got {}",
            capped.content.chars().count()
        );
        assert!(
            capped.content.contains("truncated"),
            "should contain a truncation marker"
        );
        assert!(capped.content.starts_with('B'), "head should be preserved");
        assert!(capped.content.ends_with('B'), "tail should be preserved");
    }

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
        let capped = result
            .iter()
            .find(|m| matches!(m.role, Role::Tool))
            .unwrap();

        assert!(
            capped.content.chars().count() < 16_000,
            "grep tool output should be capped below 16k chars (10k+5k cap + marker), got {}",
            capped.content.chars().count()
        );
        assert!(
            capped.content.contains("truncated"),
            "should contain a truncation marker"
        );
    }

    #[test]
    fn test_build_messages_per_tool_cap_falls_back_to_default() {
        let mut builder = PromptBuilder::new();
        let system = Message {
            role: Role::System,
            content: "S".into(),
            ..Default::default()
        };

        let big_output = "X".repeat(50_000);
        let tool_results = vec![Message {
            role: Role::Tool,
            content: big_output,
            tool_name: None,
            ..Default::default()
        }];
        let result = builder.build_messages(system, &[], 100_000, &tool_results);
        let capped = result
            .iter()
            .find(|m| matches!(m.role, Role::Tool))
            .unwrap();

        assert!(
            capped.content.chars().count() < 29_000,
            "fallback tool output should be capped below 29k chars (20k+8k cap + marker), got {}",
            capped.content.chars().count()
        );
        assert!(capped.content.contains("truncated"));
    }

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
        let capped = result
            .iter()
            .find(|m| matches!(m.role, Role::Tool))
            .unwrap();

        assert!(
            capped.content.chars().count() < 29_000,
            "unknown-tool output should fall back to default 28k cap, got {}",
            capped.content.chars().count()
        );
        assert!(capped.content.contains("truncated"));
    }

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

    #[test]
    fn test_estimate_tokens_counts_tool_calls() {
        let mut builder = PromptBuilder::new();
        let system = Message {
            role: Role::System,
            content: "S".into(),
            ..Default::default()
        };

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

        let result = builder.build_messages(system, &history, 1_000, &[]);

        assert!(
            result.len() <= 2,
            "expected the over-budget path to engage (system + maybe tail), got {} messages",
            result.len()
        );
    }

    #[test]
    fn test_estimate_tokens_ignores_none_tool_calls() {
        let mut builder = PromptBuilder::new();
        let system = Message {
            role: Role::System,
            content: "S".into(),
            ..Default::default()
        };

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

        let r_none = builder.build_messages(system.clone(), &[m_none], 1_000, &[]);
        let r_empty = builder.build_messages(system, &[m_empty], 1_000, &[]);

        assert_eq!(r_none.len(), 2);
        assert_eq!(r_empty.len(), 2);
        assert_eq!(r_none[1].content, "short");
        assert_eq!(r_empty[1].content, "short");
    }

    #[test]
    fn test_estimate_tokens_reveals_hidden_tool_call_budget_pressure() {
        let mut builder = PromptBuilder::new();
        let system = Message {
            role: Role::System,
            content: "S".into(),
            ..Default::default()
        };

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

        let result = builder.build_messages(system, &history, 14_000, &[]);

        assert!(!result.is_empty());

        assert_eq!(result[0].role, Role::System);
    }

    #[test]
    fn synthetic_extension_for_recognizes_rust_fences() {
        assert_eq!(synthetic_extension_for("```rust\nfn main() {}```"), "rs");
        assert_eq!(synthetic_extension_for("```rs\nfn main() {}```"), "rs");
    }

    #[test]
    fn synthetic_extension_for_recognizes_python_fences() {
        assert_eq!(synthetic_extension_for("```python\nprint('hi')```"), "py");
        assert_eq!(synthetic_extension_for("```py\nprint('hi')```"), "py");
    }

    #[test]
    fn synthetic_extension_for_recognizes_javascript_and_typescript() {
        assert_eq!(synthetic_extension_for("```javascript\nx```"), "js");
        assert_eq!(synthetic_extension_for("```js\nx```"), "js");
        assert_eq!(synthetic_extension_for("```typescript\nx```"), "ts");
        assert_eq!(synthetic_extension_for("```ts\nx```"), "ts");
    }

    #[test]
    fn synthetic_extension_for_recognizes_jsx_and_tsx() {
        assert_eq!(synthetic_extension_for("```jsx\nx```"), "jsx");
        assert_eq!(synthetic_extension_for("```tsx\nx```"), "tsx");
    }

    #[test]
    fn synthetic_extension_for_recognizes_other_languages() {
        assert_eq!(synthetic_extension_for("```go\nx```"), "go");
        assert_eq!(synthetic_extension_for("```c\nx```"), "c");
        assert_eq!(synthetic_extension_for("```cpp\nx```"), "cpp");
        assert_eq!(synthetic_extension_for("```c++\nx```"), "cpp");
        assert_eq!(synthetic_extension_for("```java\nx```"), "java");
        assert_eq!(synthetic_extension_for("```rb\nx```"), "rb");
        assert_eq!(synthetic_extension_for("```ruby\nx```"), "rb");
        assert_eq!(synthetic_extension_for("```sh\nx```"), "sh");
        assert_eq!(synthetic_extension_for("```bash\nx```"), "sh");
        assert_eq!(synthetic_extension_for("```zsh\nx```"), "sh");
        assert_eq!(synthetic_extension_for("```md\nx```"), "md");
        assert_eq!(synthetic_extension_for("```markdown\nx```"), "md");
        assert_eq!(synthetic_extension_for("```json\nx```"), "json");
        assert_eq!(synthetic_extension_for("```yaml\nx```"), "yaml");
        assert_eq!(synthetic_extension_for("```yml\nx```"), "yaml");
        assert_eq!(synthetic_extension_for("```toml\nx```"), "toml");
    }

    #[test]
    fn synthetic_extension_for_picks_first_recognized_fence() {
        let content = "intro\n```python\nx```\n```rust\ny```";
        assert_eq!(synthetic_extension_for(content), "py");
    }

    #[test]
    fn synthetic_extension_for_skips_unrecognized_fence_tags() {
        let content = "```text\nplain```\n```rust\nfn x() {}```";
        assert_eq!(synthetic_extension_for(content), "rs");
    }

    #[test]
    fn synthetic_extension_for_returns_txt_for_unrecognized_block() {
        assert_eq!(synthetic_extension_for("```text\nplain```"), "txt");
        assert_eq!(
            synthetic_extension_for("plain prose without any fence"),
            "txt"
        );
    }

    #[test]
    fn synthetic_extension_for_infers_rust_from_fn_and_brace_without_fence() {
        assert_eq!(synthetic_extension_for("fn main() { println!() }"), "rs");
    }

    #[test]
    fn synthetic_extension_for_returns_txt_when_braces_present_without_fn() {
        assert_eq!(synthetic_extension_for("just { braces }"), "txt");
    }

    /// WO 17.5: `build_messages_with_compaction` passes `use_llm` and
    /// `drop_threshold` through to microcompaction. When `use_llm=false`
    /// (the default), the heuristic summary is always used.
    #[test]
    fn build_messages_with_compaction_default_uses_heuristic() {
        let mut builder = PromptBuilder::new();
        let system = Message {
            role: Role::System,
            content: "S".into(),
            ..Default::default()
        };
        // Build a history long enough to trigger compaction (budget = 0).
        let mut history = Vec::new();
        for i in 0..20 {
            history.push(Message {
                role: Role::User,
                content: format!("Message {i}"),
                ..Default::default()
            });
        }
        let result = builder.build_messages_with_compaction(
            system,
            &history,
            0, // budget = 0 forces compaction
            &[],
            false, // use_llm = false (heuristic)
            0.5,   // drop_threshold
        );
        // Should have compacted; the result is shorter than the input.
        assert!(result.len() < history.len() + 1);
    }

    /// WO 17.5: `build_with_top_files` injects file bodies into the system
    /// message so they become part of the cached stem.
    #[test]
    fn build_with_top_files_injects_files_into_system_message() {
        let mut builder = PromptBuilder::new();
        let top_files = vec![
            (
                std::path::PathBuf::from("src/main.rs"),
                "fn main() {}".to_string(),
            ),
            (
                std::path::PathBuf::from("src/lib.rs"),
                "pub fn lib() {}".to_string(),
            ),
        ];
        let msg = builder.build_with_top_files(
            "test-model",
            false,
            &["bash"],
            None,
            None,
            false,
            0,
            0,
            &top_files,
        );
        assert_eq!(msg.role, Role::System);
        assert!(msg.content.contains("src/main.rs"));
        assert!(msg.content.contains("fn main()"));
        assert!(msg.content.contains("src/lib.rs"));
        assert!(msg.content.contains("pub fn lib()"));
        assert!(msg.content.contains("Frequently accessed files"));
    }

    /// WO 17.5: `build_with_top_files` with no files falls back to
    /// `build()` unchanged — no "Frequently accessed files" block.
    #[test]
    fn build_with_top_files_no_files_falls_back_to_build() {
        let mut builder = PromptBuilder::new();
        let msg_without = builder.build_with_top_files(
            "test-model",
            false,
            &["bash"],
            None,
            None,
            false,
            0,
            0,
            &[],
        );
        let mut builder2 = PromptBuilder::new();
        let msg_plain = builder2.build("test-model", false, &["bash"], None, None, false, 0, 0);
        assert_eq!(msg_without.content, msg_plain.content);
        assert!(!msg_without.content.contains("Frequently accessed files"));
    }

    #[test]
    fn estimate_message_tokens_uses_cache_when_some() {
        let msg = Message {
            role: Role::User,
            content: "hello world this is a test".into(),
            token_count: Some(99),
            ..Default::default()
        };
        assert_eq!(estimate_message_tokens(&msg), 99);
    }

    #[test]
    fn estimate_message_tokens_falls_back_when_none() {
        let msg = Message {
            role: Role::User,
            content: "hello world this is a test".into(),
            token_count: None,
            ..Default::default()
        };
        let cached = estimate_message_tokens(&msg);
        let counted = count_tokens("hello world this is a test");
        assert_eq!(cached, counted);
    }

    #[test]
    fn dedup_adjacent_tool_results_clears_token_count_cache() {
        let mk = |content: &str| Message {
            role: Role::Tool,
            content: content.to_string(),
            token_count: Some(50),
            ..Default::default()
        };
        let mut msgs = vec![mk("same"), mk("same")];
        PromptBuilder::dedup_adjacent_tool_results(&mut msgs);
        assert_eq!(msgs[1].token_count, None, "dedup must clear cache");
    }

    #[test]
    fn truncate_tool_results_clears_token_count_cache() {
        let long_content = "x".repeat(30_000);
        let mut msgs = vec![Message {
            role: Role::Tool,
            content: long_content,
            tool_name: Some("grep".into()),
            token_count: Some(1000),
            ..Default::default()
        }];
        PromptBuilder::truncate_tool_results(&mut msgs);
        assert_eq!(msgs[0].token_count, None, "truncation must clear cache");
    }
}
