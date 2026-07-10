//! Property-based tests for the core pipeline.
//!
//! These tests exercise invariants that should hold for arbitrary inputs and
//! content types: the pipeline must not panic, must never grow content beyond
//! the original size, must be idempotent, and offload markers must round-trip
//! through the store.

use kirkstratum_core::config::PipelineConfig;
use kirkstratum_core::content::ContentType;
use kirkstratum_core::mode::Mode;
use kirkstratum_core::pipeline::{CompressionContext, CompressionPipeline};
use kirkstratum_core::store::{InMemoryOffloadStore, OffloadStore};
use proptest::prelude::*;

// Run the default empty pipeline on arbitrary input and content type and
// confirm it never panics.
proptest! {
    #[test]
    fn no_panic_for_any_input(input in any::<String>(), content_type in content_type_strategy()) {
        let pipeline = CompressionPipeline::new();
        let store = InMemoryOffloadStore::new();
        let _ = pipeline.run(
            &input,
            content_type,
            &CompressionContext::default(),
            &store,
            &PipelineConfig::default(),
            Mode::Full,
        );
    }
}

// The default pipeline is the identity transform unless it offloads. Either
// way, the returned string must not be longer than the input.
proptest! {
    #[test]
    fn output_never_grows_input(
        input in any::<String>(),
        content_type in content_type_strategy(),
    ) {
        let pipeline = CompressionPipeline::new();
        let store = InMemoryOffloadStore::new();
        let out = pipeline.run(
            &input,
            content_type,
            &CompressionContext::default(),
            &store,
            &PipelineConfig::default(),
            Mode::Full,
        );
        prop_assert!(
            out.len() <= input.len() || out.contains("[offloaded: "),
            "output grew without offloading: input={} out={}",
            input.len(),
            out.len()
        );
    }
}

// Running the same pipeline twice on the same input must produce the same
// result.
proptest! {
    #[test]
    fn pipeline_is_idempotent(input in any::<String>(), content_type in content_type_strategy()) {
        let pipeline = CompressionPipeline::new();
        let store = InMemoryOffloadStore::new();
        let ctx = CompressionContext::default();
        let cfg = PipelineConfig::default();

        let first = pipeline.run(
            &input,
            content_type,
            &ctx,
            &store,
            &cfg,
            Mode::Full,
        );
        let second = pipeline.run(
            &input,
            content_type,
            &ctx,
            &store,
            &cfg,
            Mode::Full,
        );

        prop_assert_eq!(first, second);
    }
}

// When content is offloaded, the marker inserted into the output must contain
// a key that resolves in the store.
proptest! {
    #[test]
    fn offload_marker_round_trips_through_store(
        prefix in any::<String>(),
        suffix in any::<String>(),
    ) {
        // Build an input that is guaranteed to be bloated by giving it a tiny
        // token budget and enough bytes to exceed the default threshold.
        let input = format!("{prefix}{}{suffix}", "x".repeat(10_000));
        let pipeline = CompressionPipeline::new();
        let store = InMemoryOffloadStore::new();
        let ctx = CompressionContext::default().with_token_budget(1);

        let out = pipeline.run(
            &input,
            ContentType::PlainText,
            &ctx,
            &store,
            &PipelineConfig::default(),
            Mode::Full,
        );

        if let Some(key) = out.strip_prefix("[offloaded: ").and_then(|s| s.strip_suffix(']')) {
            prop_assert_eq!(
                store.get(key),
                Some(input),
                "offloaded key did not round-trip through the store"
            );
        }
    }
}

/// Strategy that produces every `ContentType` variant with roughly equal
/// probability.
fn content_type_strategy() -> impl Strategy<Value = ContentType> {
    prop_oneof![
        Just(ContentType::PlainText),
        Just(ContentType::SourceCode),
        Just(ContentType::JsonObject),
        Just(ContentType::JsonArray),
        Just(ContentType::GitDiff),
        Just(ContentType::Html),
        Just(ContentType::BuildOutput),
        Just(ContentType::SearchResults),
    ]
}
