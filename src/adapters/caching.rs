//! Content-addressed cache for model response streams, with an adapter wrapper.
//!
//! Caching is opt-in via `Config::cache_enabled`. When enabled, every
//! successful stream is serialized to disk under `cache_dir` keyed by a
//! hash of `(model, system_prompt_hash, messages_hash, tools_hash,
//! json_mode)`. On a subsequent identical request the cached
//! [`StreamEvent`]s are replayed through a fresh channel, avoiding a
//! network round-trip.
//!
//! The cache deliberately does *not* store partial or error streams,
//! and it does not attempt to cache tool-result turns (the inputs change
//! every turn). It is most useful for repeated read-only discovery
//! queries across forked personas or repeated `/explore` passes.

use crate::adapters::ModelAdapter;
use crate::shared::{Config, Message, ModelInfo, StreamEvent, ToolDef};
use sha2::{Digest, Sha256};
use std::path::PathBuf;

/// In-memory + on-disk cache for model streams.
#[derive(Clone)]
pub struct ResponseCache {
    enabled: bool,
    dir: PathBuf,
    /// Small in-memory cache to avoid re-reading disk for hot keys.
    memory: std::sync::Arc<std::sync::Mutex<std::collections::HashMap<CacheKey, Vec<StreamEvent>>>>,
}

impl ResponseCache {
    /// Create a cache. If `enabled` is false the cache never reads or
    /// writes, but the struct can still be passed around cheaply.
    pub fn new(enabled: bool, dir: Option<PathBuf>) -> Self {
        let dir = dir.unwrap_or_else(default_cache_dir);
        Self {
            enabled,
            dir,
            memory: std::sync::Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        }
    }

    /// Look up a cached stream.
    pub fn get(
        &self,
        model: &str,
        messages: &[Message],
        tools: &[ToolDef],
        json_mode: bool,
    ) -> Option<Vec<StreamEvent>> {
        if !self.enabled {
            return None;
        }
        let key = CacheKey::new(model, messages, tools, json_mode);

        // 1. In-memory
        {
            let mem = self.memory.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(events) = mem.get(&key) {
                return Some(events.clone());
            }
        }

        // 2. On-disk
        let path = self.path_for(&key);
        // Cap the read at 64 MiB so a corrupted or crafted multi-GB cache
        // file can't OOM the process (WO 15.11). A size over the cap is
        // treated as a cache miss with a warning.
        const MAX_CACHE_FILE_BYTES: u64 = 64 * 1024 * 1024;
        match std::fs::metadata(&path) {
            Ok(m) if m.len() > MAX_CACHE_FILE_BYTES => {
                tracing::warn!(
                    path = %path.display(),
                    size = m.len(),
                    cap = MAX_CACHE_FILE_BYTES,
                    "response cache entry exceeds size cap; treating as miss"
                );
                return None;
            }
            Ok(_) => {}
            Err(_) => return None,
        }
        let bytes = std::fs::read(&path).ok()?;
        let events: Vec<StreamEvent> = serde_json::from_slice(&bytes).ok()?;

        // Promote to memory for future hits.
        let mut mem = self.memory.lock().unwrap_or_else(|e| e.into_inner());
        mem.insert(key, events.clone());
        Some(events)
    }

    /// Store a stream in the cache.
    pub fn put(
        &self,
        model: &str,
        messages: &[Message],
        tools: &[ToolDef],
        json_mode: bool,
        events: &[StreamEvent],
    ) {
        if !self.enabled {
            return;
        }
        let key = CacheKey::new(model, messages, tools, json_mode);

        // Skip empty or error-only streams.
        if events.is_empty() {
            return;
        }
        if events.iter().all(|e| matches!(e, StreamEvent::Error(_))) {
            return;
        }

        let mut mem = self.memory.lock().unwrap_or_else(|e| e.into_inner());
        mem.insert(key.clone(), events.to_vec());

        let path = self.path_for(&key);
        if let Some(parent) = path.parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                tracing::warn!(
                    error = %e,
                    dir = %parent.display(),
                    "Failed to create response cache directory"
                );
            }
        }
        if let Ok(bytes) = serde_json::to_vec(events) {
            if let Err(e) = std::fs::write(&path, bytes) {
                tracing::warn!(
                    error = %e,
                    path = %path.display(),
                    "Failed to write response cache entry"
                );
            }
        }
    }

    fn path_for(&self, key: &CacheKey) -> PathBuf {
        self.dir.join(format!("{}.bin", key.hash))
    }
}

fn default_cache_dir() -> PathBuf {
    crate::session::data_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .join("cache")
}

/// Cache key content-addressed by model + hash of inputs.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
struct CacheKey {
    hash: String,
}

impl CacheKey {
    fn new(model: &str, messages: &[Message], tools: &[ToolDef], json_mode: bool) -> Self {
        let mut hasher = Sha256::new();
        hasher.update(model.as_bytes());

        if let Ok(bytes) = serde_json::to_vec(messages) {
            hasher.update(&bytes);
        }
        if let Ok(bytes) = serde_json::to_vec(
            &tools
                .iter()
                .map(|t| (t.name, t.description, &t.parameters))
                .collect::<Vec<_>>(),
        ) {
            hasher.update(&bytes);
        }
        hasher.update([json_mode as u8]);

        Self {
            hash: hex::encode(hasher.finalize()),
        }
    }
}

/// Conditionally wrap an adapter with the response cache.
///
/// Returns the adapter unchanged when caching is disabled, so callers don't
/// have to branch on `Config::cache_enabled`.
pub fn maybe_wrap_cached(adapter: Box<dyn ModelAdapter>, config: &Config) -> Box<dyn ModelAdapter> {
    if config.model.cache_enabled {
        let cache = ResponseCache::new(true, config.model.cache_dir.clone());
        Box::new(CachingAdapter::new(adapter, cache, config.model.json_mode))
    } else {
        adapter
    }
}

/// Wrapper that adds response caching to any [`ModelAdapter`].
pub struct CachingAdapter {
    inner: Box<dyn ModelAdapter>,
    cache: ResponseCache,
    json_mode: bool,
}

impl CachingAdapter {
    /// Wrap an existing adapter with a cache.
    pub fn new(inner: Box<dyn ModelAdapter>, cache: ResponseCache, json_mode: bool) -> Self {
        Self {
            inner,
            cache,
            json_mode,
        }
    }
}

#[async_trait::async_trait]
impl ModelAdapter for CachingAdapter {
    fn model_info(&self) -> ModelInfo {
        self.inner.model_info()
    }

    fn set_json_mode(&mut self, json_mode: bool) {
        self.json_mode = json_mode;
        self.inner.set_json_mode(json_mode);
    }

    async fn stream(
        &self,
        messages: &[Message],
        tools: &[ToolDef],
    ) -> anyhow::Result<tokio::sync::mpsc::Receiver<StreamEvent>> {
        let model_info = self.inner.model_info();

        if let Some(events) = self
            .cache
            .get(&model_info.name, messages, tools, self.json_mode)
        {
            let (tx, rx) = tokio::sync::mpsc::channel::<StreamEvent>(events.len().max(1));
            tokio::spawn(async move {
                for ev in events {
                    if tx.send(ev).await.is_err() {
                        break;
                    }
                }
            });
            return Ok(rx);
        }

        let rx = self.inner.stream(messages, tools).await?;
        let cache = self.cache.clone();
        let model_name = model_info.name.clone();
        let messages_owned = messages.to_vec();
        let tools_owned = tools.to_vec();
        let json_mode = self.json_mode;

        let (tx_out, rx_out) = tokio::sync::mpsc::channel::<StreamEvent>(4096);
        tokio::spawn(async move {
            let mut events = Vec::new();
            let mut inner = rx;
            loop {
                tokio::select! {
                    biased;
                    _ = tx_out.closed() => break,
                    ev = inner.recv() => {
                        let Some(ev) = ev else { break; };
                        events.push(ev.clone());
                        if tx_out.send(ev).await.is_err() {
                            break;
                        }
                    }
                }
            }
            // Only cache complete streams — the final event must be Done.
            // A dropped consumer, a cancelled turn, or an adapter that exits
            // without a terminal event would otherwise poison the cache with
            // a truncated response.
            let complete = matches!(events.last(), Some(StreamEvent::Done { .. }));
            if complete {
                cache.put(
                    &model_name,
                    &messages_owned,
                    &tools_owned,
                    json_mode,
                    &events,
                );
            } else {
                tracing::debug!(
                    model = %model_name,
                    event_count = events.len(),
                    "Stream incomplete; skipping cache write"
                );
            }
        });

        Ok(rx_out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapters::ModelAdapter;
    use crate::shared::{FinishReason, ModelInfo, TokenUsage, ToolCallStyle};

    fn message(role: crate::shared::Role, content: &str) -> Message {
        Message {
            role,
            content: content.into(),
            ..Default::default()
        }
    }

    #[test]
    fn cache_miss_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        let cache = ResponseCache::new(true, Some(dir.path().into()));
        let result = cache.get(
            "test-model",
            &[message(crate::shared::Role::User, "hello")],
            &[],
            false,
        );
        assert!(result.is_none());
    }

    #[test]
    fn cache_put_and_get_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let cache = ResponseCache::new(true, Some(dir.path().into()));
        let events = vec![
            StreamEvent::Text("hello".into()),
            StreamEvent::Done {
                finish_reason: FinishReason::Stop,
                usage: Some(TokenUsage {
                    prompt_tokens: Some(1),
                    completion_tokens: Some(1),
                    cached_tokens: None,
                }),
            },
        ];

        cache.put(
            "test-model",
            &[message(crate::shared::Role::User, "hi")],
            &[],
            false,
            &events,
        );

        let got = cache
            .get(
                "test-model",
                &[message(crate::shared::Role::User, "hi")],
                &[],
                false,
            )
            .expect("cache hit after put");
        assert_eq!(got, events);
    }

    #[test]
    fn disabled_cache_never_writes() {
        let dir = tempfile::tempdir().unwrap();
        let cache = ResponseCache::new(false, Some(dir.path().into()));
        cache.put(
            "test-model",
            &[message(crate::shared::Role::User, "hi")],
            &[],
            false,
            &[StreamEvent::Text("x".into())],
        );
        let entries: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .collect();
        assert!(entries.is_empty());
    }

    #[test]
    fn disabled_cache_get_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        let cache = ResponseCache::new(false, Some(dir.path().into()));
        assert!(cache
            .get(
                "test-model",
                &[message(crate::shared::Role::User, "hi")],
                &[],
                false
            )
            .is_none());
    }

    #[test]
    fn cache_skips_empty_event_streams() {
        let dir = tempfile::tempdir().unwrap();
        let cache = ResponseCache::new(true, Some(dir.path().into()));
        cache.put(
            "test-model",
            &[message(crate::shared::Role::User, "hi")],
            &[],
            false,
            &[],
        );
        let entries: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .collect();
        assert!(entries.is_empty());
    }

    #[test]
    fn cache_skips_error_only_streams() {
        let dir = tempfile::tempdir().unwrap();
        let cache = ResponseCache::new(true, Some(dir.path().into()));
        cache.put(
            "test-model",
            &[message(crate::shared::Role::User, "hi")],
            &[],
            false,
            &[
                StreamEvent::Error("boom".into()),
                StreamEvent::Error("boom2".into()),
            ],
        );
        let entries: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .collect();
        assert!(entries.is_empty());
    }

    #[test]
    fn cache_key_is_deterministic_for_same_inputs() {
        let k1 = CacheKey::new(
            "model",
            &[message(crate::shared::Role::User, "hi")],
            &[],
            false,
        );
        let k2 = CacheKey::new(
            "model",
            &[message(crate::shared::Role::User, "hi")],
            &[],
            false,
        );
        assert_eq!(k1, k2);
    }

    #[test]
    fn cache_key_differs_for_different_models() {
        let k1 = CacheKey::new(
            "model-a",
            &[message(crate::shared::Role::User, "hi")],
            &[],
            false,
        );
        let k2 = CacheKey::new(
            "model-b",
            &[message(crate::shared::Role::User, "hi")],
            &[],
            false,
        );
        assert_ne!(k1, k2);
    }

    #[test]
    fn cache_key_differs_for_different_messages() {
        let k1 = CacheKey::new(
            "model",
            &[message(crate::shared::Role::User, "hi")],
            &[],
            false,
        );
        let k2 = CacheKey::new(
            "model",
            &[message(crate::shared::Role::User, "bye")],
            &[],
            false,
        );
        assert_ne!(k1, k2);
    }

    #[test]
    fn cache_key_differs_for_different_json_mode() {
        let k1 = CacheKey::new(
            "model",
            &[message(crate::shared::Role::User, "hi")],
            &[],
            false,
        );
        let k2 = CacheKey::new(
            "model",
            &[message(crate::shared::Role::User, "hi")],
            &[],
            true,
        );
        assert_ne!(k1, k2);
    }

    #[test]
    fn cache_key_differs_for_different_tools() {
        let tool_a = crate::shared::ToolDef {
            name: "tool_a",
            description: "desc",
            parameters: serde_json::json!({"type": "object"}),
        };
        let tool_b = crate::shared::ToolDef {
            name: "tool_b",
            description: "desc",
            parameters: serde_json::json!({"type": "object"}),
        };
        let k1 = CacheKey::new(
            "model",
            &[message(crate::shared::Role::User, "hi")],
            &[tool_a],
            false,
        );
        let k2 = CacheKey::new(
            "model",
            &[message(crate::shared::Role::User, "hi")],
            &[tool_b],
            false,
        );
        assert_ne!(k1, k2);
    }

    #[test]
    fn cache_key_hash_is_hex_string() {
        let k = CacheKey::new(
            "model",
            &[message(crate::shared::Role::User, "hi")],
            &[],
            false,
        );
        assert!(k.hash.chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(k.hash.len(), 64);
    }

    #[test]
    fn cache_put_promotes_to_memory_for_subsequent_get() {
        let dir = tempfile::tempdir().unwrap();
        let cache = ResponseCache::new(true, Some(dir.path().into()));
        let events = vec![StreamEvent::Text("hello".into())];
        cache.put(
            "test-model",
            &[message(crate::shared::Role::User, "hi")],
            &[],
            false,
            &events,
        );
        let got = cache
            .get(
                "test-model",
                &[message(crate::shared::Role::User, "hi")],
                &[],
                false,
            )
            .expect("cache hit from memory");
        assert_eq!(got, events);
    }

    #[test]
    fn cache_get_reads_from_disk_when_not_in_memory() {
        let dir = tempfile::tempdir().unwrap();
        let cache = ResponseCache::new(true, Some(dir.path().into()));
        let events = vec![StreamEvent::Text("disk-hit".into())];
        cache.put(
            "test-model",
            &[message(crate::shared::Role::User, "hi")],
            &[],
            false,
            &events,
        );
        let cache2 = ResponseCache::new(true, Some(dir.path().into()));
        let got = cache2
            .get(
                "test-model",
                &[message(crate::shared::Role::User, "hi")],
                &[],
                false,
            )
            .expect("cache hit from disk");
        assert_eq!(got, events);
    }

    #[test]
    fn cache_path_for_uses_hex_hash_filename() {
        let dir = tempfile::tempdir().unwrap();
        let cache = ResponseCache::new(true, Some(dir.path().into()));
        let key = CacheKey::new(
            "model",
            &[message(crate::shared::Role::User, "hi")],
            &[],
            false,
        );
        let path = cache.path_for(&key);
        assert!(path.to_string_lossy().ends_with(".bin"));
        assert!(path.starts_with(dir.path()));
    }

    #[test]
    fn cache_returns_none_for_corrupt_disk_file() {
        let dir = tempfile::tempdir().unwrap();
        let cache = ResponseCache::new(true, Some(dir.path().into()));
        let key = CacheKey::new(
            "model",
            &[message(crate::shared::Role::User, "hi")],
            &[],
            false,
        );
        let path = cache.path_for(&key);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"not valid serde_json").unwrap();
        assert!(cache
            .get(
                "model",
                &[message(crate::shared::Role::User, "hi")],
                &[],
                false
            )
            .is_none());
    }

    #[test]
    fn cache_returns_none_for_oversized_disk_file() {
        let dir = tempfile::tempdir().unwrap();
        let cache = ResponseCache::new(true, Some(dir.path().into()));
        let key = CacheKey::new(
            "model",
            &[message(crate::shared::Role::User, "hi")],
            &[],
            false,
        );
        let path = cache.path_for(&key);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        // Write a file larger than the 64 MiB cap. A sparse seek would
        // not report the full length on all filesystems, so write real
        // bytes: 64 MiB + 1 byte. This is the boundary the cap guards.
        let cap = 64 * 1024 * 1024usize;
        let mut f = std::fs::File::create(&path).unwrap();
        use std::io::Write;
        let chunk = vec![0u8; 1024 * 1024];
        for _ in 0..64 {
            f.write_all(&chunk).unwrap();
        }
        f.write_all(b"!").unwrap();
        f.sync_all().unwrap();
        drop(f);
        assert_eq!(std::fs::metadata(&path).unwrap().len() as usize, cap + 1);
        assert!(cache
            .get(
                "model",
                &[message(crate::shared::Role::User, "hi")],
                &[],
                false
            )
            .is_none());
    }

    #[test]
    fn default_cache_dir_joins_cache_subdir() {
        let dir = default_cache_dir();
        assert!(dir.ends_with("cache"));
    }

    #[test]
    fn cache_new_uses_default_dir_when_none() {
        let cache = ResponseCache::new(false, None);
        assert!(cache.dir.ends_with("cache"));
    }

    struct DummyAdapter {
        events: Vec<StreamEvent>,
        info: ModelInfo,
    }

    #[async_trait::async_trait]
    impl ModelAdapter for DummyAdapter {
        fn model_info(&self) -> ModelInfo {
            self.info.clone()
        }

        async fn stream(
            &self,
            _messages: &[Message],
            _tools: &[ToolDef],
        ) -> anyhow::Result<tokio::sync::mpsc::Receiver<StreamEvent>> {
            let (tx, rx) = tokio::sync::mpsc::channel::<StreamEvent>(self.events.len().max(1));
            let events = self.events.clone();
            tokio::spawn(async move {
                for ev in events {
                    if tx.send(ev).await.is_err() {
                        break;
                    }
                }
            });
            Ok(rx)
        }
    }

    fn adapter(events: Vec<StreamEvent>) -> Box<dyn ModelAdapter> {
        Box::new(DummyAdapter {
            events,
            info: ModelInfo {
                name: "test-model".into(),
                supports_thinking: false,
                tool_call_format: ToolCallStyle::Native,
                max_context_tokens: 4096,
                recommended_temperature: 0.7,
                supports_images: false,
                supports_cache: false,
            },
        })
    }

    #[tokio::test]
    async fn caching_adapter_miss_then_hit() {
        let tmp = tempfile::tempdir().unwrap();
        let cache = ResponseCache::new(true, Some(tmp.path().into()));
        let events = vec![
            StreamEvent::Text("hi".into()),
            StreamEvent::Done {
                finish_reason: FinishReason::Stop,
                usage: Some(TokenUsage {
                    prompt_tokens: Some(1),
                    completion_tokens: Some(1),
                    cached_tokens: None,
                }),
            },
        ];
        let inner = adapter(events.clone());
        let wrapped = CachingAdapter::new(inner, cache, false);

        let messages: Vec<Message> = vec![];
        let tools: Vec<ToolDef> = vec![];

        // First call hits the inner adapter and populates the cache.
        let mut rx = wrapped.stream(&messages, &tools).await.unwrap();
        let mut got = Vec::new();
        while let Some(ev) = rx.recv().await {
            got.push(ev);
        }
        assert_eq!(got, events);

        // Second call with identical inputs replays from cache.
        let mut rx = wrapped.stream(&messages, &tools).await.unwrap();
        let mut got = Vec::new();
        while let Some(ev) = rx.recv().await {
            got.push(ev);
        }
        assert_eq!(got, events);
    }

    #[tokio::test]
    async fn caching_adapter_disabled_uses_inner_every_time() {
        let tmp = tempfile::tempdir().unwrap();
        let cache = ResponseCache::new(false, Some(tmp.path().into()));
        let events = vec![StreamEvent::Text("x".into())];
        let inner = adapter(events.clone());
        let wrapped = CachingAdapter::new(inner, cache, false);

        let messages: Vec<Message> = vec![];
        let tools: Vec<ToolDef> = vec![];

        for _ in 0..2 {
            let mut rx = wrapped.stream(&messages, &tools).await.unwrap();
            let mut got = Vec::new();
            while let Some(ev) = rx.recv().await {
                got.push(ev);
            }
            assert_eq!(got, events);
        }
    }

    #[tokio::test]
    async fn caching_adapter_skips_cache_when_consumer_drops() {
        let tmp = tempfile::tempdir().unwrap();
        let cache = ResponseCache::new(true, Some(tmp.path().into()));
        let events = vec![
            StreamEvent::Text("first".into()),
            StreamEvent::Text("second".into()),
        ];
        let inner = adapter(events.clone());
        let wrapped = CachingAdapter::new(inner, cache.clone(), false);

        let messages: Vec<Message> = vec![];
        let tools: Vec<ToolDef> = vec![];

        // Consume only the first event, then drop the receiver.
        let mut rx = wrapped.stream(&messages, &tools).await.unwrap();
        let _first = rx.recv().await;
        drop(rx);

        // Give the forwarder a moment to observe the closed receiver.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        // The cache must remain empty for this key; a truncated stream
        // must not be replayed on a later identical request.
        assert!(
            cache
                .get(&wrapped.model_info().name, &messages, &tools, false)
                .is_none(),
            "partial stream should not be cached"
        );
    }

    #[tokio::test]
    async fn caching_adapter_aborts_forwarder_on_consumer_drop() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc;
        // Inner adapter emits events one at a time (capacity-1 channel), so
        // emission of event N+1 is gated by the forwarder pulling event N.
        // `emitted` therefore counts how many events the forwarder drained.
        // With the consumer-drop abort, the forwarder stops pulling after the
        // consumer drops, so `emitted` stays small. Without the abort it would
        // drain all events.
        let emitted = Arc::new(AtomicUsize::new(0));
        struct CountingAdapter {
            emitted: Arc<AtomicUsize>,
            n: usize,
        }
        #[async_trait::async_trait]
        impl ModelAdapter for CountingAdapter {
            fn model_info(&self) -> ModelInfo {
                ModelInfo {
                    name: "count-model".into(),
                    supports_thinking: false,
                    tool_call_format: ToolCallStyle::Native,
                    max_context_tokens: 4096,
                    recommended_temperature: 0.7,
                    supports_images: false,
                    supports_cache: false,
                }
            }
            async fn stream(
                &self,
                _messages: &[Message],
                _tools: &[ToolDef],
            ) -> anyhow::Result<tokio::sync::mpsc::Receiver<StreamEvent>> {
                let (tx, rx) = tokio::sync::mpsc::channel::<StreamEvent>(1);
                let emitted = self.emitted.clone();
                let n = self.n;
                tokio::spawn(async move {
                    for i in 0..n {
                        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                        let ev = StreamEvent::Text(format!("ev{i}"));
                        if tx.send(ev).await.is_err() {
                            break;
                        }
                        emitted.fetch_add(1, Ordering::SeqCst);
                    }
                });
                Ok(rx)
            }
        }
        let tmp = tempfile::tempdir().unwrap();
        let cache = ResponseCache::new(true, Some(tmp.path().into()));
        let inner = Box::new(CountingAdapter {
            emitted: emitted.clone(),
            n: 10,
        });
        let wrapped = CachingAdapter::new(inner, cache, false);
        let messages: Vec<Message> = vec![];
        let tools: Vec<ToolDef> = vec![];

        let mut rx = wrapped.stream(&messages, &tools).await.unwrap();
        let _first = rx.recv().await;
        drop(rx);
        tokio::time::sleep(std::time::Duration::from_millis(60)).await;

        let count = emitted.load(Ordering::SeqCst);
        assert!(
            count < 10,
            "forwarder should abort after consumer drop, but drained {count}/10 events"
        );
    }

    #[tokio::test]
    async fn caching_adapter_incomplete_stream_not_cached() {
        let tmp = tempfile::tempdir().unwrap();
        let cache = ResponseCache::new(true, Some(tmp.path().into()));
        let events = vec![StreamEvent::Text("no done event".into())];
        let inner = adapter(events.clone());
        let wrapped = CachingAdapter::new(inner, cache.clone(), false);
        let messages: Vec<Message> = vec![];
        let tools: Vec<ToolDef> = vec![];
        let mut rx = wrapped.stream(&messages, &tools).await.unwrap();
        while let Some(_ev) = rx.recv().await {}
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert!(
            cache
                .get(&wrapped.model_info().name, &messages, &tools, false)
                .is_none(),
            "stream without Done event should not be cached"
        );
    }

    #[tokio::test]
    async fn caching_adapter_model_info_delegates_to_inner() {
        let tmp = tempfile::tempdir().unwrap();
        let cache = ResponseCache::new(true, Some(tmp.path().into()));
        let inner = adapter(vec![]);
        let wrapped = CachingAdapter::new(inner, cache, false);
        let info = wrapped.model_info();
        assert_eq!(info.name, "test-model");
        assert_eq!(info.max_context_tokens, 4096);
    }

    #[tokio::test]
    async fn caching_adapter_set_json_mode_propagates_to_inner() {
        let tmp = tempfile::tempdir().unwrap();
        let cache = ResponseCache::new(true, Some(tmp.path().into()));
        let inner = adapter(vec![]);
        let mut wrapped = CachingAdapter::new(inner, cache, false);
        assert!(!wrapped.json_mode);
        wrapped.set_json_mode(true);
        assert!(wrapped.json_mode);
    }

    #[tokio::test]
    async fn caching_adapter_caches_with_json_mode_flag() {
        let tmp = tempfile::tempdir().unwrap();
        let cache = ResponseCache::new(true, Some(tmp.path().into()));
        let events = vec![
            StreamEvent::Text("hi".into()),
            StreamEvent::Done {
                finish_reason: FinishReason::Stop,
                usage: None,
            },
        ];
        let inner = adapter(events.clone());
        let wrapped = CachingAdapter::new(inner, cache, true);
        let messages: Vec<Message> = vec![];
        let tools: Vec<ToolDef> = vec![];
        let mut rx = wrapped.stream(&messages, &tools).await.unwrap();
        while let Some(_ev) = rx.recv().await {}
        let mut rx = wrapped.stream(&messages, &tools).await.unwrap();
        let mut got = Vec::new();
        while let Some(ev) = rx.recv().await {
            got.push(ev);
        }
        assert_eq!(got, events);
    }

    #[tokio::test]
    async fn maybe_wrap_cached_returns_cached_when_enabled() {
        let tmp = tempfile::tempdir().unwrap();
        let mut config = Config::default();
        config.model.cache_enabled = true;
        config.model.cache_dir = Some(tmp.path().into());
        let events = vec![
            StreamEvent::Text("hi".into()),
            StreamEvent::Done {
                finish_reason: FinishReason::Stop,
                usage: None,
            },
        ];
        let inner = adapter(events.clone());
        let wrapped = maybe_wrap_cached(inner, &config);
        let messages: Vec<Message> = vec![];
        let tools: Vec<ToolDef> = vec![];
        let mut rx = wrapped.stream(&messages, &tools).await.unwrap();
        while let Some(_ev) = rx.recv().await {}
        let mut rx = wrapped.stream(&messages, &tools).await.unwrap();
        let mut got = Vec::new();
        while let Some(ev) = rx.recv().await {
            got.push(ev);
        }
        assert_eq!(got, events);
    }

    #[tokio::test]
    async fn maybe_wrap_cached_returns_inner_when_disabled() {
        let mut config = Config::default();
        config.model.cache_enabled = false;
        let events = vec![StreamEvent::Text("x".into())];
        let inner = adapter(events.clone());
        let wrapped = maybe_wrap_cached(inner, &config);
        let messages: Vec<Message> = vec![];
        let tools: Vec<ToolDef> = vec![];
        let mut rx = wrapped.stream(&messages, &tools).await.unwrap();
        let mut got = Vec::new();
        while let Some(ev) = rx.recv().await {
            got.push(ev);
        }
        assert_eq!(got, events);
    }

    #[tokio::test]
    async fn maybe_wrap_cached_preserves_model_info() {
        let mut config = Config::default();
        config.model.cache_enabled = true;
        let inner = adapter(vec![]);
        let wrapped = maybe_wrap_cached(inner, &config);
        let info = wrapped.model_info();
        assert_eq!(info.name, "test-model");
    }
}
