//! EventSink → EventBus bridge (WO 36.6, folded into WO 36.5 step 3).
//!
//! kf-orchestrator emits `ArtifactEvent`s through its `EventSink` trait;
//! in the binary they land on the shared `EventBus` so artifact.* joins
//! the same stream as every other session event instead of vanishing
//! into `NullSink`. The shapes line up cheaply: `kind`, `stream_id`,
//! `timestamp`, and `value` carry over; `task_id` folds into the value
//! (the bus `Event` has no task field); a per-sink monotonic `sequence`
//! keeps the idempotency key unique per emit.

use crate::shared::event_bus::{Event, EventBus};
use async_trait::async_trait;
use kf_orchestrator::sink::{ArtifactEvent, EventSink};
use serde_json::Value;
use std::sync::atomic::{AtomicU64, Ordering};

pub struct EventBusSink {
    bus: EventBus,
    sequence: AtomicU64,
}

impl EventBusSink {
    pub fn new(bus: EventBus) -> Self {
        Self {
            bus,
            sequence: AtomicU64::new(0),
        }
    }
}

// task_id rides beside the artifact payload; the flush_signals values are
// always objects, the wrap arm only guards a foreign producer.
fn fold_task_id(value: Value, task_id: String) -> Value {
    match value {
        Value::Object(mut obj) => {
            obj.insert("taskId".into(), Value::String(task_id));
            Value::Object(obj)
        }
        other => serde_json::json!({ "taskId": task_id, "payload": other }),
    }
}

#[async_trait]
impl EventSink for EventBusSink {
    async fn emit(&self, event: ArtifactEvent) {
        let kind = event.kind();
        let (task_id, stream_id, timestamp, value) = match event {
            ArtifactEvent::Emitted {
                task_id,
                stream_id,
                timestamp,
                value,
            }
            | ArtifactEvent::Blocked {
                task_id,
                stream_id,
                timestamp,
                value,
            }
            | ArtifactEvent::Unterminated {
                task_id,
                stream_id,
                timestamp,
                value,
            }
            | ArtifactEvent::Truncated {
                task_id,
                stream_id,
                timestamp,
                value,
            } => (task_id, stream_id, timestamp, value),
        };
        let bus_event = Event {
            kind: kind.to_string(),
            schema_version: "v1".into(),
            sequence: self.sequence.fetch_add(1, Ordering::Relaxed),
            stream_id,
            timestamp,
            value: Some(fold_task_id(value, task_id)),
        };
        // A dropped event would resurrect the silent sink this bridge
        // exists to kill — surface emit failures in the session log.
        if let Err(e) = self.bus.emit(bus_event).await {
            tracing::warn!(?e, kind, "artifact event dropped: bus not accepting emits");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::Mutex;

    // Event-driven: bus.emit awaits every handler before returning, so
    // once sink.emit resolves the assertions see the delivered events —
    // no sleeps anywhere in this module.
    #[tokio::test]
    async fn all_artifact_kinds_land_on_the_bus() {
        let bus = EventBus::default();
        let sink = EventBusSink::new(bus.clone());
        let seen: std::sync::Arc<Mutex<Vec<Event>>> = Default::default();
        let recorder = seen.clone();
        let _unsub = bus.on("artifact.emitted", move |e| {
            recorder.lock().unwrap().push(e);
            std::future::ready(Ok(()))
        });
        let recorder = seen.clone();
        let _unsub2 = bus.on("artifact.blocked", move |e| {
            recorder.lock().unwrap().push(e);
            std::future::ready(Ok(()))
        });

        sink.emit(ArtifactEvent::Emitted {
            task_id: "t1".into(),
            stream_id: "sig-1".into(),
            timestamp: "t100".into(),
            value: json!({"filesWritten": 2}),
        })
        .await;
        sink.emit(ArtifactEvent::Blocked {
            task_id: "t1".into(),
            stream_id: "sig-2".into(),
            timestamp: "t101".into(),
            value: json!({"blockedPaths": [{"path": "x.py"}]}),
        })
        .await;

        let events = seen.lock().unwrap().clone();
        assert_eq!(events.len(), 2, "both artifact events must be delivered");
        let emitted = &events[0];
        assert_eq!(emitted.kind, "artifact.emitted");
        assert_eq!(emitted.stream_id, "sig-1");
        assert_eq!(emitted.timestamp, "t100");
        assert_eq!(emitted.value.as_ref().unwrap()["taskId"], "t1");
        assert_eq!(emitted.value.as_ref().unwrap()["filesWritten"], 2);
        let blocked = &events[1];
        assert_eq!(blocked.kind, "artifact.blocked");
        assert_eq!(blocked.sequence, emitted.sequence + 1);
    }

    #[tokio::test]
    async fn non_object_payload_wraps_instead_of_vanishing() {
        let bus = EventBus::default();
        let sink = EventBusSink::new(bus.clone());
        let seen: std::sync::Arc<Mutex<Vec<Event>>> = Default::default();
        let recorder = seen.clone();
        let _unsub = bus.on("artifact.truncated", move |e| {
            recorder.lock().unwrap().push(e);
            std::future::ready(Ok(()))
        });
        sink.emit(ArtifactEvent::Truncated {
            task_id: "t9".into(),
            stream_id: "sig-9".into(),
            timestamp: "t1".into(),
            value: json!("raw string payload"),
        })
        .await;
        let events = seen.lock().unwrap().clone();
        assert_eq!(events.len(), 1);
        let v = events[0].value.as_ref().unwrap();
        assert_eq!(v["taskId"], "t9");
        assert_eq!(v["payload"], "raw string payload");
    }

    // A shut-down bus must not panic the sink (an orchestrator teardown
    // can outlive the bus); the warn path fires instead.
    #[tokio::test]
    async fn sink_survives_shutdown_bus() {
        let bus = EventBus::default();
        let sink = EventBusSink::new(bus.clone());
        bus.shutdown();
        sink.emit(ArtifactEvent::Emitted {
            task_id: "t".into(),
            stream_id: "s".into(),
            timestamp: "n".into(),
            value: json!({}),
        })
        .await;
        assert!(!bus.running());
    }
}
