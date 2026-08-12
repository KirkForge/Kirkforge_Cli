//! `EventSink` — async seam for the orchestrator's artifact-related events.
//! In TS the orchestrator calls `sharedEventBus.emit(...)` for
//! artifact.emitted / artifact.blocked / artifact.unterminated /
//! artifact.truncated. The kf-code EventBus lives in `src/shared/event_bus.rs`
//! (binary-only, not a crate); rather than pull a dep cycle in, we define a
//! minimal sink here. Production wiring adapts it to the binary's EventBus.

use std::sync::Mutex;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Subset of artifact events the orchestrator emits. Tag matches the TS
/// event `kind` literal.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ArtifactEvent {
    /// `artifact.emitted` — files written to disk.
    Emitted {
        task_id: String,
        stream_id: String,
        timestamp: String,
        value: Value,
    },
    /// `artifact.blocked` — writes rejected by the safety pipeline.
    Blocked {
        task_id: String,
        stream_id: String,
        timestamp: String,
        value: Value,
    },
    /// `artifact.unterminated` — parser saw an open block with no terminator.
    Unterminated {
        task_id: String,
        stream_id: String,
        timestamp: String,
        value: Value,
    },
    /// `artifact.truncated` — model emission hit a length limit.
    Truncated {
        task_id: String,
        stream_id: String,
        timestamp: String,
        value: Value,
    },
}

impl ArtifactEvent {
    pub fn kind(&self) -> &'static str {
        match self {
            ArtifactEvent::Emitted { .. } => "artifact.emitted",
            ArtifactEvent::Blocked { .. } => "artifact.blocked",
            ArtifactEvent::Unterminated { .. } => "artifact.unterminated",
            ArtifactEvent::Truncated { .. } => "artifact.truncated",
        }
    }
}

#[async_trait]
pub trait EventSink: Send + Sync {
    async fn emit(&self, event: ArtifactEvent);
}

/// Default no-op sink.
pub struct NullSink;

#[async_trait]
impl EventSink for NullSink {
    async fn emit(&self, _event: ArtifactEvent) {}
}

/// Test sink that records every event for later inspection.
pub struct RecordingSink {
    events: Mutex<Vec<ArtifactEvent>>,
}

impl RecordingSink {
    pub fn new() -> Self {
        Self {
            events: Mutex::new(Vec::new()),
        }
    }
    pub fn events(&self) -> Vec<ArtifactEvent> {
        self.events.lock().expect("sink lock").clone()
    }
    pub fn kinds(&self) -> Vec<&'static str> {
        self.events
            .lock()
            .expect("sink lock")
            .iter()
            .map(ArtifactEvent::kind)
            .collect()
    }
}

impl Default for RecordingSink {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl EventSink for RecordingSink {
    async fn emit(&self, event: ArtifactEvent) {
        self.events.lock().expect("sink lock").push(event);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn null_sink_swallows() {
        NullSink
            .emit(ArtifactEvent::Emitted {
                task_id: "t".into(),
                stream_id: "s".into(),
                timestamp: "now".into(),
                value: json!({}),
            })
            .await;
    }

    #[tokio::test]
    async fn recording_sink_captures_kinds() {
        let s = RecordingSink::new();
        s.emit(ArtifactEvent::Emitted {
            task_id: "t".into(),
            stream_id: "s".into(),
            timestamp: "n".into(),
            value: json!({}),
        })
        .await;
        s.emit(ArtifactEvent::Blocked {
            task_id: "t".into(),
            stream_id: "s".into(),
            timestamp: "n".into(),
            value: json!({}),
        })
        .await;
        assert_eq!(s.kinds(), vec!["artifact.emitted", "artifact.blocked"]);
        assert_eq!(s.events().len(), 2);
    }

    #[test]
    fn event_kind_strings() {
        let v = json!({});
        let cases: &[(&str, ArtifactEvent)] = &[
            (
                "artifact.emitted",
                ArtifactEvent::Emitted {
                    task_id: "t".into(),
                    stream_id: "s".into(),
                    timestamp: "n".into(),
                    value: v.clone(),
                },
            ),
            (
                "artifact.blocked",
                ArtifactEvent::Blocked {
                    task_id: "t".into(),
                    stream_id: "s".into(),
                    timestamp: "n".into(),
                    value: v.clone(),
                },
            ),
            (
                "artifact.unterminated",
                ArtifactEvent::Unterminated {
                    task_id: "t".into(),
                    stream_id: "s".into(),
                    timestamp: "n".into(),
                    value: v.clone(),
                },
            ),
            (
                "artifact.truncated",
                ArtifactEvent::Truncated {
                    task_id: "t".into(),
                    stream_id: "s".into(),
                    timestamp: "n".into(),
                    value: v.clone(),
                },
            ),
        ];
        for (kind, e) in cases {
            assert_eq!(e.kind(), *kind);
        }
    }
}
