//! Streamed tool-call accumulation for the OpenAI-compat adapter.
//!
//! Extracted from the SSE parser in `mod.rs`: accumulates incremental
//! `tool_calls` deltas, splits concatenated argument objects, and
//! de-duplicates ids.

use crate::shared::ToolInvocation;

/// Accumulator for OpenAI SSE tool-call deltas.
///
/// OpenAI streams tool calls incrementally across multiple SSE events.
/// The first delta has `id` and `name`, subsequent deltas only have
/// `arguments` fragments. Keyed by `index` (0-based within the array).
///
/// Example delta sequence:
///   {index: 0, id: "call_1", function: {name: "read_file", arguments: ""}}
///   {index: 0, id: null,      function: {name: null,        arguments: "{\"path\":" }}
///   {index: 0, id: null,      function: {name: null,        arguments: " \"/etc\"}" }}
pub(super) struct ToolCallAccumulator {
    /// Keyed by `index` field from the delta.
    calls: std::collections::HashMap<usize, (String, String, String)>, // (id, name, args_json)
}

impl ToolCallAccumulator {
    pub(super) fn new() -> Self {
        Self {
            calls: std::collections::HashMap::new(),
        }
    }

    /// Accumulate one delta. Merges `arguments` by appending.
    pub(super) fn accumulate(
        &mut self,
        index: usize,
        id: &str,
        name: Option<&str>,
        args: Option<&str>,
    ) {
        let entry = self
            .calls
            .entry(index)
            .or_insert_with(|| (id.to_string(), String::new(), String::new()));
        // ID: only set on first delta — keep whatever we get
        if !id.is_empty() {
            entry.0 = id.to_string();
        }
        // Name: set when present (first delta, usually)
        if let Some(n) = name {
            entry.1 = n.to_string();
        }
        // Arguments: append incrementally across deltas
        if let Some(a) = args {
            entry.2.push_str(a);
        }
    }

    /// Drain all accumulated calls as ToolInvocation values.
    ///
    /// Two adapter-layer problems are handled here:
    ///
    /// 1. **Concatenated argument objects.** Some models
    ///    (notably `minimax-m3:cloud` via Ollama's
    ///    OpenAI-compat layer) emit multiple parallel tool
    ///    calls in a single delta with their argument
    ///    objects *concatenated* into one string, e.g.
    ///
    ///      arguments = `"{\"path\":\"a\"}{\"path\":\"b\"}"`
    ///
    ///    We split on top-level JSON object boundaries
    ///    and emit one `ToolInvocation` per object.
    ///
    /// 2. **Duplicate call IDs.** The same model
    ///    occasionally emits multiple `tool_calls` under
    ///    the same `id` field, which is not spec-
    ///    compliant. Ollama's OpenAI-compat layer
    ///    rejects subsequent requests that reference
    ///    those duplicate ids. We de-duplicate by
    ///    suffixing the duplicate id with `__<n>`
    ///    so every emitted call has a unique id.
    ///    Spec-compliant servers that already emit unique
    ///    ids pass through unchanged.
    pub(super) fn drain(&mut self) -> Vec<ToolInvocation> {
        let mut out: Vec<_> = self.calls.drain().collect();
        out.sort_by_key(|(idx, _)| *idx);
        let mut seen_ids: std::collections::HashSet<String> =
            std::collections::HashSet::with_capacity(out.len());
        let mut next: usize = 0;
        out.into_iter()
            .flat_map(|(_, (id, name, args_json))| {
                // WO 38.5: a named call with empty arguments is a
                // zero-argument invocation (`json!({})`), not a dropped
                // call. Emitting nothing here made a `finish_reason:
                // "tool_calls"` turn error-loop on "no parseable tool
                // calls". A call with neither name nor arguments stays
                // dropped (nothing to invoke).
                let args = if args_json.trim().is_empty() {
                    if name.is_empty() {
                        vec![]
                    } else {
                        vec![serde_json::Value::Object(Default::default())]
                    }
                } else {
                    split_concatenated_json(&args_json)
                };
                args.into_iter()
                    .map(|arg| {
                        let unique_id = if seen_ids.insert(id.clone()) {
                            id.clone()
                        } else {
                            next += 1;
                            format!("{id}__{next}")
                        };
                        ToolInvocation {
                            id: unique_id,
                            name: name.clone(),
                            arguments: arg,
                        }
                    })
                    .collect::<Vec<_>>()
            })
            .collect()
    }

    pub(super) fn is_empty(&self) -> bool {
        self.calls.is_empty()
    }
}

/// Split an argument string that may contain one or more
/// top-level JSON objects concatenated together.
///
/// The OpenAI streaming spec says each `tool_call` entry carries
/// one JSON-encoded argument object. Some adapters (notably Ollama
/// routing `minimax-m3:cloud`) emit multiple parallel tool calls
/// in a single delta with their argument objects *concatenated*,
/// e.g. `{"path":"a"}{"path":"b"}`. This helper recovers the
/// original list of values so the executor sees each as a
/// separate `ToolInvocation`.
///
/// Behaviour:
/// 1. Trim outer whitespace.
/// 2. Try to parse the whole string as a single JSON value.
///    If that succeeds, return it wrapped in a one-element vec.
/// 3. Otherwise, walk the string character-by-character tracking
///    brace depth (and quote state, with backslash escapes) and
///    split at every depth-0 `}`. Parse each slice as JSON.
/// 4. Drop slices that fail to parse (defensive — the executor
///    already handles `Value::String` fallbacks).
pub(super) fn split_concatenated_json(s: &str) -> Vec<serde_json::Value> {
    let trimmed = s.trim();
    if trimmed.is_empty() {
        return vec![];
    }
    // 1. Whole-string parse first.
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(trimmed) {
        return vec![v];
    }
    // 2. Walk the string looking for top-level JSON object boundaries.
    let bytes = trimmed.as_bytes();
    let mut depth: i32 = 0;
    let mut in_str = false;
    let mut escape = false;
    let mut slice_start: Option<usize> = None;
    let mut out: Vec<serde_json::Value> = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        if escape {
            escape = false;
            i += 1;
            continue;
        }
        if in_str {
            match c {
                b'\\' => escape = true,
                b'"' => in_str = false,
                _ => {}
            }
            i += 1;
            continue;
        }
        match c {
            b'"' => in_str = true,
            b'{' => {
                if depth == 0 {
                    slice_start = Some(i);
                }
                depth += 1;
            }
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    if let Some(start) = slice_start {
                        let slice = &trimmed[start..=i];
                        if let Ok(v) = serde_json::from_str::<serde_json::Value>(slice) {
                            out.push(v);
                        }
                        slice_start = None;
                    }
                }
                if depth < 0 {
                    // Stray closing brace — bail out and let the
                    // caller fall back to the original string.
                    return vec![serde_json::Value::String(s.to_string())];
                }
            }
            _ => {}
        }
        i += 1;
    }
    if out.is_empty() {
        // Nothing parsed cleanly — fall back to the original
        // behaviour of stuffing the raw string into a `Value::String`
        // so the rest of the pipeline (which already handles this
        // case) keeps working.
        vec![serde_json::Value::String(s.to_string())]
    } else {
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn accumulator_new_is_empty() {
        let a = ToolCallAccumulator::new();
        assert!(a.is_empty());
    }

    #[test]
    fn accumulator_not_empty_after_accumulate() {
        let mut a = ToolCallAccumulator::new();
        a.accumulate(0, "id", Some("name"), Some("{}"));
        assert!(!a.is_empty());
    }

    #[test]
    fn accumulator_empty_id_uses_first_non_empty() {
        let mut a = ToolCallAccumulator::new();
        a.accumulate(0, "", Some("bash"), Some("{}"));
        a.accumulate(0, "call_1", None, None);
        let calls = a.drain();
        assert_eq!(calls[0].id, "call_1");
    }

    #[test]
    fn accumulator_appends_arguments_across_deltas() {
        let mut a = ToolCallAccumulator::new();
        a.accumulate(0, "id", Some("bash"), Some("{\"cmd\":\""));
        a.accumulate(0, "", None, Some("ls\"}"));
        let calls = a.drain();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].arguments, json!({"cmd": "ls"}));
    }

    #[test]
    fn accumulator_name_overwritten_by_later_delta() {
        let mut a = ToolCallAccumulator::new();
        a.accumulate(0, "id", Some("first"), Some("{}"));
        a.accumulate(0, "", Some("second"), None);
        let calls = a.drain();
        assert_eq!(calls[0].name, "second");
    }

    #[test]
    fn accumulator_preserves_index_order() {
        let mut a = ToolCallAccumulator::new();
        a.accumulate(2, "c", Some("tool_c"), Some("{}"));
        a.accumulate(0, "a", Some("tool_a"), Some("{}"));
        a.accumulate(1, "b", Some("tool_b"), Some("{}"));
        let calls = a.drain();
        assert_eq!(calls[0].id, "a");
        assert_eq!(calls[1].id, "b");
        assert_eq!(calls[2].id, "c");
    }

    #[test]
    fn accumulator_empty_arguments_with_name_emits_zero_arg_call() {
        // WO 38.5: previously this pinned the bug — a named call with no
        // argument deltas produced ZERO invocations, so a zero-arg tool
        // call error-looped ("no parseable tool calls"). Now it emits a
        // single `json!({})` invocation.
        let mut a = ToolCallAccumulator::new();
        a.accumulate(0, "id", Some("bash"), None);
        let calls = a.drain();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "bash");
        assert_eq!(calls[0].arguments, json!({}));
    }

    #[test]
    fn accumulator_empty_arguments_without_name_stays_dropped() {
        let mut a = ToolCallAccumulator::new();
        a.accumulate(0, "id", None, None);
        let calls = a.drain();
        assert!(calls.is_empty());
    }

    #[test]
    fn split_concatenated_json_whole_string_parse() {
        let s = r#"{"a":1}"#;
        let out = split_concatenated_json(s);
        assert_eq!(out, vec![json!({"a": 1})]);
    }

    #[test]
    fn split_concatenated_json_array_returns_as_single_value() {
        let s = "[1,2,3]";
        let out = split_concatenated_json(s);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0], json!([1, 2, 3]));
    }

    #[test]
    fn split_concatenated_json_whitespace_trimmed() {
        let s = "  {\"a\":1}  ";
        let out = split_concatenated_json(s);
        assert_eq!(out, vec![json!({"a": 1})]);
    }

    #[test]
    fn split_concatenated_json_stray_closing_brace_returns_string() {
        let s = "}}}";
        let out = split_concatenated_json(s);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0], json!("}}}"));
    }

    #[test]
    fn split_concatenated_json_nested_objects() {
        let s = r#"{"outer":{"inner":1}}"#;
        let out = split_concatenated_json(s);
        assert_eq!(out, vec![json!({"outer": {"inner": 1}})]);
    }

    #[test]
    fn split_concatenated_json_mixed_valid_invalid_drops_invalid() {
        let s = r#"{"a":1}{invalid}{"b":2}"#;
        let out = split_concatenated_json(s);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0], json!({"a": 1}));
        assert_eq!(out[1], json!({"b": 2}));
    }

    #[test]
    fn split_concatenated_json_only_whitespace_returns_empty() {
        let out = split_concatenated_json("   ");
        assert!(out.is_empty());
    }

    #[test]
    fn split_concatenated_json_unclosed_object_returns_string() {
        let s = r#"{"a":1"#;
        let out = split_concatenated_json(s);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0], json!(r#"{"a":1"#));
    }

    #[test]
    fn split_concatenated_json_string_with_braces_inside() {
        let s = r#"{"path":"{weird}"}"#;
        let out = split_concatenated_json(s);
        assert_eq!(out, vec![json!({"path": "{weird}"})]);
    }

    #[test]
    fn split_concatenated_json_escaped_backslash_before_quote() {
        let s = r#"{"path":"a\\b"}"#;
        let out = split_concatenated_json(s);
        assert_eq!(out, vec![json!({"path": "a\\b"})]);
    }
}
