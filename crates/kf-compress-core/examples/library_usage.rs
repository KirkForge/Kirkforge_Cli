//! Library usage example for `kf-compress-core`.
//!
//! Run with:
//!
//! ```bash
//! cargo run -p kf-compress-core --example library_usage
//! ```

use kf_compress_core::config::PipelineConfig;
use kf_compress_core::content::ContentType;
use kf_compress_core::mode::Mode;
use kf_compress_core::pipeline::{CompressionContext, CompressionPipeline};
use kf_compress_core::store::InMemoryOffloadStore;

fn main() {
    let pipeline = CompressionPipeline::new();
    let ctx = CompressionContext::default().with_token_budget(1024);
    let store = InMemoryOffloadStore::new();
    let cfg = PipelineConfig::default();

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

    let big = "x".repeat(10_000);
    let out = pipeline.run(&big, ContentType::PlainText, &ctx, &store, &cfg, Mode::Full);
    println!("big: {out}");
    println!("stored payloads: {len}", len = store.len());
}
