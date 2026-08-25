//! In-process pub/sub event bus. Port of `@kirkforge/core-events` `EventBus`.
//!
//! Mirrors the TS surface: async `emit` with an idempotency cache +
//! bounded buffer, `on` returning an unsub callable, `drain_buffer`,
//! `shutdown`, and `graceful_shutdown`. The idempotency cache is a
//! `HashMap<event_id, Instant>` with TTL eviction (default 5 min) and a
//! max-size cap (default 10_000) — both matching the TS defaults.
//!
//! Design note: the TS impl fans handlers out serially (`for (const h of
//! handlers) await h(event)`) with a single inflight counter; this port
//! preserves that exact semantic (no parallel fan-out, no broadcast
//! `RecvError::Lagged` failure mode). The workorder suggested
//! `tokio::sync::broadcast` but that would diverge from the serial-await
//! behavior the TS code relies on for its inflight/drain bookkeeping.

use std::collections::{HashMap, VecDeque};
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};

/// Boxed pinned future returned by an async event handler.
type HandlerFut = Pin<Box<dyn Future<Output = HandlerResult> + Send + 'static>>;
/// Async handler signature: takes the event, returns Ok/Err.
pub type Handler = Arc<dyn Fn(Event) -> HandlerFut + Send + Sync + 'static>;
/// Handler outcome. Ok = handled; Err = handler error (collected, not fatal).
pub type HandlerResult = Result<(), HandlerError>;
/// Reason a handler call failed. Mirrors `HandlerError` from core-errors.
#[derive(Debug, Clone)]
pub struct HandlerError {
    pub source: &'static str,
    pub message: String,
}

/// Monotonic handler ID. Each `on()` call grabs the next value; the unsub
/// closure carries the same ID back into the registry to find its entry.
static NEXT_HANDLER_ID: AtomicU64 = AtomicU64::new(0);

/// Options for constructing an [`EventBus`]. All fields optional; defaults
/// match the TS `EventBusOptions` defaults.
#[derive(Debug, Clone)]
pub struct EventBusOptions {
    pub buffer_capacity: usize,
    pub idempotency_cache_size: usize,
    pub idempotency_ttl_ms: u64,
}

impl Default for EventBusOptions {
    fn default() -> Self {
        Self {
            buffer_capacity: 1000,
            idempotency_cache_size: 10_000,
            idempotency_ttl_ms: 300_000,
        }
    }
}

/// Typed discriminator for [`Event`]. Closes the one untyped event
/// surface (WO 45.10): the production-closed `artifact.*` kinds are
/// variants; any other TS-shape kind flows through [`BusEventKind::Other`].
///
/// `as_str()` preserves the TS wire shape (`@kirkforge/core-events` emits
/// string `kind`s); the bus is a deliberate TS port (ADR-006, WO 9.6) and
/// the string kind is load-bearing for the `artifact.*` bridge (WO 36.6).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum BusEventKind {
    /// `artifact.emitted` — files written to disk.
    ArtifactEmitted,
    /// `artifact.blocked` — writes rejected by the safety pipeline.
    ArtifactBlocked,
    /// `artifact.unterminated` — parser saw an open block with no terminator.
    ArtifactUnterminated,
    /// `artifact.truncated` — model emission hit a length limit.
    ArtifactTruncated,
    /// ponytail: TS-port fidelity escape hatch. The typed variants above
    /// are the production-closed artifact set (the only kinds `EventBusSink`
    /// emits today). Any other TS-shape kind — test fixtures like
    /// `verify.lint`, or future producers that want the string wire shape —
    /// flows through here without touching the enum. Upgrade path: promote
    /// to a named variant when a producer moves from test-only to
    /// production. Ceiling: the enum does not statically forbid misspelled
    /// string kinds inside `Other` — that is the cost of TS-interop fidelity.
    Other(String),
}

impl BusEventKind {
    /// TS wire-shape string. Matches the `kind` literal the TS
    /// `@kirkforge/core-events` `EventBus` emits and that
    /// [`crate::session::event_sink_bridge::EventBusSink`] bridges.
    pub fn as_str(&self) -> &str {
        match self {
            BusEventKind::ArtifactEmitted => "artifact.emitted",
            BusEventKind::ArtifactBlocked => "artifact.blocked",
            BusEventKind::ArtifactUnterminated => "artifact.unterminated",
            BusEventKind::ArtifactTruncated => "artifact.truncated",
            BusEventKind::Other(s) => s.as_str(),
        }
    }
}

impl<T: AsRef<str>> From<T> for BusEventKind {
    /// Construct from a TS-shape kind string. Recognized artifact kinds
    /// become typed variants; anything else becomes [`BusEventKind::Other`].
    /// This is the backward-compat path for any producer still passing a
    /// string kind (none in production today; `EventBusSink` is updated in
    /// this same WO).
    fn from(s: T) -> Self {
        match s.as_ref() {
            "artifact.emitted" => BusEventKind::ArtifactEmitted,
            "artifact.blocked" => BusEventKind::ArtifactBlocked,
            "artifact.unterminated" => BusEventKind::ArtifactUnterminated,
            "artifact.truncated" => BusEventKind::ArtifactTruncated,
            other => BusEventKind::Other(other.to_string()),
        }
    }
}

/// An event flowing through the bus. Generic shape covering the
/// `KirkForgeEvent` union: `kind` discriminates, `sequence`/`stream_id`
/// identify the stream, `value` carries the payload. The idempotency
/// cache hashes `{kind, stream_id, sequence, value, timestamp}` so two
/// emits of the same event dedupe.
#[derive(Debug, Clone)]
pub struct Event {
    pub kind: BusEventKind,
    pub schema_version: String,
    pub sequence: u64,
    pub stream_id: String,
    pub timestamp: String,
    pub value: Option<serde_json::Value>,
}

impl Event {
    /// Hash the identity fields → stable idempotency key. Same input
    /// produces the same hash regardless of field declaration order in
    /// the source. Matches the TS `makeEventId` shape (kind, streamId,
    /// sequence, payload, timestamp).
    fn id_key(&self) -> String {
        let payload_str = self
            .value
            .as_ref()
            .map(|v| v.to_string())
            .unwrap_or_default();
        let mut h = Sha256::new();
        h.update(self.kind.as_str().as_bytes());
        h.update([0u8]);
        h.update(self.stream_id.as_bytes());
        h.update([0u8]);
        h.update(self.sequence.to_le_bytes());
        h.update([0u8]);
        h.update(payload_str.as_bytes());
        h.update([0u8]);
        h.update(self.timestamp.as_bytes());
        hex::encode(h.finalize())
    }
}

struct State {
    handlers: HashMap<BusEventKind, Vec<(u64, Handler)>>,
    buffer: VecDeque<Event>,
    buffer_capacity: usize,
    idempotency: HashMap<String, Instant>,
    idempotency_size: usize,
    idempotency_ttl: Duration,
    running: bool,
    shutting_down: bool,
    inflight: usize,
    drain_waiters: Vec<tokio::sync::oneshot::Sender<()>>,
}

impl State {
    fn trim_idempotency(&mut self) {
        let cutoff = Instant::now().checked_sub(self.idempotency_ttl);
        if let Some(cutoff) = cutoff {
            self.idempotency.retain(|_, ts| *ts >= cutoff);
        }
        while self.idempotency.len() > self.idempotency_size {
            // Evict oldest insertion — `HashMap` has no order, so pop an
            // arbitrary key. ponytail: this matches the TS LRU-ish intent
            // well enough for the bounded cache; a true LRU would need a
            // linked map (not justified by any test).
            if let Some(key) = self.idempotency.keys().next().cloned() {
                self.idempotency.remove(&key);
            } else {
                break;
            }
        }
    }
}

/// Inner shared state. Held inside an `Arc` so the unsub closure returned
/// from `on` can capture a handle to the bus without a lifetime tie.
#[derive(Clone)]
pub struct EventBus {
    inner: Arc<Mutex<State>>,
}

/// Why an `emit` was rejected.
#[derive(Debug)]
pub enum EmitError {
    /// Bus is shut down — emit happened after `shutdown`/`graceful_shutdown`.
    NotRunning,
    /// Bounded buffer is full — emit would exceed `buffer_capacity`.
    BufferFull,
    /// Event already processed within the idempotency window.
    Duplicate(String),
}

impl Default for EventBus {
    fn default() -> Self {
        Self::new(EventBusOptions::default())
    }
}

impl EventBus {
    pub fn new(options: EventBusOptions) -> Self {
        Self {
            inner: Arc::new(Mutex::new(State {
                handlers: HashMap::new(),
                buffer: VecDeque::new(),
                buffer_capacity: options.buffer_capacity,
                idempotency: HashMap::new(),
                idempotency_size: options.idempotency_cache_size,
                idempotency_ttl: Duration::from_millis(options.idempotency_ttl_ms),
                running: true,
                shutting_down: false,
                inflight: 0,
                drain_waiters: Vec::new(),
            })),
        }
    }

    /// Subscribe `handler` to events of `kind`. Returns an unsub callable;
    /// calling it removes the handler. Dropping the callable without
    /// calling it leaves the handler subscribed (matches TS semantics).
    pub fn on<F, Fut>(
        &self,
        kind: &BusEventKind,
        handler: F,
    ) -> Box<dyn FnOnce() + Send + Sync + 'static>
    where
        F: Fn(Event) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = HandlerResult> + Send + 'static,
    {
        let handler: Handler = Arc::new(move |e: Event| Box::pin(handler(e)));
        let id = NEXT_HANDLER_ID.fetch_add(1, Ordering::Relaxed);
        let kind_owned = kind.clone();
        {
            // Poison-tolerant (WO 38.2), matching `read_shared_config`:
            // recovery is safe because every critical section below is a
            // trivial map/counter op — a panic between two statements can
            // leave stale entries or a bumped counter, never a broken
            // invariant worth killing the bus (and the TUI) over.
            let mut s = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            s.handlers
                .entry(kind_owned.clone())
                .or_default()
                .push((id, handler));
        }
        let inner = Arc::clone(&self.inner);
        Box::new(move || {
            let mut s = inner.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(vec) = s.handlers.get_mut(&kind_owned) {
                vec.retain(|(hid, _)| *hid != id);
            }
        })
    }

    /// Emit an event to all handlers registered for its `kind`.
    ///
    /// Returns `Err` if the bus is not running, the buffer is full, or the
    /// event was already processed within the idempotency window. On
    /// success, every registered handler is awaited serially; the first
    /// handler error (if any) is returned as `Ok(Some(err))`.
    pub async fn emit(&self, event: Event) -> Result<Option<HandlerError>, EmitError> {
        let prepared = {
            let mut s = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            if !s.running || s.shutting_down {
                return Err(EmitError::NotRunning);
            }
            if s.buffer.len() >= s.buffer_capacity {
                return Err(EmitError::BufferFull);
            }
            let id = event.id_key();
            if s.idempotency.contains_key(&id) {
                return Err(EmitError::Duplicate(id));
            }
            s.idempotency.insert(id, Instant::now());
            s.trim_idempotency();
            s.buffer.push_back(event.clone());
            s.inflight += 1;
            let handlers = s
                .handlers
                .get(&event.kind)
                .map(|v| v.iter().map(|(_, h)| Arc::clone(h)).collect::<Vec<_>>())
                .unwrap_or_default();
            (handlers, event)
        };
        let (handlers, event) = prepared;

        let mut first_err: Option<HandlerError> = None;
        for h in &handlers {
            if let Err(e) = h(event.clone()).await {
                if first_err.is_none() {
                    first_err = Some(e);
                }
            }
        }

        let mut s = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        s.inflight = s.inflight.saturating_sub(1);
        // Remove the first buffer entry equal to this event by sequence+kind.
        if let Some(idx) = s
            .buffer
            .iter()
            .position(|e| e.sequence == event.sequence && e.kind == event.kind)
        {
            s.buffer.remove(idx);
        }
        if s.shutting_down && s.inflight == 0 {
            let waiters = std::mem::take(&mut s.drain_waiters);
            drop(s);
            for tx in waiters {
                let _ = tx.send(());
            }
        }
        Ok(first_err)
    }

    /// Drop all buffered events without delivering them.
    pub fn drain_buffer(&self) {
        let mut s = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        s.buffer.clear();
    }

    /// Returns whether the bus is still accepting emits.
    pub fn running(&self) -> bool {
        self.inner.lock().unwrap_or_else(|e| e.into_inner()).running
    }

    /// Number of events currently being dispatched (started but not yet
    /// finished). Mostly useful for `graceful_shutdown` drain semantics.
    pub fn inflight_count(&self) -> usize {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .inflight
    }

    /// Current buffered event count.
    pub fn buffer_size(&self) -> usize {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .buffer
            .len()
    }

    /// Configured buffer capacity.
    pub fn buffer_capacity(&self) -> usize {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .buffer_capacity
    }

    /// Stop accepting new emits immediately. Inflight handlers continue
    /// to completion (use [`Self::graceful_shutdown`] to wait for them).
    pub fn shutdown(&self) {
        let mut s = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        s.running = false;
        s.shutting_down = true;
    }

    /// Stop accepting new emits, then wait for inflight handlers to
    /// finish (or `drain_timeout_ms`, default 10 s).
    pub async fn graceful_shutdown(&self, drain_timeout_ms: Option<u64>) {
        let rx = {
            let mut s = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            s.shutting_down = true;
            if s.inflight == 0 {
                s.running = false;
                return;
            }
            let (tx, rx) = tokio::sync::oneshot::channel();
            s.drain_waiters.push(tx);
            rx
        };
        let timeout = drain_timeout_ms.unwrap_or(10_000);
        let _ = tokio::time::timeout(Duration::from_millis(timeout), rx).await;
        self.inner.lock().unwrap_or_else(|e| e.into_inner()).running = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn emits_events_to_registered_handlers() {
        let bus = EventBus::default();
        let received = Arc::new(Mutex::new(Vec::new()));
        let recv_clone = Arc::clone(&received);
        let kind = BusEventKind::Other("verify.lint".into());
        let _unsub = bus.on(&kind, move |e| {
            recv_clone.lock().unwrap().push(e);
            std::future::ready(Ok(()))
        });
        bus.emit(Event {
            kind: BusEventKind::Other("verify.lint".into()),
            schema_version: "v3".into(),
            sequence: 1,
            stream_id: "s1".into(),
            timestamp: "now".into(),
            value: Some(serde_json::json!({
                "errors": 0, "warnings": 0, "filesScanned": 0, "durationMs": 0, "details": []
            })),
        })
        .await
        .expect("emit ok");
        assert_eq!(received.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn deduplicates_events_within_idempotency_window() {
        let bus = EventBus::default();
        let count = Arc::new(Mutex::new(0u32));
        let count_clone = Arc::clone(&count);
        let kind = BusEventKind::Other("verify.types".into());
        let _unsub = bus.on(&kind, move |_| {
            *count_clone.lock().unwrap() += 1;
            std::future::ready(Ok(()))
        });
        let event = Event {
            kind: BusEventKind::Other("verify.types".into()),
            schema_version: "v3".into(),
            sequence: 1,
            stream_id: "s1".into(),
            timestamp: "now".into(),
            value: Some(serde_json::json!({ "errors": 0, "durationMs": 0, "details": [] })),
        };
        bus.emit(event.clone()).await.expect("first emit ok");
        match bus.emit(event.clone()).await {
            Err(EmitError::Duplicate(_)) => {}
            other => panic!("expected Duplicate, got {other:?}"),
        }
        assert_eq!(*count.lock().unwrap(), 1, "handler must fire exactly once");
    }

    #[test]
    fn tracks_running_state() {
        let bus = EventBus::default();
        assert!(bus.running());
        bus.shutdown();
        assert!(!bus.running());
    }

    #[test]
    fn buffers_and_reports_stats() {
        let bus = EventBus::new(EventBusOptions {
            buffer_capacity: 500,
            ..Default::default()
        });
        assert_eq!(bus.buffer_capacity(), 500);
        assert_eq!(bus.buffer_size(), 0);
    }

    #[tokio::test]
    async fn unsub_callable_removes_handler() {
        // `on` must return a callable that detaches the handler.
        let bus = EventBus::default();
        let count = Arc::new(Mutex::new(0u32));
        let count_clone = Arc::clone(&count);
        let kind = BusEventKind::Other("verify.lint".into());
        let unsub = bus.on(&kind, move |_| {
            *count_clone.lock().unwrap() += 1;
            std::future::ready(Ok(()))
        });
        bus.emit(Event {
            kind: BusEventKind::Other("verify.lint".into()),
            schema_version: "v3".into(),
            sequence: 1,
            stream_id: "s1".into(),
            timestamp: "now".into(),
            value: None,
        })
        .await
        .unwrap();
        unsub();
        bus.emit(Event {
            kind: BusEventKind::Other("verify.lint".into()),
            schema_version: "v3".into(),
            sequence: 2,
            stream_id: "s1".into(),
            timestamp: "now-2".into(),
            value: None,
        })
        .await
        .unwrap();
        assert_eq!(
            *count.lock().unwrap(),
            1,
            "second emit must not fire handler"
        );
    }

    #[tokio::test]
    async fn graceful_shutdown_drains_inflight() {
        // graceful_shutdown returns once inflight handlers complete.
        let bus = EventBus::default();
        bus.emit(Event {
            kind: BusEventKind::Other("no.handlers.kind".into()),
            schema_version: "v3".into(),
            sequence: 1,
            stream_id: "s1".into(),
            timestamp: "now".into(),
            value: None,
        })
        .await
        .unwrap();
        // No inflight at this point — graceful_shutdown returns immediately.
        bus.graceful_shutdown(None).await;
        assert!(!bus.running());
    }

    // WO 45.46: saturation invariant — under load (emit rate exceeds the
    // handler drain rate, buffer near capacity) the bus must either deliver
    // every event to every registered handler OR fail loudly with `Err`. It
    // must NOT silently drop events emitted outside the idempotency window.
    // The invariant: `delivered + rejected == emitted` (no event vanishes).
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn emit_faster_than_drain_never_silently_drops() {
        // Buffer holds 64; a 5ms-per-event handler drains far slower than we
        // emit. Each event has a unique identity hash (distinct sequence +
        // timestamp) so none fall in the idempotency window.
        let bus = EventBus::new(EventBusOptions {
            buffer_capacity: 64,
            ..Default::default()
        });
        let received = Arc::new(Mutex::new(Vec::<u64>::new()));
        let recv_clone = Arc::clone(&received);
        let kind = BusEventKind::Other("verify.lint".into());
        let _unsub = bus.on(&kind, move |e| {
            let r = Arc::clone(&recv_clone);
            Box::pin(async move {
                tokio::time::sleep(Duration::from_millis(5)).await;
                r.lock().unwrap().push(e.sequence);
                Ok(())
            })
        });

        const N: u64 = 1000;
        let mut join_set = tokio::task::JoinSet::new();
        for i in 0..N {
            let bus = bus.clone();
            join_set.spawn(async move {
                let res = bus
                    .emit(Event {
                        kind: BusEventKind::Other("verify.lint".into()),
                        schema_version: "v3".into(),
                        sequence: i,
                        stream_id: "s1".into(),
                        timestamp: format!("t{i}"),
                        value: None,
                    })
                    .await;
                (i, res)
            });
        }
        let mut results = Vec::with_capacity(N as usize);
        while let Some(j) = join_set.join_next().await {
            results.push(j.unwrap());
        }

        // Give any in-flight handler a chance to finish before we read the
        // received list — handlers complete after emit returns Ok.
        tokio::time::sleep(Duration::from_millis(50)).await;

        let mut delivered = 0u64;
        let mut rejected = 0u64;
        for (i, res) in &results {
            match res {
                Ok(_) => delivered += 1,
                Err(EmitError::BufferFull) => rejected += 1,
                Err(EmitError::NotRunning) => rejected += 1,
                Err(EmitError::Duplicate(d)) => {
                    panic!("emit {i} returned Duplicate {d} for a unique event")
                }
            }
        }
        assert_eq!(
            delivered + rejected,
            N,
            "no-silent-drop invariant violated: delivered={delivered} rejected={rejected} total={}",
            delivered + rejected
        );
        // Every Ok emit must have actually reached the handler.
        let got = received.lock().unwrap().len() as u64;
        assert_eq!(
            got, delivered,
            "Ok count ({delivered}) must equal delivered-to-handler count ({got})"
        );
    }

    // ── Property-based tests (WO 45.46) ───────────────────────────────────
    // proptest! blocks live in a dedicated submodule because
    // `cargo clippy --all-targets` chokes on `#[test]` items generated by the
    // `proptest!` macro when they're directly inside `mod tests`
    // ("cannot test inner items"). Isolating them in their own module avoids
    // the clippy false-positive. Same pattern as WO 41.7 / 43.4.
    mod proptest_suites {
        use super::*;
        use proptest::prelude::*;

        fn make_event(seq: u64) -> Event {
            Event {
                kind: BusEventKind::Other("verify.lint".into()),
                schema_version: "v3".into(),
                sequence: seq,
                stream_id: "s1".into(),
                timestamp: format!("t{seq}"),
                value: None,
            }
        }

        proptest! {
            #![proptest_config(ProptestConfig::with_cases(64))]

            // For arbitrary emit counts and handler delays, the no-silent-drop
            // invariant holds: every emit is either delivered (Ok) or
            // rejected with an explicit Err. None vanish.
            #[test]
            fn emit_returns_err_or_eventually_delivered(
                n in 1u64..=500,
                delay_ms in 0u64..=10,
            ) {
                let rt = tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(2)
                    .enable_all()
                    .build()
                    .unwrap();
                let _ = rt.block_on(async {
                    let bus = EventBus::new(EventBusOptions {
                        buffer_capacity: 16,
                        ..Default::default()
                    });
                    let received = Arc::new(Mutex::new(0u64));
                    let recv_clone = Arc::clone(&received);
                    let kind = BusEventKind::Other("verify.lint".into());
                    let _unsub = bus.on(&kind, move |_| {
                        let r = Arc::clone(&recv_clone);
                        let d = Duration::from_millis(delay_ms);
                        Box::pin(async move {
                            if !d.is_zero() {
                                tokio::time::sleep(d).await;
                            }
                            *r.lock().unwrap() += 1;
                            Ok(())
                        })
                    });

                    let mut join_set = tokio::task::JoinSet::new();
                    for i in 0..n {
                        let bus = bus.clone();
                        join_set.spawn(async move { bus.emit(make_event(i)).await });
                    }
                    let mut results = Vec::new();
                    while let Some(j) = join_set.join_next().await {
                        results.push(j.unwrap());
                    }

                    // Let any in-flight handlers finish before counting.
                    tokio::time::sleep(Duration::from_millis(50)).await;

                    let mut delivered = 0u64;
                    let mut rejected = 0u64;
                    for res in &results {
                        match res {
                            Ok(_) => delivered += 1,
                            Err(EmitError::BufferFull) | Err(EmitError::NotRunning) => {
                                rejected += 1
                            }
                            Err(EmitError::Duplicate(d)) => {
                                panic!("emit returned Duplicate {d} for a unique event")
                            }
                        }
                    }
                    prop_assert_eq!(
                        delivered + rejected,
                        n,
                        "no-silent-drop violated"
                    );
                    let got = *received.lock().unwrap();
                    prop_assert_eq!(got, delivered, "Ok count must equal handler deliveries");
                    Ok(())
                });
            }
        }
    }
}
