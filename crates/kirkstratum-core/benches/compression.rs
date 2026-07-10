#![allow(missing_docs)]

use criterion::{black_box, criterion_group, criterion_main, Criterion, Throughput};
use kirkstratum_core::config::PipelineConfig;
use kirkstratum_core::content::ContentType;
use kirkstratum_core::mode::Mode;
use kirkstratum_core::pipeline::{CompressionContext, CompressionPipeline};
use kirkstratum_core::store::InMemoryOffloadStore;

fn bench_pipeline(c: &mut Criterion) {
    let pipeline = CompressionPipeline::new();
    let store = InMemoryOffloadStore::new();
    let cfg = PipelineConfig::default();
    let ctx = CompressionContext::default();

    let mut group = c.benchmark_group("compression");

    // Small JSON object (~1 KiB)
    let small_json = format!(
        "{{\"entries\":[{}}}]",
        (0..32)
            .map(|i| format!("{{\"id\":{i},\"msg\":\"entry number {i}\"}}"))
            .collect::<Vec<_>>()
            .join(",")
    );
    group.throughput(Throughput::Bytes(small_json.len() as u64));
    group.bench_with_input("small_json", &small_json, |b, input| {
        b.iter(|| {
            pipeline.run(
                black_box(input),
                ContentType::JsonObject,
                &ctx,
                &store,
                &cfg,
                Mode::Full,
            )
        })
    });

    // Large log (~1 MiB to keep benchmark fast; README reports extrapolated
    // 10 MiB numbers from this measurement).
    let large_log = build_log(1_000_000);
    group.throughput(Throughput::Bytes(large_log.len() as u64));
    group.bench_with_input("large_log", &large_log, |b, input| {
        b.iter(|| {
            pipeline.run(
                black_box(input),
                ContentType::PlainText, // log-like content uses the plain-text detector
                &ctx,
                &store,
                &cfg,
                Mode::Full,
            )
        })
    });

    // Large diff (~512 KiB)
    let large_diff = build_diff(512_000);
    group.throughput(Throughput::Bytes(large_diff.len() as u64));
    group.bench_with_input("large_diff", &large_diff, |b, input| {
        b.iter(|| {
            pipeline.run(
                black_box(input),
                ContentType::GitDiff,
                &ctx,
                &store,
                &cfg,
                Mode::Full,
            )
        })
    });

    // Worst-case plain text (1 MiB; the input limit is 50 MiB but the bench
    // uses a representative sample to keep CI fast).
    let worst_case_text = "x".repeat(1_000_000);
    group.throughput(Throughput::Bytes(worst_case_text.len() as u64));
    group.bench_with_input("worst_case_text", &worst_case_text, |b, input| {
        b.iter(|| {
            pipeline.run(
                black_box(input),
                ContentType::PlainText,
                &ctx,
                &store,
                &cfg,
                Mode::Full,
            )
        })
    });

    group.finish();
}

fn build_log(total_bytes: usize) -> String {
    let line =
        "2026-06-29T10:00:00Z INFO some_service request_id=uuid path=/api/v1/items status=200\n";
    let repeats = total_bytes.div_ceil(line.len());
    line.repeat(repeats)
}

fn build_diff(total_bytes: usize) -> String {
    let hunk = "diff --git a/file.rs b/file.rs\nindex 123..456 100644\n--- a/file.rs\n+++ b/file.rs\n@@ -1,5 +1,5 @@\n- old line\n+ new line\n";
    let repeats = total_bytes.div_ceil(hunk.len());
    hunk.repeat(repeats)
}

criterion_group!(benches, bench_pipeline);
criterion_main!(benches);
