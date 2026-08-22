//! Stream-iteration preamble builder.
//!
//! Snapshots config, memory context, top-files, builds the system + messages
//! list, records the prompt-cache stem hash, and estimates stem tokens —
//! everything `stream_iteration` needs before it starts consuming the SSE
//! stream from the adapter. Extracted from `turn.rs::stream_iteration` (WO
//! 28.3-R3) so the SSE-driver loop reads as a loop, not a 170-line preamble.

use crate::session::toolset::Toolset;
use crate::shared::metrics::{record, MetricEvent, PlanDecisionKind};
use crate::shared::{read_shared_config, Message, Role, ToolDef};

use super::Executor;

pub(super) struct StreamPreamble {
    pub(super) messages: Vec<Message>,
    pub(super) tool_defs: Vec<ToolDef>,
    pub(super) stem_tokens: usize,
}

impl Executor {
    pub(super) fn build_stream_preamble(&mut self, user_input: &str) -> StreamPreamble {
        let model_info = self.adapter.model_info();
        let tool_defs: Vec<ToolDef> = self.tools.definitions();
        let tool_names: Vec<&str> = tool_defs.iter().map(|t| t.name).collect();

        let carryover_block = if self.cost.carryover_enabled {
            let block = self.cost.carryover.to_prompt_block();
            if block.is_empty() {
                None
            } else {
                Some(block)
            }
        } else {
            None
        };

        // Snapshot memory knobs and compaction knobs so we don't hold the
        // config lock across the prompt-builder memory lookup or the
        // microcompaction call.
        let (
            memory_enabled,
            memory_max_tokens,
            memory_top_n,
            memory_auto_populate,
            compaction_use_heuristic,
            compaction_drop_threshold,
            stem_file_cap,
        ) = {
            let cfg = read_shared_config(&self.config);
            (
                cfg.display.memory_enabled,
                cfg.display.memory_max_tokens,
                cfg.display.memory_top_n,
                cfg.display.memory_auto_populate,
                cfg.session.compaction_use_heuristic,
                cfg.session.compaction_drop_threshold,
                cfg.session.stem_file_cap,
            )
        };

        // Build a richer memory context from the current user turn plus
        // the most recent assistant message, if any.
        // When memory_auto_populate is false, skip auto-extraction.
        let memory_context = if memory_auto_populate {
            let history = self.conversation.all();
            let mut ctx = String::from(user_input);
            if let Some(last_assistant) = history
                .iter()
                .rev()
                .find(|m| matches!(m.role, Role::Assistant) && !m.content.is_empty())
            {
                ctx.push(' ');
                ctx.push_str(&last_assistant.content);
            }
            if ctx.trim().is_empty() {
                None
            } else {
                Some(ctx)
            }
        } else {
            None
        };

        // WO 17.5: inject top-N frequently-accessed file bodies into the
        // cached stem so the model stops re-reading the same files every
        // turn. The files come from the read gate's access stats; their
        // contents are minified to keep the stem small.
        let top_file_paths = self
            .sandbox
            .top_files(crate::session::prompt::cache_stem::DEFAULT_TOP_N_FILES);
        let top_files: Vec<(std::path::PathBuf, String)> = top_file_paths
            .iter()
            .filter_map(|p| {
                let content = std::fs::read_to_string(p).ok()?;
                // ponytail: minify each file to keep the stem small; if
                // minification fails or inflates, use the raw content as a
                // fallback so the file is still present in the stem.
                let minified = crate::shared::minify::minify_source_safe(p, &content);
                if minified.len() < content.len() {
                    Some((p.clone(), minified))
                } else {
                    // Minification didn't help; use a truncated version of
                    // the raw content to keep the stem bounded.
                    // ponytail: truncate at 4 KiB default, configurable via stem_file_cap —
                    // enough for context, small enough for cache.
                    // Keep in sync with Config::default().compaction.stem_file_cap
                    const STEM_FILE_CAP: usize = 4096;
                    let cap = stem_file_cap.unwrap_or(STEM_FILE_CAP);
                    if content.len() > cap {
                        Some((p.clone(), format!("{} [...truncated]", &content[..cap])))
                    } else {
                        Some((p.clone(), content))
                    }
                }
            })
            .collect();

        let system = if top_files.is_empty() {
            self.prompt_builder.build(
                &model_info.name,
                model_info.supports_thinking,
                &tool_names,
                carryover_block.as_deref(),
                memory_context.as_deref(),
                memory_enabled,
                memory_max_tokens,
                memory_top_n,
            )
        } else {
            self.prompt_builder.build_with_top_files(
                &model_info.name,
                model_info.supports_thinking,
                &tool_names,
                carryover_block.as_deref(),
                memory_context.as_deref(),
                memory_enabled,
                memory_max_tokens,
                memory_top_n,
                &top_files,
            )
        };

        let history = self.conversation.all();
        let tool_results: Vec<Message> = Vec::new(); // sent as part of history

        let mut messages = self.prompt_builder.build_messages_with_compaction(
            system,
            history,
            model_info.max_context_tokens,
            &tool_results,
            compaction_use_heuristic,
            compaction_drop_threshold,
        );

        // WO 38.9 item 6: inject the volatile suffix (carryover + memory
        // facts) as a separate system message AFTER the stable stem
        // (messages[0]) so the cache-stem tracker (prefix_len=1) sees a
        // stable prefix. The stem is byte-identical across turns; only
        // the suffix changes. This is what makes the provider KV cache
        // actually hit.
        if let Some(suffix) = self.prompt_builder.build_dynamic_suffix(
            carryover_block.as_deref(),
            memory_context.as_deref(),
            memory_enabled,
            memory_max_tokens,
            memory_top_n,
        ) {
            // Insert after the stem (index 0). If the context index
            // already inserted a relevant_symbols message at index 1,
            // the suffix goes after it — both are volatile, order
            // doesn't matter for cache stability (prefix_len=1).
            let insert_at = 1.min(messages.len());
            messages.insert(insert_at, suffix);
        }

        // WO 10.2: prompt-cache stem-reuse detection (ADR-052). The
        // stable prefix is the system message — the one part of the
        // prompt that is byte-for-byte identical across turns when the
        // model, tool list, carryover, and memory inputs are unchanged.
        // The conversation history grows every turn, so it cannot be
        // part of the stable stem; `prefix_len = 1` (system message
        // only) is the first-cut policy documented in ADR-052's
        // Future Work. When the stem is stable, emit a
        // `PlanReason::CacheStemReuse` metric event so operators can
        // see the stem is being reused (the server-side KV-cache hit
        // is reported by the adapter's usage stats; this is the
        // client-side observability signal). Then advance the recorded
        // hash for the next turn.
        //
        // The Anthropic API requires the full content of every message
        // on every request (the cache key is computed from the bytes),
        // so there is no adapter short-circuit to make here — the
        // `cache_control` markers in `anthropic.rs` are unchanged. This
        // is the measurement, not a wire-bytes saving (ADR-052).
        let prefix_len = 1;
        if self.cost.cache_stem.is_stable(&messages, prefix_len) {
            record(MetricEvent::PlanReason {
                decision_kind: PlanDecisionKind::CacheStemReuse,
                reason: "prompt-cache stem stable across turns".into(),
                related_id: None,
                confidence: 1.0,
            });
        }
        self.cost
            .cache_stem
            .record_prefix_hash(&messages, prefix_len);

        // Snapshot the stable prompt-cache stem size for this turn so we
        // can verify KV-cache reuse against the adapter usage stats.
        let stem_tokens = self.prompt_builder.estimate_stem_tokens(
            &model_info.name,
            model_info.supports_thinking,
            &tool_names,
        );

        StreamPreamble {
            messages,
            tool_defs,
            stem_tokens,
        }
    }
}
