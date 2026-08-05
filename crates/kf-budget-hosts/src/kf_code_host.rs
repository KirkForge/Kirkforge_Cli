//! KirkForge host shim. Per ADR-0013.
//!
//! KirkForge-Cli is the sibling host in the same plugin ecosystem.
//! Its hook model emits the same canonical events as Claude Code,
//! so the shim is a thin pass-through with typed parse/format.

use crate::canonical::{
    PostToolUsePayload, PostToolUseResponse, PreCompactPayload, PreCompactResponse,
    UserPromptSubmitPayload, UserPromptSubmitResponse,
};

/// Parse KirkForge's PostToolUse stdin JSON into the canonical payload.
/// KirkForge uses the same canonical envelope as Claude Code.
pub fn parse_post_tool_use(stdin: &str) -> Result<PostToolUsePayload, String> {
    serde_json::from_str(stdin).map_err(|e| format!("kf-code PostToolUse parse: {e}"))
}

/// Serialise a canonical PostToolUseResponse back to KirkForge's expected JSON.
pub fn format_post_tool_use(resp: &PostToolUseResponse) -> serde_json::Value {
    serde_json::json!({
        "content": resp.content,
        "note": resp.note,
    })
}

/// Parse KirkForge's UserPromptSubmit stdin JSON.
pub fn parse_user_prompt_submit(stdin: &str) -> Result<UserPromptSubmitPayload, String> {
    serde_json::from_str(stdin).map_err(|e| format!("kf-code UserPromptSubmit parse: {e}"))
}

/// Serialise a canonical UserPromptSubmitResponse to KirkForge's expected JSON.
pub fn format_user_prompt_submit(resp: &UserPromptSubmitResponse) -> serde_json::Value {
    serde_json::to_value(resp).unwrap_or(serde_json::json!({"kind": "allow"}))
}

/// Parse KirkForge's PreCompact stdin JSON.
pub fn parse_pre_compact(stdin: &str) -> Result<PreCompactPayload, String> {
    serde_json::from_str(stdin).map_err(|e| format!("kf-code PreCompact parse: {e}"))
}

/// Serialise a canonical PreCompactResponse to KirkForge's expected JSON.
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
            "kf_budget_hosts::kf_code_host::tests",
            "kf-code module path drifted from ADR-0013 layout"
        );
    }

    #[test]
    fn parse_post_tool_use_round_trips() {
        let json = r#"{"tool_name":"bash","content":"hello","session_id":"s1"}"#;
        let payload = parse_post_tool_use(json).unwrap();
        assert_eq!(payload.tool_name, "bash");
        assert_eq!(payload.content, "hello");
    }

    #[test]
    fn format_post_tool_use_produces_expected_keys() {
        let resp = PostToolUseResponse {
            content: "result".into(),
            note: None,
        };
        let v = format_post_tool_use(&resp);
        assert_eq!(v["content"], "result");
    }

    #[test]
    fn round_trip_kf_code_post_tool_use() {
        let json = r#"{"tool_name":"kf-draw","content":"rendered","session_id":"s5"}"#;
        let payload = parse_post_tool_use(json).unwrap();
        let resp = PostToolUseResponse {
            content: payload.content.clone(),
            note: None,
        };
        let v = format_post_tool_use(&resp);
        assert_eq!(v["content"], "rendered");
    }
}
