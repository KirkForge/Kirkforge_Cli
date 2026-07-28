/// Event bus — pub/sub dispatcher for tool execution events.
///
/// The event bus sits between tool execution and downstream consumers
/// (verifiers, hooks, logging). It provides:
///
/// - **Typed events**: 9 event kinds covering all tool operations
/// - **Handler dispatch**: events fan out to all registered handlers
/// - **Idempotency**: events with the same (kind, content-hash) are
///   only delivered once per handler, preventing duplicate reactions
/// - **Sequential-per-handler**: each handler processes events in order
///
/// # Phase 4 integration
///
/// Verifiers register as handlers on specific event kinds. A lint verifier
/// registers on `LintRun` and `FileEdit`, a security verifier on `BashExec`
/// and `FileWrite`. See [`crate::session::verifier`].
use std::collections::{HashMap, HashSet, VecDeque};
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::Mutex;

// ── Event Kinds ─────────────────────────────────────────────────────────

/// All event kinds the bus supports.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize)]
pub enum EventKind {
    FileRead,
    FileWrite,
    Edit,
    BashExec,
    GitOperation,
    LintRun,
    TypeCheck,
    SecurityScan,
    ToolError,
}

impl EventKind {
    /// All known kinds — used for "subscribe to everything".
    pub fn all() -> &'static [EventKind] {
        &[
            EventKind::FileRead,
            EventKind::FileWrite,
            EventKind::Edit,
            EventKind::BashExec,
            EventKind::GitOperation,
            EventKind::LintRun,
            EventKind::TypeCheck,
            EventKind::SecurityScan,
            EventKind::ToolError,
        ]
    }

    /// Human-readable label.
    pub fn label(self) -> &'static str {
        match self {
            EventKind::FileRead => "file_read",
            EventKind::FileWrite => "file_write",
            EventKind::Edit => "edit",
            EventKind::BashExec => "bash_exec",
            EventKind::GitOperation => "git_operation",
            EventKind::LintRun => "lint_run",
            EventKind::TypeCheck => "type_check",
            EventKind::SecurityScan => "security_scan",
            EventKind::ToolError => "tool_error",
        }
    }
}

impl std::fmt::Display for EventKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.label())
    }
}

// ── Events ──────────────────────────────────────────────────────────────

/// A concrete event on the bus.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(tag = "kind", content = "payload")]
pub enum BusEvent {
    FileRead(FileReadEvent),
    FileWrite(FileWriteEvent),
    Edit(EditEvent),
    BashExec(BashExecEvent),
    GitOperation(GitOperationEvent),
    LintRun(LintRunEvent),
    TypeCheck(TypeCheckEvent),
    SecurityScan(SecurityScanEvent),
    ToolError(ToolErrorEvent),
}

impl BusEvent {
    /// The event kind discriminator.
    pub fn kind(&self) -> EventKind {
        match self {
            BusEvent::FileRead(_) => EventKind::FileRead,
            BusEvent::FileWrite(_) => EventKind::FileWrite,
            BusEvent::Edit(_) => EventKind::Edit,
            BusEvent::BashExec(_) => EventKind::BashExec,
            BusEvent::GitOperation(_) => EventKind::GitOperation,
            BusEvent::LintRun(_) => EventKind::LintRun,
            BusEvent::TypeCheck(_) => EventKind::TypeCheck,
            BusEvent::SecurityScan(_) => EventKind::SecurityScan,
            BusEvent::ToolError(_) => EventKind::ToolError,
        }
    }

    /// Compute an idempotency key for this event.
    ///
    /// Two events with the same (kind, idem_key) are considered
    /// duplicates. The key is content-derived so semantically identical
    /// events deduplicate even if they arrive at different timestamps.
    pub fn idem_key(&self) -> u64 {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        match self {
            BusEvent::FileRead(e) => {
                e.path.hash(&mut hasher);
            }
            BusEvent::FileWrite(e) => {
                e.path.hash(&mut hasher);
                e.content_length.hash(&mut hasher);
            }
            BusEvent::Edit(e) => {
                e.path.hash(&mut hasher);
                e.diff.hash(&mut hasher);
            }
            BusEvent::BashExec(e) => {
                e.command.hash(&mut hasher);
            }
            BusEvent::GitOperation(e) => {
                e.args.hash(&mut hasher);
            }
            BusEvent::LintRun(e) => {
                e.tool.hash(&mut hasher);
                e.target.hash(&mut hasher);
            }
            BusEvent::TypeCheck(e) => {
                e.target.hash(&mut hasher);
            }
            BusEvent::SecurityScan(e) => {
                e.target.hash(&mut hasher);
            }
            BusEvent::ToolError(e) => {
                e.tool.hash(&mut hasher);
                e.error.hash(&mut hasher);
            }
        }
        hasher.finish()
    }
}

// ── Event payloads ──────────────────────────────────────────────────────

#[derive(Debug, Clone, serde::Serialize)]
pub struct FileReadEvent {
    pub path: PathBuf,
    pub size_bytes: u64,
    pub truncated: bool,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct FileWriteEvent {
    pub path: PathBuf,
    pub content_length: usize,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct EditEvent {
    pub path: PathBuf,
    pub diff: String,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct BashExecEvent {
    pub command: String,
    pub exit_code: i32,
    pub stdout_len: usize,
    pub stderr_len: usize,
    /// Working directory the command ran in, when known. Used by downstream
    /// verifiers (e.g. the git verifier) to run follow-up checks in the
    /// correct repository.
    pub workdir: Option<PathBuf>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct GitOperationEvent {
    pub args: Vec<String>,
    pub output: String,
    pub success: bool,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct LintRunEvent {
    pub tool: String,
    pub target: String,
    pub findings: Vec<LintFinding>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct TypeCheckEvent {
    pub target: String,
    pub errors: Vec<String>,
    pub success: bool,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct SecurityScanEvent {
    pub target: String,
    pub issues: Vec<SecurityIssue>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ToolErrorEvent {
    pub tool: String,
    pub error: String,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct LintFinding {
    pub severity: String, // "error" | "warning" | "info"
    pub message: String,
    pub file: Option<String>,
    pub line: Option<usize>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct SecurityIssue {
    pub severity: String,
    pub kind: String,
    pub description: String,
    pub file: Option<String>,
}

// ── Handler trait ───────────────────────────────────────────────────────

/// Result returned by a handler after processing an event.
#[derive(Debug, Clone)]
pub struct HandlerResult {
    /// Unique handler ID (used for idempotency tracking).
    pub handler_id: String,
    /// Whether processing was successful.
    pub success: bool,
    /// Optional message (error description or summary).
    pub message: String,
}

/// A handler receives events and processes them.
///
/// Handlers are registered on one or more [`EventKind`]s. Every matching
/// event is dispatched to the handler's `handle()` method.
#[async_trait::async_trait]
pub trait EventHandler: Send + Sync {
    /// Unique identifier for this handler (used for idempotency tracking).
    fn id(&self) -> &str;

    /// The event kinds this handler subscribes to.
    fn subscribed_kinds(&self) -> Vec<EventKind>;

    /// Process an event. Called sequentially (never concurrently for the
    /// same handler instance).
    async fn handle(&self, event: &BusEvent) -> HandlerResult;
}

// ── Event bus ───────────────────────────────────────────────────────────

/// Inner state protected by the bus's mutex.
struct BusInner {
    /// Registered handlers, keyed by handler_id.
    handlers: HashMap<String, Arc<dyn EventHandler>>,
    /// Idempotency cache: for each (handler_id, EventKind, idem_key) tuple,
    /// stores whether it's been processed. This prevents a handler from
    /// seeing the same event twice.
    ///
    /// Key format: "{handler_id}:{kind_label}:{idem_key}"
    idem_cache: HashSet<(String, EventKind, u64)>,
    /// Insertion order of idempotency-cache entries. Kept in sync with
    /// `idem_cache` so trimming is deterministic (oldest-first) rather
    /// than relying on HashSet iteration order.
    idem_order: VecDeque<(String, EventKind, u64)>,
    /// Max idempotency-cache entries. When exceeded the oldest entries are
    /// dropped. The cache is bounded to prevent unbounded growth during long
    /// sessions that produce huge numbers of unique events.
    max_idem_cache: usize,
    /// Event history (most recent N events for inspection/debug).
    history: Vec<StoredEvent>,
    /// Max history entries.
    max_history: usize,
}

/// A stored event with metadata.
#[derive(Debug, Clone)]
pub struct StoredEvent {
    pub kind: EventKind,
    pub event: Arc<BusEvent>,
    pub timestamp: Instant,
    pub idem_key: u64,
    pub handled_by: Vec<String>,
}

/// Thread-safe event bus.
#[derive(Clone)]
pub struct EventBus {
    inner: Arc<Mutex<BusInner>>,
    /// Guards against concurrent dispatch of the same (kind, idem_key)
    /// to prevent TOCTOU duplicates when handlers are slow.
    in_flight: Arc<Mutex<HashSet<(EventKind, u64)>>>,
}

impl EventBus {
    /// Create a new empty event bus.
    /// Default maximum idempotency-cache entries.
    const DEFAULT_MAX_IDEM_CACHE: usize = 10_000;

    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(BusInner {
                handlers: HashMap::new(),
                idem_cache: HashSet::new(),
                idem_order: VecDeque::new(),
                max_idem_cache: Self::DEFAULT_MAX_IDEM_CACHE,
                history: Vec::new(),
                max_history: 100,
            })),
            in_flight: Arc::new(Mutex::new(HashSet::new())),
        }
    }

    /// Create with a custom history limit.
    pub fn with_history_limit(max_history: usize) -> Self {
        Self {
            inner: Arc::new(Mutex::new(BusInner {
                handlers: HashMap::new(),
                idem_cache: HashSet::new(),
                idem_order: VecDeque::new(),
                max_idem_cache: Self::DEFAULT_MAX_IDEM_CACHE,
                history: Vec::new(),
                max_history,
            })),
            in_flight: Arc::new(Mutex::new(HashSet::new())),
        }
    }

    #[cfg(test)]
    fn with_limits(max_idem_cache: usize, max_history: usize) -> Self {
        Self {
            inner: Arc::new(Mutex::new(BusInner {
                handlers: HashMap::new(),
                idem_cache: HashSet::new(),
                idem_order: VecDeque::new(),
                max_idem_cache,
                history: Vec::new(),
                max_history,
            })),
            in_flight: Arc::new(Mutex::new(HashSet::new())),
        }
    }

    /// Register a handler.
    ///
    /// Returns an error if a handler with the same ID is already registered.
    pub async fn register(&self, handler: Arc<dyn EventHandler>) -> anyhow::Result<()> {
        let mut inner = self.inner.lock().await;
        let id = handler.id().to_string();
        if inner.handlers.contains_key(&id) {
            anyhow::bail!("Handler '{id}' is already registered");
        }
        inner.handlers.insert(id, handler);
        Ok(())
    }

    /// Unregister a handler by ID.
    ///
    /// Also cleans up its idempotency cache entries.
    pub async fn unregister(&self, handler_id: &str) -> bool {
        let mut inner = self.inner.lock().await;
        let existed = inner.handlers.remove(handler_id).is_some();
        if existed {
            // Remove all idem-cache entries for this handler
            inner.idem_cache.retain(|(hid, _, _)| hid != handler_id);
            inner.idem_order.retain(|(hid, _, _)| hid != handler_id);
        }
        existed
    }

    /// Check if an event has already been handled by a specific handler.
    pub async fn was_handled(&self, handler_id: &str, kind: EventKind, idem_key: u64) -> bool {
        let inner = self.inner.lock().await;
        inner
            .idem_cache
            .contains(&(handler_id.to_string(), kind, idem_key))
    }

    /// Dispatch an event to all matching handlers.
    ///
    /// Returns the list of handler results. Handlers that have already
    /// processed an idempotent-equivalent event are skipped.
    /// Uses an in-flight guard to prevent TOCTOU duplicates from
    /// concurrent dispatch calls.
    pub async fn dispatch(&self, event: &BusEvent) -> Vec<HandlerResult> {
        let kind = event.kind();
        let idem_key = event.idem_key();

        // ── In-flight guard: prevent TOCTOU duplicates ──
        {
            let mut in_flight = self.in_flight.lock().await;
            if !in_flight.insert((kind, idem_key)) {
                // This event is already being dispatched by another task
                return vec![];
            }
        }

        let result = self
            .dispatch_inner(Arc::new(event.clone()), kind, idem_key)
            .await;

        // Release in-flight guard
        {
            let mut in_flight = self.in_flight.lock().await;
            in_flight.remove(&(kind, idem_key));
        }

        result
    }

    /// Inner dispatch (holds in-flight guard already).
    async fn dispatch_inner(
        &self,
        event: Arc<BusEvent>,
        kind: EventKind,
        idem_key: u64,
    ) -> Vec<HandlerResult> {
        let inner = self.inner.lock().await;

        // Find matching handlers that haven't seen this event
        let matching: Vec<Arc<dyn EventHandler>> = inner
            .handlers
            .values()
            .filter(|h| {
                h.subscribed_kinds().contains(&kind)
                    && !inner
                        .idem_cache
                        .contains(&(h.id().to_string(), kind, idem_key))
            })
            .cloned()
            .collect();

        // Drop the lock before calling handlers (each handler may be slow)
        drop(inner);

        let mut results = Vec::with_capacity(matching.len());
        let mut handled_by = Vec::new();

        for handler in &matching {
            let result = handler.handle(&event).await;
            let handler_id = handler.id().to_string();
            handled_by.push(handler_id.clone());
            results.push(result);
        }

        // Re-acquire to update idem cache and history
        let mut inner = self.inner.lock().await;
        for handler_id in &handled_by {
            let key = (handler_id.clone(), kind, idem_key);
            if inner.idem_cache.insert(key.clone()) {
                inner.idem_order.push_back(key);
            }
        }
        // Trim idempotency cache if it has grown beyond its bound. The
        // companion `idem_order` queue records insertion order, so we drop
        // the oldest entries deterministically instead of relying on
        // HashSet iteration order.
        while inner.idem_cache.len() > inner.max_idem_cache {
            if let Some(key) = inner.idem_order.pop_front() {
                inner.idem_cache.remove(&key);
            } else {
                break;
            }
        }

        // Trim history and store. Keep the event behind an `Arc` so the
        // in-memory history does not clone large owned payloads on every
        // `recent_events` call (P8).
        inner.history.push(StoredEvent {
            kind,
            event: event.clone(),
            timestamp: Instant::now(),
            idem_key,
            handled_by,
        });
        while inner.history.len() > inner.max_history {
            inner.history.remove(0);
        }

        results
    }

    /// Dispatch an event and wait for all handlers. Convenience wrapper.
    pub async fn dispatch_and_await(&self, event: BusEvent) -> Vec<HandlerResult> {
        self.dispatch(&event).await
    }

    /// Return recent event history (for debugging / UI display).
    pub async fn recent_events(&self, count: usize) -> Vec<StoredEvent> {
        let inner = self.inner.lock().await;
        let start = inner.history.len().saturating_sub(count);
        inner.history[start..].to_vec()
    }

    /// Return the number of registered handlers.
    pub async fn handler_count(&self) -> usize {
        let inner = self.inner.lock().await;
        inner.handlers.len()
    }

    /// Clear all handlers and the idempotency cache.
    pub async fn reset(&self) {
        let mut inner = self.inner.lock().await;
        inner.handlers.clear();
        inner.idem_cache.clear();
        inner.idem_order.clear();
        inner.history.clear();
    }

    /// Clear only the idempotency cache (keep handlers).
    pub async fn clear_idem_cache(&self) {
        let mut inner = self.inner.lock().await;
        inner.idem_cache.clear();
        inner.idem_order.clear();
    }

    /// Return registered handler IDs.
    pub async fn handler_ids(&self) -> Vec<String> {
        let inner = self.inner.lock().await;
        inner.handlers.keys().cloned().collect()
    }
}

impl Default for EventBus {
    fn default() -> Self {
        Self::new()
    }
}

// ── No-op handler (for testing) ─────────────────────────────────────────

/// A handler that does nothing — useful as a placeholder or for testing.
pub struct NoopHandler {
    id: String,
    kinds: Vec<EventKind>,
}

impl NoopHandler {
    pub fn new(id: &str, kinds: Vec<EventKind>) -> Self {
        Self {
            id: id.to_string(),
            kinds,
        }
    }
}

#[async_trait::async_trait]
impl EventHandler for NoopHandler {
    fn id(&self) -> &str {
        &self.id
    }

    fn subscribed_kinds(&self) -> Vec<EventKind> {
        self.kinds.clone()
    }

    async fn handle(&self, _event: &BusEvent) -> HandlerResult {
        HandlerResult {
            handler_id: self.id.clone(),
            success: true,
            message: "noop".into(),
        }
    }
}

// ── Tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct CountingHandler {
        id: String,
        kinds: Vec<EventKind>,
        call_count: AtomicUsize,
        last_event: Mutex<Option<BusEvent>>,
    }

    impl CountingHandler {
        fn new(id: &str, kinds: Vec<EventKind>) -> Self {
            Self {
                id: id.to_string(),
                kinds,
                call_count: AtomicUsize::new(0),
                last_event: Mutex::new(None),
            }
        }
    }

    #[async_trait::async_trait]
    impl EventHandler for CountingHandler {
        fn id(&self) -> &str {
            &self.id
        }

        fn subscribed_kinds(&self) -> Vec<EventKind> {
            self.kinds.clone()
        }

        async fn handle(&self, event: &BusEvent) -> HandlerResult {
            self.call_count.fetch_add(1, Ordering::SeqCst);
            let mut last = self.last_event.lock().await;
            *last = Some(event.clone());
            HandlerResult {
                handler_id: self.id.clone(),
                success: true,
                message: "ok".into(),
            }
        }
    }

    fn make_event(kind: EventKind) -> BusEvent {
        match kind {
            EventKind::FileRead => BusEvent::FileRead(FileReadEvent {
                path: PathBuf::from("/tmp/test.rs"),
                size_bytes: 100,
                truncated: false,
            }),
            EventKind::FileWrite => BusEvent::FileWrite(FileWriteEvent {
                path: PathBuf::from("/tmp/test.rs"),
                content_length: 200,
            }),
            EventKind::Edit => BusEvent::Edit(EditEvent {
                path: PathBuf::from("/tmp/test.rs"),
                diff: "@@ -1 +1 @@\n-old\n+new".into(),
            }),
            EventKind::BashExec => BusEvent::BashExec(BashExecEvent {
                command: "cargo check".into(),
                exit_code: 0,
                stdout_len: 100,
                stderr_len: 0,
                workdir: None,
            }),
            EventKind::GitOperation => BusEvent::GitOperation(GitOperationEvent {
                args: vec!["status".into()],
                output: "On branch main\nnothing to commit".into(),
                success: true,
            }),
            EventKind::LintRun => BusEvent::LintRun(LintRunEvent {
                tool: "clippy".into(),
                target: ".".into(),
                findings: vec![],
            }),
            EventKind::TypeCheck => BusEvent::TypeCheck(TypeCheckEvent {
                target: ".".into(),
                errors: vec![],
                success: true,
            }),
            EventKind::SecurityScan => BusEvent::SecurityScan(SecurityScanEvent {
                target: ".".into(),
                issues: vec![],
            }),
            EventKind::ToolError => BusEvent::ToolError(ToolErrorEvent {
                tool: "bash".into(),
                error: "command not found".into(),
            }),
        }
    }

    #[tokio::test]
    async fn test_dispatch_to_matching_handler() {
        let bus = EventBus::new();
        let handler = Arc::new(CountingHandler::new(
            "counter",
            vec![EventKind::FileRead, EventKind::FileWrite],
        ));
        bus.register(handler.clone()).await.unwrap();

        let event = make_event(EventKind::FileRead);
        let results = bus.dispatch(&event).await;
        assert_eq!(results.len(), 1);
        assert!(results[0].success);
        assert_eq!(handler.call_count.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn test_handler_not_called_for_unsubscribed_kind() {
        let bus = EventBus::new();
        let handler = Arc::new(CountingHandler::new("counter", vec![EventKind::FileRead]));
        bus.register(handler.clone()).await.unwrap();

        let event = make_event(EventKind::BashExec);
        let results = bus.dispatch(&event).await;
        assert!(results.is_empty());
        assert_eq!(handler.call_count.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn test_idempotency_same_event_skipped() {
        let bus = EventBus::new();
        let handler = Arc::new(CountingHandler::new("counter", vec![EventKind::FileRead]));
        bus.register(handler.clone()).await.unwrap();

        let event = make_event(EventKind::FileRead);
        let _ = bus.dispatch(&event).await;
        let _ = bus.dispatch(&event).await; // same content — should be skipped

        assert_eq!(handler.call_count.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn test_different_events_not_deduplicated() {
        let bus = EventBus::new();
        let handler = Arc::new(CountingHandler::new(
            "counter",
            vec![EventKind::FileRead, EventKind::BashExec],
        ));
        bus.register(handler.clone()).await.unwrap();

        let e1 = make_event(EventKind::FileRead);
        let e2 = make_event(EventKind::BashExec);
        let _ = bus.dispatch(&e1).await;
        let _ = bus.dispatch(&e2).await;

        assert_eq!(handler.call_count.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn test_multiple_handlers_all_called() {
        let bus = EventBus::new();
        let h1 = Arc::new(CountingHandler::new("h1", vec![EventKind::FileRead]));
        let h2 = Arc::new(CountingHandler::new("h2", vec![EventKind::FileRead]));
        bus.register(h1.clone()).await.unwrap();
        bus.register(h2.clone()).await.unwrap();

        let event = make_event(EventKind::FileRead);
        let results = bus.dispatch(&event).await;
        assert_eq!(results.len(), 2);
        assert_eq!(h1.call_count.load(Ordering::SeqCst), 1);
        assert_eq!(h2.call_count.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn test_duplicate_registration_rejected() {
        let bus = EventBus::new();
        let h1 = Arc::new(CountingHandler::new("dup", vec![EventKind::FileRead]));
        let h2 = Arc::new(CountingHandler::new("dup", vec![EventKind::FileRead]));
        bus.register(h1).await.unwrap();
        let result = bus.register(h2).await;
        assert!(result.is_err(), "Duplicate registration should be rejected");
    }

    #[tokio::test]
    async fn test_unregister_removes_handler_and_idem_cache() {
        let bus = EventBus::new();
        let handler = Arc::new(CountingHandler::new("remove-me", vec![EventKind::FileRead]));
        bus.register(handler.clone()).await.unwrap();

        let event = make_event(EventKind::FileRead);
        let _ = bus.dispatch(&event).await;
        assert_eq!(handler.call_count.load(Ordering::SeqCst), 1);

        // Unregister — should return true
        assert!(bus.unregister("remove-me").await);

        // Re-dispatch — no handlers remain, count stays at 1
        let _ = bus.dispatch(&event).await;
        assert_eq!(handler.call_count.load(Ordering::SeqCst), 1);
        assert_eq!(bus.handler_count().await, 0);
    }

    #[tokio::test]
    async fn test_reset_clears_everything() {
        let bus = EventBus::new();
        let handler = Arc::new(CountingHandler::new("reset-me", vec![EventKind::FileRead]));
        bus.register(handler.clone()).await.unwrap();

        let event = make_event(EventKind::FileRead);
        let _ = bus.dispatch(&event).await;
        assert_eq!(bus.handler_count().await, 1);

        bus.reset().await;
        assert_eq!(bus.handler_count().await, 0);

        // Re-dispatch (no handlers)
        let results = bus.dispatch(&event).await;
        assert!(results.is_empty());
    }

    #[tokio::test]
    async fn test_event_history() {
        let bus = EventBus::new();
        let handler = Arc::new(NoopHandler::new(
            "historian",
            vec![EventKind::FileRead, EventKind::BashExec],
        ));
        bus.register(handler).await.unwrap();

        let e1 = make_event(EventKind::FileRead);
        let e2 = make_event(EventKind::BashExec);
        let _ = bus.dispatch(&e1).await;
        let _ = bus.dispatch(&e2).await;

        let recent = bus.recent_events(10).await;
        assert_eq!(recent.len(), 2);
        assert_eq!(recent[0].kind, EventKind::FileRead);
        assert_eq!(recent[1].kind, EventKind::BashExec);
    }

    #[tokio::test]
    async fn test_idem_key_differentiation() {
        // Two events of same kind with different content should NOT deduplicate
        let bus = EventBus::new();
        let handler = Arc::new(CountingHandler::new("idem-test", vec![EventKind::FileRead]));
        bus.register(handler.clone()).await.unwrap();

        let e1 = BusEvent::FileRead(FileReadEvent {
            path: PathBuf::from("/tmp/a.rs"),
            size_bytes: 100,
            truncated: false,
        });
        let e2 = BusEvent::FileRead(FileReadEvent {
            path: PathBuf::from("/tmp/b.rs"),
            size_bytes: 200,
            truncated: false,
        });

        let _ = bus.dispatch(&e1).await;
        let _ = bus.dispatch(&e2).await;
        assert_eq!(handler.call_count.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn test_clear_idem_cache() {
        let bus = EventBus::new();
        let handler = Arc::new(CountingHandler::new(
            "clear-test",
            vec![EventKind::FileRead],
        ));
        bus.register(handler.clone()).await.unwrap();

        let event = make_event(EventKind::FileRead);
        let _ = bus.dispatch(&event).await;
        assert_eq!(handler.call_count.load(Ordering::SeqCst), 1);

        // Clear idem cache, re-dispatch same event — should process again
        bus.clear_idem_cache().await;
        let _ = bus.dispatch(&event).await;
        assert_eq!(handler.call_count.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn test_idem_cache_trims_oldest_entries_first() {
        // Regression for C11: HashSet iteration order made eviction random.
        // With the ordered queue, the oldest entries are evicted first.
        let bus = EventBus::with_limits(2, 10);
        let handler = Arc::new(CountingHandler::new("trim-test", vec![EventKind::FileRead]));
        bus.register(handler.clone()).await.unwrap();

        // Dispatch three distinct events. Cache size is 2, so the first one
        // is the oldest and must be evicted.
        let e1 = BusEvent::FileRead(FileReadEvent {
            path: PathBuf::from("/tmp/e1.rs"),
            size_bytes: 100,
            truncated: false,
        });
        let e2 = BusEvent::FileRead(FileReadEvent {
            path: PathBuf::from("/tmp/e2.rs"),
            size_bytes: 200,
            truncated: false,
        });
        let e3 = BusEvent::FileRead(FileReadEvent {
            path: PathBuf::from("/tmp/e3.rs"),
            size_bytes: 300,
            truncated: false,
        });

        let _ = bus.dispatch(&e1).await;
        let _ = bus.dispatch(&e2).await;
        let _ = bus.dispatch(&e3).await;
        assert_eq!(handler.call_count.load(Ordering::SeqCst), 3);

        // e1 is the oldest entry and should have been evicted when e3 was
        // inserted, so re-dispatching e1 calls the handler again.
        let _ = bus.dispatch(&e1).await;
        assert_eq!(handler.call_count.load(Ordering::SeqCst), 4);

        // Re-dispatching the just-reinserted e1 is idempotent.
        let _ = bus.dispatch(&e1).await;
        assert_eq!(handler.call_count.load(Ordering::SeqCst), 4);

        // e3 was newer than e1 and should still be retained, so it is skipped.
        let _ = bus.dispatch(&e3).await;
        assert_eq!(handler.call_count.load(Ordering::SeqCst), 4);
    }

    #[test]
    fn test_event_kind_all_contains_all_variants() {
        let all = EventKind::all();
        assert_eq!(all.len(), 9);
        assert!(all.contains(&EventKind::FileRead));
        assert!(all.contains(&EventKind::FileWrite));
        assert!(all.contains(&EventKind::Edit));
        assert!(all.contains(&EventKind::BashExec));
        assert!(all.contains(&EventKind::GitOperation));
        assert!(all.contains(&EventKind::LintRun));
        assert!(all.contains(&EventKind::TypeCheck));
        assert!(all.contains(&EventKind::SecurityScan));
        assert!(all.contains(&EventKind::ToolError));
    }

    #[test]
    fn test_event_kind_label_matches_each_variant() {
        assert_eq!(EventKind::FileRead.label(), "file_read");
        assert_eq!(EventKind::FileWrite.label(), "file_write");
        assert_eq!(EventKind::Edit.label(), "edit");
        assert_eq!(EventKind::BashExec.label(), "bash_exec");
        assert_eq!(EventKind::GitOperation.label(), "git_operation");
        assert_eq!(EventKind::LintRun.label(), "lint_run");
        assert_eq!(EventKind::TypeCheck.label(), "type_check");
        assert_eq!(EventKind::SecurityScan.label(), "security_scan");
        assert_eq!(EventKind::ToolError.label(), "tool_error");
    }

    #[test]
    fn test_event_kind_display_matches_label() {
        for kind in EventKind::all() {
            assert_eq!(format!("{kind}"), kind.label());
        }
    }

    #[test]
    fn test_bus_event_kind_returns_correct_discriminant() {
        assert_eq!(make_event(EventKind::FileRead).kind(), EventKind::FileRead);
        assert_eq!(
            make_event(EventKind::FileWrite).kind(),
            EventKind::FileWrite
        );
        assert_eq!(make_event(EventKind::Edit).kind(), EventKind::Edit);
        assert_eq!(make_event(EventKind::BashExec).kind(), EventKind::BashExec);
        assert_eq!(
            make_event(EventKind::GitOperation).kind(),
            EventKind::GitOperation
        );
        assert_eq!(make_event(EventKind::LintRun).kind(), EventKind::LintRun);
        assert_eq!(
            make_event(EventKind::TypeCheck).kind(),
            EventKind::TypeCheck
        );
        assert_eq!(
            make_event(EventKind::SecurityScan).kind(),
            EventKind::SecurityScan
        );
        assert_eq!(
            make_event(EventKind::ToolError).kind(),
            EventKind::ToolError
        );
    }

    #[test]
    fn test_bus_event_idem_key_stable_across_calls() {
        let e = make_event(EventKind::FileRead);
        let k1 = e.idem_key();
        let k2 = e.idem_key();
        assert_eq!(k1, k2, "idem_key must be deterministic for the same event");
    }

    #[test]
    fn test_bus_event_idem_key_differs_for_different_content() {
        let e1 = BusEvent::FileRead(FileReadEvent {
            path: PathBuf::from("/tmp/a.rs"),
            size_bytes: 100,
            truncated: false,
        });
        let e2 = BusEvent::FileRead(FileReadEvent {
            path: PathBuf::from("/tmp/b.rs"),
            size_bytes: 200,
            truncated: false,
        });
        assert_ne!(e1.idem_key(), e2.idem_key());
    }

    #[test]
    fn test_bus_event_idem_key_file_write_uses_path_and_length() {
        let e1 = BusEvent::FileWrite(FileWriteEvent {
            path: PathBuf::from("/tmp/a.rs"),
            content_length: 100,
        });
        let e2 = BusEvent::FileWrite(FileWriteEvent {
            path: PathBuf::from("/tmp/a.rs"),
            content_length: 200,
        });
        assert_ne!(e1.idem_key(), e2.idem_key());
    }

    #[test]
    fn test_bus_event_idem_key_edit_uses_path_and_diff() {
        let e1 = BusEvent::Edit(EditEvent {
            path: PathBuf::from("/tmp/a.rs"),
            diff: "diff1".into(),
        });
        let e2 = BusEvent::Edit(EditEvent {
            path: PathBuf::from("/tmp/a.rs"),
            diff: "diff2".into(),
        });
        assert_ne!(e1.idem_key(), e2.idem_key());
    }

    #[test]
    fn test_bus_event_idem_key_bash_exec_uses_command() {
        let e1 = BusEvent::BashExec(BashExecEvent {
            command: "ls".into(),
            exit_code: 0,
            stdout_len: 10,
            stderr_len: 0,
            workdir: None,
        });
        let e2 = BusEvent::BashExec(BashExecEvent {
            command: "pwd".into(),
            exit_code: 0,
            stdout_len: 10,
            stderr_len: 0,
            workdir: None,
        });
        assert_ne!(e1.idem_key(), e2.idem_key());
    }

    #[test]
    fn test_bus_event_idem_key_tool_error_uses_tool_and_error() {
        let e1 = BusEvent::ToolError(ToolErrorEvent {
            tool: "bash".into(),
            error: "e1".into(),
        });
        let e2 = BusEvent::ToolError(ToolErrorEvent {
            tool: "bash".into(),
            error: "e2".into(),
        });
        assert_ne!(e1.idem_key(), e2.idem_key());
    }

    #[test]
    fn test_noop_handler_returns_success() {
        let handler = NoopHandler::new("noop", vec![EventKind::FileRead]);
        let event = make_event(EventKind::FileRead);
        let rt = tokio::runtime::Runtime::new().unwrap();
        let result = rt.block_on(handler.handle(&event));
        assert!(result.success);
        assert_eq!(result.handler_id, "noop");
        assert_eq!(result.message, "noop");
    }

    #[test]
    fn test_handler_result_fields() {
        let r = HandlerResult {
            handler_id: "h1".into(),
            success: true,
            message: "ok".into(),
        };
        assert_eq!(r.handler_id, "h1");
        assert!(r.success);
        assert_eq!(r.message, "ok");
    }

    #[test]
    fn test_stored_event_fields() {
        let event = Arc::new(make_event(EventKind::FileRead));
        let stored = StoredEvent {
            kind: EventKind::FileRead,
            event: event.clone(),
            timestamp: Instant::now(),
            idem_key: 42,
            handled_by: vec!["h1".into()],
        };
        assert_eq!(stored.kind, EventKind::FileRead);
        assert_eq!(stored.idem_key, 42);
        assert_eq!(stored.handled_by, vec!["h1".to_string()]);
    }

    #[tokio::test]
    async fn test_with_history_limit_caps_history() {
        let bus = EventBus::with_history_limit(2);
        let handler = Arc::new(NoopHandler::new("hist", EventKind::all().to_vec()));
        bus.register(handler).await.unwrap();
        for _ in 0..5 {
            let _ = bus.dispatch(&make_event(EventKind::BashExec)).await;
        }
        let recent = bus.recent_events(100).await;
        assert_eq!(recent.len(), 2, "history should be capped at 2");
    }

    #[tokio::test]
    async fn test_recent_events_returns_last_n() {
        let bus = EventBus::new();
        let handler = Arc::new(NoopHandler::new("recent", EventKind::all().to_vec()));
        bus.register(handler).await.unwrap();
        let _ = bus.dispatch(&make_event(EventKind::FileRead)).await;
        let _ = bus.dispatch(&make_event(EventKind::BashExec)).await;
        let _ = bus.dispatch(&make_event(EventKind::Edit)).await;
        let recent = bus.recent_events(2).await;
        assert_eq!(recent.len(), 2);
        assert_eq!(recent[0].kind, EventKind::BashExec);
        assert_eq!(recent[1].kind, EventKind::Edit);
    }

    #[tokio::test]
    async fn test_recent_events_returns_empty_when_zero_requested() {
        let bus = EventBus::new();
        let handler = Arc::new(NoopHandler::new("zero", EventKind::all().to_vec()));
        bus.register(handler).await.unwrap();
        let _ = bus.dispatch(&make_event(EventKind::FileRead)).await;
        let recent = bus.recent_events(0).await;
        assert!(recent.is_empty());
    }

    #[tokio::test]
    async fn test_handler_ids_returns_registered_ids() {
        let bus = EventBus::new();
        let h1 = Arc::new(NoopHandler::new("a", vec![EventKind::FileRead]));
        let h2 = Arc::new(NoopHandler::new("b", vec![EventKind::FileRead]));
        bus.register(h1).await.unwrap();
        bus.register(h2).await.unwrap();
        let mut ids = bus.handler_ids().await;
        ids.sort();
        assert_eq!(ids, vec!["a".to_string(), "b".to_string()]);
    }

    #[tokio::test]
    async fn test_unregister_unknown_returns_false() {
        let bus = EventBus::new();
        assert!(!bus.unregister("nope").await);
    }

    #[tokio::test]
    async fn test_was_handled_returns_false_for_unknown() {
        let bus = EventBus::new();
        assert!(!bus.was_handled("h", EventKind::FileRead, 123).await);
    }

    #[tokio::test]
    async fn test_dispatch_and_await_is_alias_for_dispatch() {
        let bus = EventBus::new();
        let handler = Arc::new(CountingHandler::new("alias", vec![EventKind::FileRead]));
        bus.register(handler.clone()).await.unwrap();
        let event = make_event(EventKind::FileRead);
        let results = bus.dispatch_and_await(event).await;
        assert_eq!(results.len(), 1);
        assert_eq!(handler.call_count.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn test_dispatch_no_handlers_returns_empty() {
        let bus = EventBus::new();
        let event = make_event(EventKind::FileRead);
        let results = bus.dispatch(&event).await;
        assert!(results.is_empty());
    }

    #[tokio::test]
    async fn test_handler_subscribes_to_multiple_kinds() {
        let bus = EventBus::new();
        let handler = Arc::new(CountingHandler::new(
            "multi",
            vec![EventKind::FileRead, EventKind::BashExec, EventKind::Edit],
        ));
        bus.register(handler.clone()).await.unwrap();
        let _ = bus.dispatch(&make_event(EventKind::FileRead)).await;
        let _ = bus.dispatch(&make_event(EventKind::BashExec)).await;
        let _ = bus.dispatch(&make_event(EventKind::Edit)).await;
        let _ = bus.dispatch(&make_event(EventKind::GitOperation)).await;
        assert_eq!(handler.call_count.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn test_default_is_new_bus() {
        let bus = EventBus::default();
        assert_eq!(bus.handler_count().await, 0);
    }
}
