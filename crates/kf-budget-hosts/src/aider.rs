//! Aider host shim. Per ADR-0013.
//!
//! Aider pipes tool results via stdin as JSON. The envelope is
//! flat and matches the canonical PostToolUse shape directly
//! (no nested `result` field like Cursor).

use crate::canonical::{
    PostToolUsePayload, PostToolUseResponse, PreCompactPayload, PreCompactResponse,
    UserPromptSubmitPayload, UserPromptSubmitResponse,
};

/// Parse Aider's PostToolUse stdin JSON into the canonical payload.
/// Aider's envelope matches the canonical shape directly.
pub fn parse_post_tool_use(stdin: &str) -> Result<PostToolUsePayload, String> {
    serde_json::from_str(stdin).map_err(|e| format!("aider PostToolUse parse: {e}"))
}

/// Serialise a canonical PostToolUseResponse back to Aider's expected
/// flat JSON envelope.
pub fn format_post_tool_use(resp: &PostToolUseResponse) -> serde_json::Value {
    serde_json::json!({
        "content": resp.content,
        "note": resp.note,
    })
}

/// Parse Aider's UserPromptSubmit stdin JSON.
pub fn parse_user_prompt_submit(stdin: &str) -> Result<UserPromptSubmitPayload, String> {
    serde_json::from_str(stdin).map_err(|e| format!("aider UserPromptSubmit parse: {e}"))
}

/// Serialise a canonical UserPromptSubmitResponse to Aider's expected JSON.
pub fn format_user_prompt_submit(resp: &UserPromptSubmitResponse) -> serde_json::Value {
    serde_json::to_value(resp).unwrap_or(serde_json::json!({"kind": "allow"}))
}

/// Parse Aider's PreCompact stdin JSON.
pub fn parse_pre_compact(stdin: &str) -> Result<PreCompactPayload, String> {
    serde_json::from_str(stdin).map_err(|e| format!("aider PreCompact parse: {e}"))
}

/// Serialise a canonical PreCompactResponse to Aider's expected JSON.
pub fn format_pre_compact(resp: &PreCompactResponse) -> serde_json::Value {
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
            "kf_budget_hosts::aider::tests",
            "aider module path drifted from ADR-0013 layout"
        );
    }

    #[test]
    fn parse_post_tool_use_round_trips() {
        let json = r#"{"tool_name":"bash","content":"output","session_id":"s1"}"#;
        let payload = parse_post_tool_use(json).unwrap();
        assert_eq!(payload.tool_name, "bash");
        assert_eq!(payload.content, "output");
    }

    #[test]
    fn format_post_tool_use_produces_expected_keys() {
        let resp = PostToolUseResponse {
            content: "kept".into(),
            note: None,
        };
        let v = format_post_tool_use(&resp);
        assert_eq!(v["content"], "kept");
        assert!(v.get("note").is_some());
    }

    #[test]
    fn round_trip_aider_post_tool_use() {
        let json = r#"{"tool_name":"write","content":"file contents here"}"#;
        let payload = parse_post_tool_use(json).unwrap();
        let resp = PostToolUseResponse {
            content: payload.content.clone(),
            note: None,
        };
        let v = format_post_tool_use(&resp);
        assert_eq!(v["content"], "file contents here");
    }
}
