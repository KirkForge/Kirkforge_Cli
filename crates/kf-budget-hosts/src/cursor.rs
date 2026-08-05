//! Cursor host shim. Per ADR-0013.
//!
//! Cursor wraps tool results under a `result` field with `content` and
//! `id` sub-keys, and expects the response in a `patch` field.

use serde_json::Value;

use crate::canonical::{PostToolUsePayload, PostToolUseResponse};

/// Parse Cursor's PostToolUse stdin JSON into the canonical payload.
/// Cursor's envelope nests the tool result: `result.content` and `result.id`.
pub fn parse_post_tool_use(stdin: &str) -> Result<PostToolUsePayload, String> {
    let v: Value = serde_json::from_str(stdin)
        .map_err(|e| format!("cursor PostToolUse parse: {e}"))?;
    let tool_name = v["tool_name"]
        .as_str()
        .unwrap_or("unknown")
        .to_string();
    let content = v["result"]["content"]
        .as_str()
        .unwrap_or("")
        .to_string();
    let tool_result_key = v["result"]["id"]
        .as_str()
        .unwrap_or("")
        .to_string();
    let session_id = v["session_id"]
        .as_str()
        .unwrap_or("")
        .to_string();
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
pub fn parse_user_prompt_submit(stdin: &str) -> Result<crate::canonical::UserPromptSubmitPayload, String> {
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
}
