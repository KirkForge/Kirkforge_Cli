//! Canonical payload schemas — host-agnostic. Per ADR-0013.
//!
//! The canonical adapter is the identity translation: it parses and
//! emits the canonical wire format directly (no envelope mangling).

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PostToolUsePayload {
    pub tool_name: String,
    #[serde(default)]
    pub tool_result_key: String,
    pub content: String,
    // ponytail: session_id is load-bearing for ADR-0010's
    // usage.jsonl grouping. ADR-0013 lists tool_name/result_key/
    // content; the canonical schema absorbs session_id because
    // a host that doesn't tag sessions still emits it as
    // default-empty rather than breaking the cost reporter.
    #[serde(default)]
    pub session_id: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PostToolUseResponse {
    /// Modified tool result content. The host replaces its
    /// in-memory tool result with this string.
    pub content: String,
    /// Optional human-readable note for the user.
    pub note: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct UserPromptSubmitPayload {
    pub prompt: String,
    #[serde(default)]
    pub session_id: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum UserPromptSubmitResponse {
    Allow,
    Warn { remaining: usize },
    Slice { target_key: String, slice_to: usize },
    Compact { reason: String },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PreCompactPayload {
    pub history_turns: Vec<Turn>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Turn {
    pub index: usize,
    pub role: String,
    pub content_preview: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PreCompactResponse {
    pub hint: serde_json::Value,
}

pub fn parse_post_tool_use(stdin: &str) -> Result<PostToolUsePayload, String> {
    serde_json::from_str(stdin).map_err(|e| format!("canonical PostToolUse parse: {e}"))
}

pub fn format_post_tool_use(resp: &PostToolUseResponse) -> serde_json::Value {
    serde_json::json!({
        "content": resp.content,
        "note": resp.note,
    })
}

pub fn parse_user_prompt_submit(stdin: &str) -> Result<UserPromptSubmitPayload, String> {
    serde_json::from_str(stdin).map_err(|e| format!("canonical UserPromptSubmit parse: {e}"))
}

pub fn format_user_prompt_submit(resp: &UserPromptSubmitResponse) -> serde_json::Value {
    serde_json::to_value(resp).unwrap_or(serde_json::json!({"kind": "allow"}))
}

pub fn parse_pre_compact(stdin: &str) -> Result<PreCompactPayload, String> {
    serde_json::from_str(stdin).map_err(|e| format!("canonical PreCompact parse: {e}"))
}

pub fn format_pre_compact(resp: &PreCompactResponse) -> serde_json::Value {
    serde_json::json!({
        "hint": resp.hint,
        "summary": null,
    })
}

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
            "kf_budget_hosts::canonical::tests",
            "canonical module path drifted from ADR-0013 layout"
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
