//! Cursor host shim. Per ADR-0013.
//!
//! Cursor wraps tool results under a `result` field with `content` and
//! `id` sub-keys, and expects the response in a `patch` field.

use serde_json::Value;

use crate::canonical::{PostToolUsePayload, PostToolUseResponse};

/// Parse Cursor's PostToolUse stdin JSON into the canonical payload.
/// Cursor's envelope nests the tool result: `result.content` and `result.id`.
pub fn parse_post_tool_use(stdin: &str) -> Result<PostToolUsePayload, String> {
    let v: Value =
        serde_json::from_str(stdin).map_err(|e| format!("cursor PostToolUse parse: {e}"))?;
    let tool_name = v["tool_name"].as_str().unwrap_or("unknown").to_string();
    let content = v["result"]["content"].as_str().unwrap_or("").to_string();
    let tool_result_key = v["result"]["id"].as_str().unwrap_or("").to_string();
    let session_id = v["session_id"].as_str().unwrap_or("").to_string();
    Ok(PostToolUsePayload {
        tool_name,
        tool_result_key,
        content,
        session_id,
    })
}

/// Serialise a canonical PostToolUseResponse back to Cursor's expected
/// envelope: `{ "patch": { "content": "..." }, "note": null }`.
pub fn format_post_tool_use(resp: &PostToolUseResponse) -> Value {
    serde_json::json!({
        "patch": {
            "content": resp.content,
        },
        "note": resp.note,
    })
}

/// Parse Cursor's UserPromptSubmit stdin JSON into the canonical payload.
/// Cursor's envelope matches the canonical shape.
pub fn parse_user_prompt_submit(
    stdin: &str,
) -> Result<crate::canonical::UserPromptSubmitPayload, String> {
    serde_json::from_str(stdin).map_err(|e| format!("cursor UserPromptSubmit parse: {e}"))
}

/// Serialise a canonical UserPromptSubmitResponse to Cursor's expected JSON.
pub fn format_user_prompt_submit(resp: &crate::canonical::UserPromptSubmitResponse) -> Value {
    serde_json::to_value(resp).unwrap_or(serde_json::json!({"kind": "allow"}))
}

/// Parse Cursor's PreCompact stdin JSON into the canonical payload.
pub fn parse_pre_compact(stdin: &str) -> Result<crate::canonical::PreCompactPayload, String> {
    serde_json::from_str(stdin).map_err(|e| format!("cursor PreCompact parse: {e}"))
}

/// Serialise a canonical PreCompactResponse to Cursor's expected JSON.
pub fn format_pre_compact(resp: &crate::canonical::PreCompactResponse) -> Value {
    serde_json::json!({
        "hint": resp.hint,
        "summary": null,
    })
}

/// Cursor hook configuration for `register_hooks`.
/// Returns the full three-slot HookConfig matching ADR-0009.
pub fn hook_config() -> serde_json::Value {
    serde_json::json!({
        "PostToolUse": [{"type": "command", "command": "kf-budget hook post-tool-use", "timeout": 5}],
        "UserPromptSubmit": [{"type": "command", "command": "kf-budget hook user-prompt-submit", "timeout": 2}],
        "PreCompact": [{"type": "command", "command": "kf-budget hook pre-compact", "timeout": 10}],
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn module_path_matches_adr_layout() {
        assert_eq!(
            std::module_path!(),
            "kf_budget_hosts::cursor::tests",
            "cursor module path drifted from ADR-0013 layout"
        );
    }

    #[test]
    fn parse_post_tool_use_translates_envelope() {
        let json = r#"{"tool_name":"bash","result":{"content":"hello","id":"res_1"}}"#;
        let payload = parse_post_tool_use(json).unwrap();
        assert_eq!(payload.tool_name, "bash");
        assert_eq!(payload.content, "hello");
        assert_eq!(payload.tool_result_key, "res_1");
    }

    #[test]
    fn parse_post_tool_use_defaults_missing_fields() {
        let json = r#"{"tool_name":"bash"}"#;
        let payload = parse_post_tool_use(json).unwrap();
        assert_eq!(payload.content, "");
        assert_eq!(payload.tool_result_key, "");
        assert_eq!(payload.session_id, "");
    }

    #[test]
    fn format_post_tool_use_uses_patch_envelope() {
        let resp = PostToolUseResponse {
            content: "modified".into(),
            note: Some("sliced".into()),
        };
        let v = format_post_tool_use(&resp);
        assert_eq!(v["patch"]["content"], "modified");
        assert_eq!(v["note"], "sliced");
    }

    #[test]
    fn round_trip_cursor_post_tool_use() {
        let json = r#"{"tool_name":"edit_file","result":{"content":"new content","id":"ed_42"}}"#;
        let payload = parse_post_tool_use(json).unwrap();
        let resp = PostToolUseResponse {
            content: payload.content.clone(),
            note: None,
        };
        let v = format_post_tool_use(&resp);
        assert_eq!(v["patch"]["content"], "new content");
    }

    #[test]
    fn parse_user_prompt_submit_round_trips() {
        let json = r#"{"prompt":"hello cursor","session_id":"c1"}"#;
        let payload = parse_user_prompt_submit(json).unwrap();
        assert_eq!(payload.prompt, "hello cursor");
    }

    #[test]
    fn format_user_prompt_submit_allow() {
        let v = format_user_prompt_submit(&crate::canonical::UserPromptSubmitResponse::Allow);
        assert_eq!(v["kind"], "allow");
    }

    #[test]
    fn format_user_prompt_submit_warn() {
        let v = format_user_prompt_submit(&crate::canonical::UserPromptSubmitResponse::Warn {
            remaining: 10,
        });
        assert_eq!(v["kind"], "warn");
        assert_eq!(v["remaining"], 10);
    }

    #[test]
    fn parse_pre_compact_round_trips() {
        let json = r#"{"history_turns":[{"index":0,"role":"user","content_preview":"hi"}]}"#;
        let payload = parse_pre_compact(json).unwrap();
        assert_eq!(payload.history_turns.len(), 1);
        assert_eq!(payload.history_turns[0].role, "user");
    }

    #[test]
    fn format_pre_compact_produces_expected_keys() {
        let resp = crate::canonical::PreCompactResponse {
            hint: serde_json::json!({"turns": 3}),
        };
        let v = format_pre_compact(&resp);
        assert_eq!(v["hint"]["turns"], 3);
        assert!(v.get("summary").is_some());
    }

    #[test]
    fn hook_config_has_all_three_slots() {
        let v = hook_config();
        assert!(v.get("PostToolUse").is_some());
        assert!(v.get("UserPromptSubmit").is_some());
        assert!(v.get("PreCompact").is_some());
    }
}
