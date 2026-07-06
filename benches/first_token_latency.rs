use criterion::{black_box, criterion_group, criterion_main, Criterion};
use kirkforge::adapters::ollama_ndjson::{parse_ollama_ndjson_stream, OllamaNdjsonConfig};

/// Benchmark the latency from the start of NDJSON parsing to the first
/// `StreamEvent::Text` event. This isolates the adapter parser path without
/// needing a live Ollama server, so the benchmark is stable in CI and can
/// detect parser regressions (buffering, UTF-8 decoding, JSON parsing).
fn first_token_latency(c: &mut Criterion) {
    let line = b"{\"message\":{\"content\":\"hello\"},\"done\":false}\n".to_vec();

    c.bench_function("ollama_ndjson_first_token", |b| {
        b.to_async(tokio::runtime::Runtime::new().unwrap())
            .iter(|| async {
                let (tx, mut rx) = tokio::sync::mpsc::channel(16);
                let stream =
                    tokio_stream::iter(vec![Ok::<_, std::convert::Infallible>(line.clone())]);

                let parser = parse_ollama_ndjson_stream(tx, OllamaNdjsonConfig::GLM, stream);
                let receive = async {
                    let _first = rx.recv().await;
                };

                tokio::join!(parser, receive);
                black_box(())
            });
    });
}

criterion_group!(benches, first_token_latency);
criterion_main!(benches);
