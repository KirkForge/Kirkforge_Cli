//! Tests for M5 (review gaps #10 multimodal, #11 cache, #12 JSON mode).
//!
//! All tests are pure: no HTTP, no executor, no TUI. They exercise
//! the body builders and the round-trip serialization of the new
//! types. The end-to-end "image attached to next user turn" path
//! is covered by `attach_pending_image_splices_image_onto_user_message`
//! at the bottom of this file — that's the only test that touches
//! the prompt builder.
use crate::adapters::{build_ollama_chat_body, build_openai_compat_body};
use crate::shared::{ContentPart, Message, ModelInfo, Role, TokenUsage, ToolCallStyle, ToolDef};
use serde_json::json;

fn dummy_model_info() -> ModelInfo {
    ModelInfo {
        name: "test-model".into(),
        supports_thinking: false,
        tool_call_format: ToolCallStyle::OpenAiCompat,
        max_context_tokens: 8192,
        recommended_temperature: 0.7,
        supports_images: false,
        supports_cache: false,
    }
}

fn user_text(text: &str) -> Message {
    Message {
        role: Role::User,
        content: text.into(),
        ..Default::default()
    }
}

fn user_with_parts(parts: Vec<ContentPart>) -> Message {
    Message {
        role: Role::User,
        content: String::new(),
        content_parts: Some(parts),
        ..Default::default()
    }
}

fn tool_image() -> Message {
    Message {
        role: Role::Tool,
        content: "[image: shot.png (image/png, 12 bytes)]".into(),
        content_parts: Some(vec![ContentPart::Image {
            data_base64: "BASE64DATA".into(),
            mime: "image/png".into(),
        }]),
        tool_call_id: Some("call_1".into()),
        tool_name: Some("read_image".into()),
        ..Default::default()
    }
}

// ── ContentPart serde round-trip ───────────────────────────────────

#[test]
fn content_part_text_round_trips() {
    let p = ContentPart::Text {
        text: "hello".into(),
    };
    let j = serde_json::to_value(&p).unwrap();
    assert_eq!(j, json!({"type": "text", "text": "hello"}));
    let back: ContentPart = serde_json::from_value(j).unwrap();
    assert_eq!(back, p);
}

#[test]
fn content_part_image_round_trips() {
    let p = ContentPart::Image {
        data_base64: "AAAA".into(),
        mime: "image/png".into(),
    };
    let j = serde_json::to_value(&p).unwrap();
    assert_eq!(
        j,
        json!({"type": "image", "data_base64": "AAAA", "mime": "image/png"})
    );
    let back: ContentPart = serde_json::from_value(j).unwrap();
    assert_eq!(back, p);
}

#[test]
fn message_with_content_parts_serializes_compactly() {
    // The Option<…> field is skipped when None, so a text-only
    // message stays as `{role, content}` — backward compat with
    // pre-M5 NDJSON logs.
    let m = user_text("hi");
    let j = serde_json::to_value(&m).unwrap();
    assert_eq!(j, json!({"role": "user", "content": "hi"}));

    // A multimodal message serializes with `content_parts` present.
    let m = user_with_parts(vec![ContentPart::Text {
        text: "what is this?".into(),
    }]);
    let j = serde_json::to_value(&m).unwrap();
    assert!(j.get("content_parts").is_some());
}

// ── OpenAI-compat body builder ──────────────────────────────────────

fn oai_body(msgs: &[Message], json_mode: bool) -> serde_json::Value {
    let mut mi = dummy_model_info();
    mi.supports_cache = false; // tests focus on multimodal + json_mode
    let tools: Vec<ToolDef> = vec![ToolDef {
        name: "read_image",
        description: "x",
        parameters: json!({"type": "object", "properties": {}}),
    }];
    build_openai_compat_body("test-model", &mi, msgs, &tools, json_mode)
}

#[test]
fn openai_text_only_message_uses_string_content() {
    let body = oai_body(&[user_text("hi")], false);
    let msg = &body["messages"][0];
    assert!(msg["content"].is_string(), "got: {msg}");
    assert_eq!(msg["content"], "hi");
}

#[test]
fn openai_multimodal_message_uses_vision_array() {
    let m = user_with_parts(vec![
        ContentPart::Text {
            text: "what is this?".into(),
        },
        ContentPart::Image {
            data_base64: "BASE64".into(),
            mime: "image/png".into(),
        },
    ]);
    let body = oai_body(&[m], false);
    let content = &body["messages"][0]["content"];
    assert!(content.is_array(), "expected vision array, got: {content}");
    let arr = content.as_array().unwrap();
    assert_eq!(arr.len(), 2);
    assert_eq!(arr[0], json!({"type": "text", "text": "what is this?"}));
    assert_eq!(
        arr[1],
        json!({
            "type": "image_url",
            "image_url": {"url": "data:image/png;base64,BASE64"}
        })
    );
}

#[test]
fn openai_json_mode_adds_response_format_and_tool_choice() {
    let body = oai_body(&[user_text("hi")], true);
    assert_eq!(body["response_format"], json!({"type": "json_object"}));
    // tools list is non-empty in oai_body, so tool_choice is set
    assert_eq!(body["tool_choice"], "auto");
}

#[test]
fn openai_json_mode_off_omits_response_format() {
    let body = oai_body(&[user_text("hi")], false);
    assert!(body.get("response_format").is_none());
    assert!(body.get("tool_choice").is_none());
}

#[test]
fn openai_cache_mode_marks_last_two_prefix_messages() {
    // Three prefix messages + a trailing user turn. The trailing
    // turn is NOT marked (changes every turn); the last 2 of the
    // prefix ARE marked with cache_control.
    let mut mi = dummy_model_info();
    mi.supports_cache = true;
    let tools: Vec<ToolDef> = vec![];
    let msgs = vec![
        Message {
            role: Role::System,
            content: "system".into(),
            ..Default::default()
        },
        Message {
            role: Role::Assistant,
            content: "first".into(),
            ..Default::default()
        },
        Message {
            role: Role::Assistant,
            content: "second".into(),
            ..Default::default()
        },
        user_text("third (user)"),
    ];
    let body = build_openai_compat_body("m", &mi, &msgs, &tools, false);
    let oai_msgs = body["messages"].as_array().unwrap();
    // System message (idx 0): no marker (only the last 2 of the
    // prefix are marked, and the system is the head of the prefix
    // — out of the window).
    assert!(oai_msgs[0].get("cache_control").is_none());
    // First assistant (idx 1): MARKER (last 2 of the prefix, with
    // prefix = [0, 1, 2])
    assert_eq!(oai_msgs[1]["cache_control"], json!({"type": "ephemeral"}));
    // Second assistant (idx 2): MARKER (last 2 of the prefix)
    assert_eq!(oai_msgs[2]["cache_control"], json!({"type": "ephemeral"}));
    // Trailing user (idx 3): no marker — it's the live turn
    assert!(oai_msgs[3].get("cache_control").is_none());
}

#[test]
fn openai_cache_mode_off_omits_cache_control() {
    let mut mi = dummy_model_info();
    mi.supports_cache = false;
    let tools: Vec<ToolDef> = vec![];
    let msgs = vec![user_text("a"), user_text("b"), user_text("c")];
    let body = build_openai_compat_body("m", &mi, &msgs, &tools, false);
    for m in body["messages"].as_array().unwrap() {
        assert!(m.get("cache_control").is_none());
    }
}

// ── Ollama body builder ────────────────────────────────────────────

fn ollama_body(msgs: &[Message], json_mode: bool) -> serde_json::Value {
    let mi = dummy_model_info();
    let tools: Vec<ToolDef> = vec![];
    build_ollama_chat_body("test-model", &mi, msgs, &tools, true, json_mode)
}

#[test]
fn ollama_text_only_message_omits_images_field() {
    let body = ollama_body(&[user_text("hi")], false);
    let m = &body["messages"][0];
    assert_eq!(m["content"], "hi");
    assert!(m.get("images").is_none());
}

#[test]
fn ollama_multimodal_message_emits_images_array() {
    let m = user_with_parts(vec![
        ContentPart::Text {
            text: "what?".into(),
        },
        ContentPart::Image {
            data_base64: "BASE64".into(),
            mime: "image/png".into(),
        },
    ]);
    let body = ollama_body(&[m], false);
    let msg = &body["messages"][0];
    assert_eq!(
        msg["images"],
        json!(["BASE64"]),
        "expected images array of base64 string, got: {msg}"
    );
    // Text projection concatenates the text parts followed by a
    // [image] marker (the model that ignores the `images` field
    // still sees a hint that an attachment is present).
    assert_eq!(msg["content"], "what?\n[image]");
}

#[test]
fn ollama_json_mode_adds_format_field() {
    let body = ollama_body(&[user_text("hi")], true);
    assert_eq!(body["format"], "json");
}

#[test]
fn ollama_json_mode_off_omits_format_field() {
    let body = ollama_body(&[user_text("hi")], false);
    assert!(body.get("format").is_none());
}

// ── TokenUsage + calculate_cost ────────────────────────────────────

#[test]
fn calculate_cost_no_cache_matches_legacy_formula() {
    // With cached_tokens = None, the discount path is a no-op and
    // the cost equals the original input*input_rate + output*output_rate.
    let usage = TokenUsage {
        prompt_tokens: Some(1_000_000),
        completion_tokens: Some(1_000_000),
        cached_tokens: None,
    };
    // First row in PRICING_TABLE is "opus-4" with input 15.00,
    // output 75.00. 1M tokens of each = 15.0 + 75.0 = 90.0.
    let cost = crate::shared::calculate_cost("opus-4-anything", &usage);
    assert!((cost - 90.0).abs() < 0.001, "got: {cost}");
}

#[test]
fn calculate_cost_cached_tokens_apply_discount() {
    // opus-4 cache rates: input 15.00, cache_read 1.50. 1M cached
    // tokens should bill at 1.50, not 15.00. 1M completion at 75.00.
    // Total: 1.50 + 75.00 = 76.50.
    let usage = TokenUsage {
        prompt_tokens: Some(1_000_000),
        completion_tokens: Some(1_000_000),
        cached_tokens: Some(1_000_000),
    };
    let cost = crate::shared::calculate_cost("opus-4-anything", &usage);
    assert!(
        (cost - 76.50).abs() < 0.001,
        "expected 76.50 (1.5 cache + 75 output), got: {cost}"
    );
}

#[test]
fn calculate_cost_capped_at_prompt() {
    // A misbehaving server might report cached_tokens > prompt_tokens.
    // The cost function must clamp cached at prompt, not bill
    // negative "fresh" input.
    let usage = TokenUsage {
        prompt_tokens: Some(100),
        completion_tokens: Some(50),
        cached_tokens: Some(500),
    };
    let cost = crate::shared::calculate_cost("opus-4-x", &usage);
    // 100 cached @ 1.50/M + 0 fresh + 50 completion @ 75.00/M
    // = 0.00015 + 0 + 0.00375 = 0.00390
    assert!((cost - 0.0039).abs() < 0.0001, "got: {cost}");
}

// ── PromptBuilder::attach_pending_image ────────────────────────────

#[test]
fn attach_pending_image_splices_image_onto_user_message() {
    use crate::session::prompt::PromptBuilder;

    let system = Message {
        role: Role::System,
        content: "sys".into(),
        ..Default::default()
    };
    let history = vec![
        Message {
            role: Role::Assistant,
            content: "I'll look at the screenshot.".into(),
            ..Default::default()
        },
        tool_image(), // ← read_image tool result
        user_text("What does the error say?"),
    ];
    let mut pb = PromptBuilder::new();
    let messages = pb.build_messages(system, &history, 32_000, &[]);

    // The user message should now have a `content_parts` field
    // with the image prepended + a text part containing the original
    // user input.
    let last = messages.last().expect("non-empty");
    assert_eq!(last.role, Role::User);
    let parts = last
        .content_parts
        .as_ref()
        .expect("image should be attached to the user message");
    assert_eq!(parts.len(), 2);
    assert!(matches!(&parts[0], ContentPart::Image { mime, .. } if mime == "image/png"));
    assert!(matches!(&parts[1], ContentPart::Text { text } if text == "What does the error say?"));
}

#[test]
fn attach_pending_image_does_nothing_when_no_read_image_in_history() {
    use crate::session::prompt::PromptBuilder;
    let system = Message {
        role: Role::System,
        content: "sys".into(),
        ..Default::default()
    };
    let history = vec![
        Message {
            role: Role::Assistant,
            content: "ok".into(),
            ..Default::default()
        },
        user_text("hi"),
    ];
    let mut pb = PromptBuilder::new();
    let messages = pb.build_messages(system, &history, 32_000, &[]);
    let last = messages.last().unwrap();
    assert!(last.content_parts.is_none(), "no read_image → no splice");
}

#[test]
fn attach_pending_image_preserves_existing_user_parts() {
    use crate::session::prompt::PromptBuilder;
    let system = Message {
        role: Role::System,
        content: "sys".into(),
        ..Default::default()
    };
    let history = vec![
        tool_image(),
        // User already has parts set (e.g. another image or a
        // pre-formatted text part). Splice should PREPEND, not
        // replace.
        user_with_parts(vec![ContentPart::Text {
            text: "first message".into(),
        }]),
    ];
    let mut pb = PromptBuilder::new();
    let messages = pb.build_messages(system, &history, 32_000, &[]);
    let last = messages.last().unwrap();
    let parts = last.content_parts.as_ref().unwrap();
    assert_eq!(parts.len(), 2);
    assert!(matches!(&parts[0], ContentPart::Image { .. }));
    assert!(matches!(&parts[1], ContentPart::Text { text } if text == "first message"));
}
