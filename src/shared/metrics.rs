//! Operational metrics — append-only NDJSON event log + optional OTel export.
//!
//! Records lightweight, structured events for tool calls, verifier
//! verdicts, turn outcomes, and approval decisions. The log lives at
//! `~/.local/share/kirkforge/metrics.ndjson` and is designed to be
//! human-readable and trivial to query with standard shell tools.
//!
//! When the `otel` feature is enabled and `OTEL_EXPORTER_OTLP_ENDPOINT` is
//! set, each `MetricEvent` is also emitted as an OpenTelemetry span so the
//! runtime integrates with the existing TS `core-telemetry` pipeline.

use serde::{Deserialize, Serialize};
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::sync::Mutex;
#[cfg(test)]
use std::sync::MutexGuard;

#[cfg(feature = "otel")]
mod otel {
    use opentelemetry::trace::{Span, SpanKind, Tracer, TracerProvider};
    use opentelemetry::KeyValue;
    use opentelemetry_otlp::{MetricExporter, SpanExporter, WithExportConfig};
    use opentelemetry_sdk::metrics::{MeterProviderBuilder, PeriodicReader, SdkMeterProvider};
    use opentelemetry_sdk::trace::{Sampler, SdkTracerProvider};
    use opentelemetry_sdk::Resource;
    use std::sync::OnceLock;

    static TRACER_PROVIDER: OnceLock<SdkTracerProvider> = OnceLock::new();
    static METER_PROVIDER: OnceLock<SdkMeterProvider> = OnceLock::new();

    fn otlp_endpoint() -> Option<String> {
        std::env::var("OTEL_EXPORTER_OTLP_ENDPOINT")
            .ok()
            .filter(|s| !s.is_empty())
    }

    pub fn init_telemetry() -> Option<String> {
        let endpoint = otlp_endpoint()?;
        let resource = Resource::builder()
            .with_attribute(KeyValue::new("service.name", "kirkforge"))
            .with_attribute(KeyValue::new("service.version", env!("CARGO_PKG_VERSION")))
            .build();

        let span_exporter = SpanExporter::builder()
            .with_tonic()
            .with_endpoint(endpoint.clone())
            .build()
            .ok()?;
        let tracer_provider = SdkTracerProvider::builder()
            .with_resource(resource.clone())
            .with_batch_exporter(span_exporter)
            .with_sampler(Sampler::AlwaysOn)
            .build();
        let _ = TRACER_PROVIDER.set(tracer_provider.clone());
        opentelemetry::global::set_text_map_propagator(
            opentelemetry_sdk::propagation::TraceContextPropagator::new(),
        );

        let metric_exporter = MetricExporter::builder()
            .with_tonic()
            .with_endpoint(endpoint.clone())
            .build()
            .ok()?;
        let reader = PeriodicReader::builder(metric_exporter).build();
        let meter_provider = MeterProviderBuilder::default()
            .with_reader(reader)
            .with_resource(resource)
            .build();
        let _ = METER_PROVIDER.set(meter_provider.clone());

        opentelemetry::global::set_tracer_provider(tracer_provider.clone());
        opentelemetry::global::set_meter_provider(meter_provider);

        Some(endpoint)
    }

    pub fn emit_event_span(event_name: &str, attributes: &[KeyValue]) {
        if let Some(provider) = TRACER_PROVIDER.get() {
            let tracer = provider.tracer("kirkforge");
            let mut builder = tracer.span_builder(event_name.to_string());
            builder.span_kind = Some(SpanKind::Internal);
            let mut span = tracer.build(builder);
            span.set_attributes(attributes.iter().cloned());
            span.end();
        }
    }

    pub fn shutdown() {
        if let Some(p) = TRACER_PROVIDER.get() {
            let _ = p.shutdown();
        }
        if let Some(p) = METER_PROVIDER.get() {
            let _ = p.shutdown();
        }
    }
}

/// Maximum size of the active metrics log before rotation. Older logs are
/// moved to `metrics.ndjson.1`; earlier rotations are overwritten.
const MAX_LOG_BYTES: u64 = 5 * 1024 * 1024;

/// Global serialize-and-append lock.
///
/// `record()` opens, rotates, and writes to the same file from many
/// concurrent tasks. Without serialization, the content and newline of
/// two events can interleave, producing one line like
/// `{"event":"a"}{"event":"b"}\n` and a following blank line. The lock
/// keeps each event's full write atomic relative to other events.
static RECORD_LOCK: Mutex<()> = Mutex::new(());

/// A metric event. Serialized as one NDJSON line.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum MetricEvent {
    ToolCall {
        name: String,
        success: bool,
        duration_ms: u64,
        error_kind: Option<String>,
    },
    Verifier {
        name: String,
        verdict: String,
    },
    Turn {
        model: String,
        duration_ms: u64,
        tool_calls: usize,
        finish_reason: String,
    },
    Approval {
        action: String,
    },
}

impl MetricEvent {
    /// Human-readable category used by the summary command.
    pub fn category(&self) -> &'static str {
        match self {
            MetricEvent::ToolCall { .. } => "tool",
            MetricEvent::Verifier { .. } => "verifier",
            MetricEvent::Turn { .. } => "turn",
            MetricEvent::Approval { .. } => "approval",
        }
    }

    /// Convert the event into an OpenTelemetry span name and attribute set.
    #[cfg(feature = "otel")]
    fn to_otel_attrs(&self) -> (String, Vec<opentelemetry::KeyValue>) {
        let mut attrs = vec![
            opentelemetry::KeyValue::new("event.category", self.category()),
        ];
        let name = match self {
            MetricEvent::ToolCall {
                name,
                success,
                duration_ms,
                error_kind,
            } => {
                attrs.push(opentelemetry::KeyValue::new("tool.name", name.clone()));
                attrs.push(opentelemetry::KeyValue::new("tool.success", *success));
                attrs.push(opentelemetry::KeyValue::new(
                    "tool.duration_ms",
                    *duration_ms as i64,
                ));
                if let Some(ek) = error_kind {
                    attrs.push(opentelemetry::KeyValue::new("tool.error_kind", ek.clone()));
                }
                "tool.call".to_string()
            }
            MetricEvent::Verifier { name, verdict } => {
                attrs.push(opentelemetry::KeyValue::new("verifier.name", name.clone()));
                attrs.push(opentelemetry::KeyValue::new("verifier.verdict", verdict.clone()));
                "verifier.run".to_string()
            }
            MetricEvent::Turn {
                model,
                duration_ms,
                tool_calls,
                finish_reason,
            } => {
                attrs.push(opentelemetry::KeyValue::new("turn.model", model.clone()));
                attrs.push(opentelemetry::KeyValue::new(
                    "turn.duration_ms",
                    *duration_ms as i64,
                ));
                attrs.push(opentelemetry::KeyValue::new(
                    "turn.tool_calls",
                    *tool_calls as i64,
                ));
                attrs.push(opentelemetry::KeyValue::new(
                    "turn.finish_reason",
                    finish_reason.clone(),
                ));
                "turn.complete".to_string()
            }
            MetricEvent::Approval { action } => {
                attrs.push(opentelemetry::KeyValue::new("approval.action", action.clone()));
                "approval.decision".to_string()
            }
        };
        (name, attrs)
    }
}

// ponytail: thread-local test override for the metrics path. Thread-local
// (not a global Mutex) so an incidental `record()` from another test's
// thread (verifier/executor/approval) can't land in this test's override
// path. `#[cfg(test)]`-only; production builds compile the whole override
// block out, so this never affects real `metrics_path()` resolution.
#[cfg(test)]
thread_local! {
    static PATH_OVERRIDE: std::cell::RefCell<Option<PathBuf>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
static TEST_LOCK: Mutex<()> = Mutex::new(());

/// Unique counter so each `with_test_path` invocation gets its own temp
/// directory. Using only `process::id()` caused every test in the same process
/// to share one path, and a slow/interleaved test could see the directory
/// removed by a faster neighbour under heavy parallelism.
#[cfg(test)]
static TEST_DIR_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Resolve the metrics log path inside the platform data directory.
pub fn metrics_path() -> Option<PathBuf> {
    #[cfg(test)]
    {
        if let Some(path) = PATH_OVERRIDE.with(|o| o.borrow().clone()) {
            return Some(path);
        }
    }
    let dirs = directories::ProjectDirs::from("", "KirkForge", "kirkforge")?;
    let data = dirs.data_local_dir();
    std::fs::create_dir_all(data).ok()?;
    Some(data.join("metrics.ndjson"))
}

/// Open (or create) the metrics log at the resolved path.
fn open_metrics_file(path: &PathBuf) -> std::io::Result<File> {
    OpenOptions::new().create(true).append(true).open(path)
}

/// Rotate the metrics log if it has grown past [`MAX_LOG_BYTES`].
fn rotate_if_needed(path: &PathBuf) {
    let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    if size < MAX_LOG_BYTES {
        return;
    }
    let backup = path.with_extension("ndjson.1");
    if let Err(e) = std::fs::rename(path, &backup) {
        tracing::warn!(error = %e, "failed to rotate metrics log backup");
    }
}

/// Write a serialized event to the given log path under the global lock.
fn write_event(path: &PathBuf, event: &MetricEvent) {
    let line = match serde_json::to_string(event) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(error = %e, "failed to serialize metric event");
            return;
        }
    };

    // Serialize the whole event (content + newline) into one buffer and
    // guard the rotate/open/write sequence so concurrent records cannot
    // interleave content and newlines.
    let _guard = RECORD_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    rotate_if_needed(path);

    match open_metrics_file(path) {
        Ok(mut file) => {
            // Write the line with a single syscall to keep each record
            // atomic even if the lock were ever removed.
            let buf = format!("{line}\n");
            if let Err(e) = file.write_all(buf.as_bytes()) {
                tracing::warn!(error = %e, "failed to write metric event");
            }
        }
        Err(e) => {
            tracing::warn!(error = %e, path = %path.display(), "failed to open metrics file");
        }
    }
}

/// Record a metric event.
///
/// Events are appended synchronously; failures are logged via `tracing`
/// but never propagated, so a metrics write error cannot break a turn.
/// When the `otel` feature is enabled and `OTEL_EXPORTER_OTLP_ENDPOINT` is
/// set, a matching span is also exported.
pub fn record(event: MetricEvent) {
    let Some(path) = metrics_path() else {
        tracing::debug!("no metrics path available; dropping event");
        return;
    };
    write_event(&path, &event);

    #[cfg(feature = "otel")]
    {
        let (name, attrs) = event.to_otel_attrs();
        otel::emit_event_span(&name, &attrs);
    }
}

/// Summary statistics computed from the metrics log.
#[derive(Debug, Default, Clone)]
pub struct MetricsSummary {
    pub tool_calls: usize,
    pub tool_success: usize,
    pub tool_failure: usize,
    pub verifier_clean: usize,
    pub verifier_fixable: usize,
    pub verifier_unfixable: usize,
    pub verifier_skipped: usize,
    pub turns: usize,
    pub total_turn_duration_ms: u64,
    pub approvals_allow: usize,
    pub approvals_ask: usize,
    pub approvals_deny: usize,
    pub approvals_always: usize,
}

impl MetricsSummary {
    pub fn avg_turn_duration_ms(&self) -> u64 {
        if self.turns == 0 {
            0
        } else {
            self.total_turn_duration_ms / self.turns as u64
        }
    }
}

/// Read all events from the metrics log.
pub fn read_events() -> Vec<MetricEvent> {
    let Some(path) = metrics_path() else {
        return Vec::new();
    };
    let file = match File::open(&path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
        Err(e) => {
            tracing::warn!(error = %e, path = %path.display(), "failed to read metrics log");
            return Vec::new();
        }
    };
    let reader = BufReader::new(file);
    let mut events = Vec::new();
    for line in reader.lines() {
        let line = match line {
            Ok(l) => l,
            Err(e) => {
                tracing::warn!(error = %e, "failed to read metrics line");
                continue;
            }
        };
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<MetricEvent>(&line) {
            Ok(e) => events.push(e),
            Err(e) => {
                tracing::warn!(error = %e, line = %line, "failed to parse metrics line");
            }
        }
    }
    events
}

/// Summarize the metrics log.
pub fn summarize() -> MetricsSummary {
    let mut summary = MetricsSummary::default();
    for event in read_events() {
        match event {
            MetricEvent::ToolCall { success, .. } => {
                summary.tool_calls += 1;
                if success {
                    summary.tool_success += 1;
                } else {
                    summary.tool_failure += 1;
                }
            }
            MetricEvent::Verifier { verdict, .. } => match verdict.as_str() {
                "clean" => summary.verifier_clean += 1,
                "fixable" => summary.verifier_fixable += 1,
                "unfixable" => summary.verifier_unfixable += 1,
                _ => summary.verifier_skipped += 1,
            },
            MetricEvent::Turn {
                duration_ms,
                tool_calls,
                ..
            } => {
                summary.turns += 1;
                summary.total_turn_duration_ms += duration_ms;
                summary.tool_calls += tool_calls;
            }
            MetricEvent::Approval { action, .. } => match action.as_str() {
                "approved" => summary.approvals_allow += 1,
                "denied" => summary.approvals_deny += 1,
                "always_approved" => summary.approvals_always += 1,
                _ => {}
            },
        }
    }
    summary
}

/// Initialize OpenTelemetry export when the `otel` feature is enabled and
/// `OTEL_EXPORTER_OTLP_ENDPOINT` is configured. Returns the endpoint on
/// success, `None` when disabled or not configured.
pub fn init_telemetry() -> Option<String> {
    #[cfg(feature = "otel")]
    {
        otel::init_telemetry()
    }
    #[cfg(not(feature = "otel"))]
    {
        None
    }
}

/// Gracefully shut down OpenTelemetry exporters.
pub fn shutdown_telemetry() {
    #[cfg(feature = "otel")]
    {
        otel::shutdown();
    }
}

/// Format a summary as user-facing text.
pub fn format_summary(summary: &MetricsSummary) -> String {
    let mut lines = Vec::new();
    lines.push("Metrics summary".to_string());
    lines.push(format!("  turns:          {}", summary.turns));
    lines.push(format!(
        "  avg turn time:  {} ms",
        summary.avg_turn_duration_ms()
    ));
    lines.push(format!(
        "  tool calls:     {} ({} ok, {} failed)",
        summary.tool_calls, summary.tool_success, summary.tool_failure
    ));
    lines.push(format!(
        "  verifiers:      {} clean / {} fixable / {} unfixable / {} skipped",
        summary.verifier_clean,
        summary.verifier_fixable,
        summary.verifier_unfixable,
        summary.verifier_skipped
    ));
    lines.push(format!(
        "  approvals:      {} approved / {} denied / {} always-approved",
        summary.approvals_allow, summary.approvals_deny, summary.approvals_always
    ));
    lines.join("\n")
}

#[cfg(test)]
pub(crate) fn with_test_path<F, R>(f: F) -> R
where
    F: FnOnce(PathBuf, MutexGuard<'static, ()>) -> R,
{
    let lock = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let counter = TEST_DIR_COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!(
        "kirkforge_metrics_test_{}_{}",
        std::process::id(),
        counter
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("metrics.ndjson");
    PATH_OVERRIDE.with(|o| *o.borrow_mut() = Some(path.clone()));
    let result = f(dir.clone(), lock);
    PATH_OVERRIDE.with(|o| *o.borrow_mut() = None);
    let _ = std::fs::remove_dir_all(&dir);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_record_and_read_round_trip() {
        with_test_path(|_dir, _lock| {
            record(MetricEvent::ToolCall {
                name: "read_file".into(),
                success: true,
                duration_ms: 12,
                error_kind: None,
            });
            record(MetricEvent::Approval {
                action: "approved".into(),
            });

            let events = read_events();
            assert_eq!(events.len(), 2);
            assert!(matches!(events[0], MetricEvent::ToolCall { .. }));
            assert!(matches!(events[1], MetricEvent::Approval { .. }));
        });
    }

    #[test]
    fn test_summarize_counts() {
        with_test_path(|_dir, _lock| {
            record(MetricEvent::ToolCall {
                name: "bash".into(),
                success: false,
                duration_ms: 100,
                error_kind: Some("execution".into()),
            });
            record(MetricEvent::Verifier {
                name: "lint".into(),
                verdict: "fixable".into(),
            });
            record(MetricEvent::Turn {
                model: "kimi-2.7k-coder:cloud".into(),
                duration_ms: 2500,
                tool_calls: 1,
                finish_reason: "stop".into(),
            });

            let summary = summarize();
            assert_eq!(summary.tool_failure, 1);
            assert_eq!(summary.verifier_fixable, 1);
            assert_eq!(summary.turns, 1);
            assert_eq!(summary.avg_turn_duration_ms(), 2500);
        });
    }

    #[test]
    fn test_rotation_replaces_old_log() {
        with_test_path(|_dir, _lock| {
            let path = metrics_path().unwrap();
            // Pre-seed an oversized log.
            let big = "a".repeat(MAX_LOG_BYTES as usize + 100);
            std::fs::write(&path, big).unwrap();

            record(MetricEvent::Approval {
                action: "denied".into(),
            });

            let events = read_events();
            assert_eq!(events.len(), 1);
            assert!(matches!(
                events[0],
                MetricEvent::Approval { ref action } if action == "denied"
            ));
        });
    }

    #[test]
    fn test_concurrent_records_are_not_interleaved() {
        with_test_path(|_dir, _lock| {
            let path = metrics_path().unwrap();
            let mut handles = Vec::new();
            for i in 0..100 {
                let p = path.clone();
                handles.push(std::thread::spawn(move || {
                    write_event(
                        &p,
                        &MetricEvent::ToolCall {
                            name: format!("tool-{i}"),
                            success: true,
                            duration_ms: i as u64,
                            error_kind: None,
                        },
                    );
                }));
            }
            for h in handles {
                h.join().unwrap();
            }

            let events = read_events();

            // The 100 writes go through `write_event` directly (not
            // `record()`), so they target `path` regardless of the global
            // PATH_OVERRIDE. But `read_events()` resolves via PATH_OVERRIDE,
            // and production `record()` calls in OTHER tests (verifier /
            // executor / approval) also resolve via that same global — so
            // under parallel test execution a `record()` from another test
            // can land in this file as an extra, well-formed event. That is
            // a cross-test isolation artefact, not a write-interleaving
            // failure. The invariant we actually care about is that our 100
            // concurrent writes all survived intact: interleaving that
            // merged two events into one line would make serde reject it
            // and drop the line, so the `tool-N` name would go missing.
            // Assert the exact set of names is present rather than the raw
            // line count, which would flake on a contaminating write.
            use std::collections::HashSet;
            let ours: HashSet<String> = events
                .iter()
                .filter_map(|e| match e {
                    MetricEvent::ToolCall { name, .. } if name.starts_with("tool-") => {
                        Some(name.clone())
                    }
                    _ => None,
                })
                .collect();
            let expected: HashSet<String> = (0u64..100).map(|i| format!("tool-{i}")).collect();
            assert_eq!(
                ours, expected,
                "all 100 concurrent writes must be present and intact; \
                 extra events from other tests' record() are tolerated"
            );
        });
    }

    #[test]
    fn test_format_summary_output() {
        let summary = MetricsSummary {
            turns: 10,
            total_turn_duration_ms: 5000,
            tool_calls: 20,
            tool_success: 18,
            tool_failure: 2,
            verifier_clean: 5,
            verifier_fixable: 3,
            verifier_unfixable: 1,
            verifier_skipped: 1,
            approvals_allow: 4,
            approvals_ask: 1,
            approvals_deny: 1,
            approvals_always: 2,
        };
        let text = format_summary(&summary);
        assert!(text.contains("turns:          10"));
        assert!(text.contains("tool calls:     20"));
        assert!(text.contains("avg turn time:  500 ms"));
    }

    #[cfg(feature = "otel")]
    #[test]
    fn test_otel_attrs_from_tool_call() {
        let event = MetricEvent::ToolCall {
            name: "read_file".into(),
            success: true,
            duration_ms: 12,
            error_kind: None,
        };
        let (name, attrs) = event.to_otel_attrs();
        assert_eq!(name, "tool.call");
        assert!(attrs.iter().any(|kv| {
            kv.key.as_str() == "tool.name" && kv.value.as_str().as_ref() == "read_file"
        }));
        assert!(attrs.iter().any(|kv| kv.key.as_str() == "tool.success"));
    }

    #[cfg(feature = "otel")]
    #[test]
    fn test_otel_init_returns_none_without_endpoint() {
        // Ensure no endpoint is set for this test.
        std::env::remove_var("OTEL_EXPORTER_OTLP_ENDPOINT");
        assert!(init_telemetry().is_none());
    }
}
