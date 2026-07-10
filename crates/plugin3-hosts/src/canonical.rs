//! Canonical payload schemas — host-agnostic. Per ADR-0013.

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
