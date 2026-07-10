//! Negative-corpus tests.
//!
//! These tests feed malformed or pathological inputs through the pipeline and
//! assert that the process never panics and never returns a result larger than
//! the original input (unless it is legitimately offloaded).

use kirkstratum_core::config::PipelineConfig;
use kirkstratum_core::content::ContentType;
use kirkstratum_core::mode::Mode;
use kirkstratum_core::pipeline::{CompressionContext, CompressionPipeline};
use kirkstratum_core::store::InMemoryOffloadStore;

fn run(content_type: ContentType, input: &str) -> String {
    let pipeline = CompressionPipeline::new();
    let store = InMemoryOffloadStore::new();
    pipeline.run(
        input,
        content_type,
        &CompressionContext::default(),
        &store,
        &PipelineConfig::default(),
        Mode::Full,
    )
}

#[test]
fn truncated_json_object_does_not_panic() {
    let out = run(ContentType::JsonObject, "{\"a\":\"b");
    assert!(out.len() <= 8 || out.starts_with("[offloaded: "));
}

#[test]
fn truncated_json_array_does_not_panic() {
    let out = run(ContentType::JsonArray, "[{\"a\":\"b\"");
    assert!(out.len() <= 12 || out.starts_with("[offloaded: "));
}

#[test]
fn invalid_utf8_replaced_by_lossy_representation() {
    // `String::from_utf8_lossy` is the caller's responsibility; the core
    // pipeline works on `str`. This test exercises a content string that
    // contains the Unicode replacement character, which detection must treat
    // as plain text without panicking.
    let input = "\u{FFFD}\u{FFFD}\u{FFFD}";
    let out = run(ContentType::PlainText, input);
    assert!(out.len() <= input.len() || out.starts_with("[offloaded: "));
}

#[test]
fn corrupted_diff_missing_headers_does_not_panic() {
    let input = "@@ -1,5 +1,5 @@\n- old\n+ new\n";
    let out = run(ContentType::GitDiff, input);
    assert!(out.len() <= input.len() || out.starts_with("[offloaded: "));
}

#[test]
fn deeply_nested_braces_does_not_panic() {
    let depth = 10_000;
    let input = "{".repeat(depth) + "x" + &"}".repeat(depth);
    let out = run(ContentType::JsonObject, &input);
    assert!(out.len() <= input.len() || out.starts_with("[offloaded: "));
}

#[test]
fn deeply_nested_brackets_does_not_panic() {
    let depth = 10_000;
    let input = "[".repeat(depth) + "x" + &"]".repeat(depth);
    let out = run(ContentType::JsonArray, &input);
    assert!(out.len() <= input.len() || out.starts_with("[offloaded: "));
}

#[test]
fn long_single_line_plain_text_does_not_panic() {
    let input = "x".repeat(1_000_000);
    let out = run(ContentType::PlainText, &input);
    assert!(out.len() <= input.len() || out.starts_with("[offloaded: "));
}
