//! Claude Code host shim. Per ADR-0013.
//!
//! Claude Code pipes hook payloads as JSON on stdin. The envelope
//! matches the canonical types directly (Claude Code *is* the
//! reference host). These functions provide a thin typed
//! translation layer so the CLI hook handlers don't parse raw
//! `serde_json::Value`.

use crate::canonical::{
    PostToolUsePayload, PostToolUseResponse, PreCompactPayload, PreCompactResponse,
    UserPromptSubmitPayload, UserPromptSubmitResponse,
};

/// Parse Claude Code's PostToolUse stdin JSON into the canonical payload.
/// Claude Code's envelope matches the canonical shape exactly.
pub fn parse_post_tool_use(stdin: &str) -> Result<PostToolUsePayload, String> {
    serde_json::from_str(stdin).map_err(|e| format!("claude-code PostToolUse parse: {e}"))
}

/// Serialise a canonical PostToolUseResponse back to Claude Code's
/// expected JSON envelope.
pub fn format_post_tool_use(resp: &PostToolUseResponse) -> serde_json::Value {
    serde_json::json!({
        "content": resp.content,
        "note": resp.note,
    })
}

/// Parse Claude Code's UserPromptSubmit stdin JSON.
/// Claude Code's envelope matches the canonical shape exactly.
pub fn parse_user_prompt_submit(stdin: &str) -> Result<UserPromptSubmitPayload, String> {
    serde_json::from_str(stdin).map_err(|e| format!("claude-code UserPromptSubmit parse: {e}"))
}

/// Serialise a canonical UserPromptSubmitResponse to Claude Code's
/// expected tagged-enum JSON.
pub fn format_user_prompt_submit(resp: &UserPromptSubmitResponse) -> serde_json::Value {
    serde_json::to_value(resp).unwrap_or(serde_json::json!({"kind": "allow"}))
}

/// Parse Claude Code's PreCompact stdin JSON.
/// Claude Code wraps turns in `history_turns` with `index`/`role`/`content_preview`.
pub fn parse_pre_compact(stdin: &str) -> Result<PreCompactPayload, String> {
    serde_json::from_str(stdin).map_err(|e| format!("claude-code PreCompact parse: {e}"))
}

/// Serialise a canonical PreCompactResponse to Claude Code's expected
/// `{ "hint": ..., "summary": ... }` envelope.
pub fn format_pre_compact(resp: &PreCompactResponse) -> serde_json::Value {
    serde_json::json!({
        "hint": resp.hint,
        "summary": null,
    })
}

/// Claude Code hook configuration for `register_hooks`.
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
            "kf_budget_hosts::claude_code::tests",
            "claude_code module path drifted from ADR-0013 layout"
        );
    }

    #[test]
    fn parse_post_tool_use_round_trips() {
        let json = r#"{"tool_name":"bash","tool_result_key":"toolu_1","content":"hello","session_id":"s1"}"#;
        let payload = parse_post_tool_use(json).unwrap();
        assert_eq!(payload.tool_name, "bash");
        assert_eq!(payload.tool_result_key, "toolu_1");
        assert_eq!(payload.content, "hello");
        assert_eq!(payload.session_id, "s1");
    }

    #[test]
    fn parse_post_tool_use_defaults_optional_fields() {
        let json = r#"{"tool_name":"bash","content":"hello"}"#;
        let payload = parse_post_tool_use(json).unwrap();
        assert_eq!(payload.tool_result_key, "");
        assert_eq!(payload.session_id, "");
    }

    #[test]
    fn format_post_tool_use_produces_expected_keys() {
        let resp = PostToolUseResponse {
            content: "modified".into(),
            note: Some("sliced".into()),
        };
        let v = format_post_tool_use(&resp);
        assert_eq!(v["content"], "modified");
        assert_eq!(v["note"], "sliced");
    }

    #[test]
    fn parse_user_prompt_submit_round_trips() {
        let json = r#"{"prompt":"hello world","session_id":"s1"}"#;
        let payload = parse_user_prompt_submit(json).unwrap();
        assert_eq!(payload.prompt, "hello world");
    }

    #[test]
    fn format_user_prompt_submit_allow() {
        let v = format_user_prompt_submit(&UserPromptSubmitResponse::Allow);
        assert_eq!(v["kind"], "allow");
    }

    #[test]
    fn format_user_prompt_submit_warn() {
        let v = format_user_prompt_submit(&UserPromptSubmitResponse::Warn { remaining: 42 });
        assert_eq!(v["kind"], "warn");
        assert_eq!(v["remaining"], 42);
    }

    #[test]
    fn parse_pre_compact_round_trips() {
        let json = r#"{"history_turns":[{"index":0,"role":"user","content_preview":"hi"}]}"#;
        let payload = parse_pre_compact(json).unwrap();
        assert_eq!(payload.history_turns.len(), 1);
        assert_eq!(payload.history_turns[0].index, 0);
        assert_eq!(payload.history_turns[0].role, "user");
    }

    #[test]
    fn format_pre_compact_produces_expected_keys() {
        let resp = PreCompactResponse {
            hint: serde_json::json!({"turns": 5}),
        };
        let v = format_pre_compact(&resp);
        assert_eq!(v["hint"]["turns"], 5);
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
