//! Library usage example for `kirkstratum-core`.
//!
//! Run with:
//!
//! ```bash
//! cargo run -p kirkstratum-core --example library_usage
//! ```

use kirkstratum_core::config::PipelineConfig;
use kirkstratum_core::content::ContentType;
use kirkstratum_core::mode::Mode;
use kirkstratum_core::pipeline::{CompressionContext, CompressionPipeline};
use kirkstratum_core::store::InMemoryOffloadStore;

fn main() {
    // Build a pipeline and register a content transform.
    let mut pipeline = CompressionPipeline::new();
    pipeline.register_content_transform(|s| {
        // A trivial lossless transform: trim trailing whitespace.
        s.trim_end().to_string()
    });
    pipeline.register_output_transform(|s| {
        // Wrap the final output so it is easy to spot in logs.
        format!("<stratum>{s}</stratum>")
    });

    // Set up the per-invocation context and an in-memory offload store.
    let ctx = CompressionContext::default().with_token_budget(1024);
    let store = InMemoryOffloadStore::new();
    let cfg = PipelineConfig::default();

    // A small payload stays in-band...
    let small = "hello world\n\n\n";
    let out = pipeline.run(
        small,
        ContentType::PlainText,
        &ctx,
        &store,
        &cfg,
        Mode::Full,
    );
    println!("small: {out}");

    // ...while a bloated payload is offloaded and replaced by a reference.
    let big = "x".repeat(10_000);
    let out = pipeline.run(&big, ContentType::PlainText, &ctx, &store, &cfg, Mode::Full);
    println!("big: {out}");
    println!("stored payloads: {len}", len = store.len());
}
