//! Content-addressed cache for model response streams, with an adapter wrapper.
//!
//! Caching is opt-in via `Config::cache_enabled`. When enabled, every
//! successful stream is serialized to disk under `cache_dir` keyed by a
//! hash of the request fingerprint (provider/endpoint scope + model +
//! generation config: seed, max_tokens, extended_thinking, budget_tokens,
//! computer_use dims — WO 47.20) plus `messages_hash, tools_hash,
//! json_mode`. On a subsequent identical request the cached
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

// ponytail: simple count-based cap, not true LRU. Adequate for a hot-key
// layer backed by disk; eviction is arbitrary (HashMap order). Upgrade
// path: an `IndexMap` + move-to-back on get for real LRU if hit-rate
// measurement ever shows arbitrary eviction hurting (WO 46.23).
const MAX_MEMORY_ENTRIES: usize = 100;

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

    /// Look up a cached stream. `model_scope` is the caller's canonical
    /// request fingerprint (model identity + generation config + provider
    /// routing — see `CachingAdapter::request_fingerprint`); it is hashed
    /// together with the messages/tools/response_format.
    pub fn get(
        &self,
        model_scope: &str,
        messages: &[Message],
        tools: &[ToolDef],
        response_format: Option<&crate::shared::ResponseFormat>,
    ) -> Option<Vec<StreamEvent>> {
        if !self.enabled {
            return None;
        }
        let key = CacheKey::new(model_scope, messages, tools, response_format);

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
        self.insert_memory(key, events.clone());
        Some(events)
    }

    /// Store a stream in the cache. `model_scope` as in [`get`](Self::get).
    pub fn put(
        &self,
        model_scope: &str,
        messages: &[Message],
        tools: &[ToolDef],
        response_format: Option<&crate::shared::ResponseFormat>,
        events: &[StreamEvent],
    ) {
        if !self.enabled {
            return;
        }
        let key = CacheKey::new(model_scope, messages, tools, response_format);

        // Never cache error-carrying or empty streams (WO 38.5). A
        // single Error event anywhere means the stream is replayable
        // poison — previously only all-Error streams were skipped while
        // mixed streams (error + a synthesized Done) were cached and
        // replayed forever.
        if events.is_empty() || events.iter().any(|e| matches!(e, StreamEvent::Error(_))) {
            return;
        }

        // Clone the data out of the lock first, then do ALL disk I/O
        // (create_dir_all + to_vec + fs::write) OUTSIDE the memory lock.
        // Holding the sync Mutex across fs::write lets a disk stall (NFS,
        // full disk) block every subsequent stream call, and a panic in
        // to_vec would poison the mutex (WO 46.23).
        self.insert_memory(key.clone(), events.to_vec());

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

    // Insert into the memory map under a short-lived lock, enforcing the
    // size cap. Disk I/O must NEVER happen while this lock is held (WO 46.23).
    fn insert_memory(&self, key: CacheKey, events: Vec<StreamEvent>) {
        let mut mem = self.memory.lock().unwrap_or_else(|e| e.into_inner());
        // ponytail: arbitrary eviction (HashMap order), not LRU. See
        // MAX_MEMORY_ENTRIES comment for upgrade path.
        while mem.len() >= MAX_MEMORY_ENTRIES {
            let drop = mem.keys().next().cloned();
            match drop {
                Some(k) => {
                    mem.remove(&k);
                }
                None => break,
            }
        }
        mem.insert(key, events);
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
    fn new(
        model_scope: &str,
        messages: &[Message],
        tools: &[ToolDef],
        response_format: Option<&crate::shared::ResponseFormat>,
    ) -> Self {
        let mut hasher = Sha256::new();
        hasher.update(model_scope.as_bytes());

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
        if let Some(rf) = response_format {
            hasher.update(serde_json::to_vec(rf).unwrap_or_default());
        }

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
        // The provider/endpoint routing scope: everything in Config that
        // decides WHICH endpoint serves the model. Part of the cache key
        // so two providers serving the same model name don't share
        // entries (WO 47.20).
        let scope = format!(
            "provider={}\0ollama={}\0zen={}\0anthropic_base={}\0aws_region={}\0gcp_project={}\0gcp_region={}",
            config.model.anthropic_provider,
            config.model.ollama_host,
            config.model.opencode_zen_endpoint,
            config.model.anthropic_api_base,
            config.model.aws_region,
            config.model.gcp_project_id,
            config.model.gcp_region,
        );
        let mut wrapped = CachingAdapter::new(adapter, cache, config.model.json_mode);
        wrapped.set_request_scope(scope);
        Box::new(wrapped)
    } else {
        adapter
    }
}

/// Wrapper that adds response caching to any [`ModelAdapter`].
pub struct CachingAdapter {
    inner: Box<dyn ModelAdapter>,
    cache: ResponseCache,
    json_mode: bool,
    response_format: Option<crate::shared::ResponseFormat>,
    // WO 47.20: request-shaping knobs the executor pushes via set_* after
    // wrapping, plus the provider/endpoint scope pinned at wrap time. All
    // are folded into the cache key — a request with different generation
    // config must never replay another request's cached stream, and two
    // providers serving the same model name must not share entries.
    scope: String,
    seed: Option<u64>,
    max_tokens: u32,
    extended_thinking: bool,
    budget_tokens: usize,
    computer_use_dims: Option<(u32, u32)>,
}

impl CachingAdapter {
    /// Wrap an existing adapter with a cache.
    pub fn new(inner: Box<dyn ModelAdapter>, cache: ResponseCache, json_mode: bool) -> Self {
        Self {
            inner,
            cache,
            json_mode,
            response_format: if json_mode {
                Some(crate::shared::ResponseFormat::JsonObject)
            } else {
                None
            },
            scope: String::new(),
            seed: None,
            max_tokens: 0,
            extended_thinking: false,
            budget_tokens: 0,
            computer_use_dims: None,
        }
    }

    /// Pin the provider/endpoint routing scope folded into the cache key.
    /// Set by [`maybe_wrap_cached`] from `Config`; empty for hand-built
    /// wrappers (tests) where all wrappers share one scope anyway.
    pub fn set_request_scope(&mut self, scope: String) {
        self.scope = scope;
    }

    // Canonical fingerprint of everything request-shaping that is NOT
    // messages/tools/response_format. \0-separated so adjacent values
    // can't collide by concatenation. WO 47.20.
    fn request_fingerprint(&self) -> String {
        format!(
            "{}\0{}\0seed={:?}\0max_tokens={}\0thinking={}\0budget={}\0computer_use={:?}",
            self.scope,
            self.inner.model_info().name,
            self.seed,
            self.max_tokens,
            self.extended_thinking,
            self.budget_tokens,
            self.computer_use_dims,
        )
    }
}

#[async_trait::async_trait]
impl ModelAdapter for CachingAdapter {
    fn model_info(&self) -> ModelInfo {
        self.inner.model_info()
    }

    fn set_json_mode(&mut self, json_mode: bool) {
        self.json_mode = json_mode;
        if json_mode {
            self.response_format = Some(crate::shared::ResponseFormat::JsonObject);
        }
        self.inner.set_json_mode(json_mode);
    }
    fn set_response_format(&mut self, format: crate::shared::ResponseFormat) {
        self.inner.set_response_format(format.clone());
        self.response_format = Some(format);
    }
    fn set_extended_thinking(&mut self, enabled: bool) {
        self.extended_thinking = enabled;
        self.inner.set_extended_thinking(enabled);
    }

    fn set_budget_tokens(&mut self, budget: usize) {
        self.budget_tokens = budget;
        self.inner.set_budget_tokens(budget);
    }

    fn set_seed(&mut self, seed: Option<u64>) {
        self.seed = seed;
        self.inner.set_seed(seed);
    }

    fn set_max_tokens(&mut self, max_tokens: u32) {
        self.max_tokens = max_tokens;
        self.inner.set_max_tokens(max_tokens);
    }

    fn set_streaming_timeout(&mut self, secs: u64) {
        self.inner.set_streaming_timeout(secs);
    }

    fn set_computer_use_dims(&mut self, dims: Option<(u32, u32)>) {
        self.computer_use_dims = dims;
        self.inner.set_computer_use_dims(dims);
    }

    async fn stream(
        &self,
        messages: &[Message],
        tools: &[ToolDef],
    ) -> anyhow::Result<tokio::sync::mpsc::Receiver<StreamEvent>> {
        let model_info = self.inner.model_info();
        let fingerprint = self.request_fingerprint();

        if let Some(events) =
            self.cache
                .get(&fingerprint, messages, tools, self.response_format.as_ref())
        {
            tracing::info!(
                model = %model_info.name,
                events = events.len(),
                "response cache hit; replaying without billing"
            );
            let (tx, rx) = tokio::sync::mpsc::channel::<StreamEvent>(events.len().max(1));
            tokio::spawn(async move {
                for ev in events {
                    // WO 38.5: zero the usage on replay — a cache hit
                    // costs nothing, and re-billing the ORIGINAL turn's
                    // tokens every replay double-counts in CostStats.
                    // Usage=None means no CostStats event fires at all.
                    let ev = match ev {
                        StreamEvent::Done {
                            finish_reason,
                            usage: Some(_),
                        } => StreamEvent::Done {
                            finish_reason,
                            usage: None,
                        },
                        other => other,
                    };
                    if tx.send(ev).await.is_err() {
                        break;
                    }
                }
            });
            return Ok(rx);
        }

        let rx = self.inner.stream(messages, tools).await?;
        let cache = self.cache.clone();
        let fingerprint = fingerprint.clone();
        let model_name = model_info.name.clone();
        let messages_owned = messages.to_vec();
        let tools_owned = tools.to_vec();
        let response_format = self.response_format.clone();

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
            // Only cache complete, clean streams (WO 38.5): the final
            // event must be Done with a non-Error reason AND no Error
            // event anywhere. A dropped consumer, a cancelled turn, an
            // adapter that exits without a terminal event, or a
            // truncated stream (which now ends Done{Error}) would
            // otherwise poison the cache.
            let complete = matches!(
                events.last(),
                Some(StreamEvent::Done { finish_reason, .. })
                    if finish_reason != &crate::shared::FinishReason::Error
            ) && !events.iter().any(|e| matches!(e, StreamEvent::Error(_)));
            if complete {
                cache.put(
                    &fingerprint,
                    &messages_owned,
                    &tools_owned,
                    response_format.as_ref(),
                    &events,
                );
            } else {
                tracing::trace!(
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
            None,
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
                    cache_write_tokens: None,
                }),
            },
        ];

        cache.put(
            "test-model",
            &[message(crate::shared::Role::User, "hi")],
            &[],
            None,
            &events,
        );

        let got = cache
            .get(
                "test-model",
                &[message(crate::shared::Role::User, "hi")],
                &[],
                None,
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
            None,
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
                None
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
            None,
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
            None,
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

    /// WO 38.5: one Error event anywhere disqualifies the stream, even
    /// when a Done follows (mixed streams used to be cached).
    #[test]
    fn cache_skips_streams_containing_any_error() {
        let dir = tempfile::tempdir().unwrap();
        let cache = ResponseCache::new(true, Some(dir.path().into()));
        cache.put(
            "test-model",
            &[message(crate::shared::Role::User, "hi")],
            &[],
            None,
            &[
                StreamEvent::Text("partial".into()),
                StreamEvent::Error("reset".into()),
                StreamEvent::Done {
                    finish_reason: FinishReason::Stop,
                    usage: None,
                },
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
            None,
        );
        let k2 = CacheKey::new(
            "model",
            &[message(crate::shared::Role::User, "hi")],
            &[],
            None,
        );
        assert_eq!(k1, k2);
    }

    #[test]
    fn cache_key_differs_for_different_models() {
        let k1 = CacheKey::new(
            "model-a",
            &[message(crate::shared::Role::User, "hi")],
            &[],
            None,
        );
        let k2 = CacheKey::new(
            "model-b",
            &[message(crate::shared::Role::User, "hi")],
            &[],
            None,
        );
        assert_ne!(k1, k2);
    }

    #[test]
    fn cache_key_differs_for_different_messages() {
        let k1 = CacheKey::new(
            "model",
            &[message(crate::shared::Role::User, "hi")],
            &[],
            None,
        );
        let k2 = CacheKey::new(
            "model",
            &[message(crate::shared::Role::User, "bye")],
            &[],
            None,
        );
        assert_ne!(k1, k2);
    }

    #[test]
    fn cache_key_differs_for_different_response_format() {
        let rf = crate::shared::ResponseFormat::JsonObject;
        let k1 = CacheKey::new(
            "model",
            &[message(crate::shared::Role::User, "hi")],
            &[],
            None,
        );
        let k2 = CacheKey::new(
            "model",
            &[message(crate::shared::Role::User, "hi")],
            &[],
            Some(&rf),
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
            None,
        );
        let k2 = CacheKey::new(
            "model",
            &[message(crate::shared::Role::User, "hi")],
            &[tool_b],
            None,
        );
        assert_ne!(k1, k2);
    }

    #[test]
    fn cache_key_hash_is_hex_string() {
        let k = CacheKey::new(
            "model",
            &[message(crate::shared::Role::User, "hi")],
            &[],
            None,
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
            None,
            &events,
        );
        let got = cache
            .get(
                "test-model",
                &[message(crate::shared::Role::User, "hi")],
                &[],
                None,
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
            None,
            &events,
        );
        let cache2 = ResponseCache::new(true, Some(dir.path().into()));
        let got = cache2
            .get(
                "test-model",
                &[message(crate::shared::Role::User, "hi")],
                &[],
                None,
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
            None,
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
            None,
        );
        let path = cache.path_for(&key);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"not valid serde_json").unwrap();
        assert!(cache
            .get(
                "model",
                &[message(crate::shared::Role::User, "hi")],
                &[],
                None
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
            None,
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
                None
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

    /// WO 46.23: the in-memory map is capped — inserting more distinct
    /// keys than MAX_MEMORY_ENTRIES must not grow the map beyond the cap.
    /// The newest entry is always kept.
    #[test]
    fn cache_memory_respects_size_cap() {
        let dir = tempfile::tempdir().unwrap();
        let cache = ResponseCache::new(true, Some(dir.path().into()));
        let events = vec![StreamEvent::Text("x".into())];
        for i in 0..(MAX_MEMORY_ENTRIES + 20) {
            cache.put(
                "test-model",
                &[message(crate::shared::Role::User, &format!("msg{i}"))],
                &[],
                None,
                &events,
            );
        }
        let mem = cache.memory.lock().unwrap_or_else(|e| e.into_inner());
        assert!(
            mem.len() <= MAX_MEMORY_ENTRIES,
            "memory map grew to {} (cap {})",
            mem.len(),
            MAX_MEMORY_ENTRIES
        );
        // The most-recently inserted key must still be present.
        let last_key = CacheKey::new(
            "test-model",
            &[message(
                crate::shared::Role::User,
                &format!("msg{}", MAX_MEMORY_ENTRIES + 19),
            )],
            &[],
            None,
        );
        assert!(
            mem.contains_key(&last_key),
            "newest entry must survive eviction"
        );
    }

    /// WO 47.20: every generation knob pushed via set_* must reach the
    /// cache key — a fingerprint that ignores any of them lets a request
    /// with different config replay another request's cached stream.
    #[test]
    fn caching_adapter_generation_knobs_change_the_fingerprint() {
        let tmp = tempfile::tempdir().unwrap();
        let cache = ResponseCache::new(true, Some(tmp.path().into()));
        let mut wrapped = CachingAdapter::new(adapter(vec![]), cache, false);
        let base = wrapped.request_fingerprint();
        wrapped.set_seed(Some(1));
        let seeded = wrapped.request_fingerprint();
        assert_ne!(base, seeded, "seed must be in the key");
        wrapped.set_max_tokens(64000);
        assert_ne!(
            seeded,
            wrapped.request_fingerprint(),
            "max_tokens must be in the key"
        );
        wrapped.set_extended_thinking(true);
        assert_ne!(
            base,
            wrapped.request_fingerprint(),
            "extended_thinking must be in the key"
        );
        wrapped.set_budget_tokens(10000);
        assert_ne!(
            base,
            wrapped.request_fingerprint(),
            "budget_tokens must be in the key"
        );
        wrapped.set_computer_use_dims(Some((1024, 768)));
        assert_ne!(
            base,
            wrapped.request_fingerprint(),
            "computer_use dims must be in the key"
        );
    }

    fn done_event() -> StreamEvent {
        StreamEvent::Done {
            finish_reason: FinishReason::Stop,
            usage: None,
        }
    }

    /// WO 47.20: request B with a different seed must not replay request
    /// A's cached response even though messages/tools/model are identical.
    #[tokio::test]
    async fn caching_adapter_different_seed_does_not_replay_cached_stream() {
        let tmp = tempfile::tempdir().unwrap();
        let cache = ResponseCache::new(true, Some(tmp.path().into()));
        let events_a = vec![StreamEvent::Text("from-plain".into()), done_event()];
        let events_b = vec![StreamEvent::Text("from-seeded".into()), done_event()];
        let messages: Vec<Message> = vec![];
        let tools: Vec<ToolDef> = vec![];

        // Request A: no seed — populates the cache.
        let wrapped_a = CachingAdapter::new(adapter(events_a), cache.clone(), false);
        let mut rx = wrapped_a.stream(&messages, &tools).await.unwrap();
        while (rx.recv().await).is_some() {}

        // Request B: same model/messages/tools, different generation config.
        let mut wrapped_b = CachingAdapter::new(adapter(events_b.clone()), cache.clone(), false);
        wrapped_b.set_seed(Some(7));
        let mut rx = wrapped_b.stream(&messages, &tools).await.unwrap();
        let mut got = Vec::new();
        while let Some(ev) = rx.recv().await {
            got.push(ev);
        }
        assert_eq!(
            got, events_b,
            "different seed must miss the cache and hit the inner adapter"
        );
    }

    /// WO 47.20: two providers serving the same model name (different
    /// request scope) must not share cache entries.
    #[tokio::test]
    async fn caching_adapter_different_scope_does_not_replay_cached_stream() {
        let tmp = tempfile::tempdir().unwrap();
        let cache = ResponseCache::new(true, Some(tmp.path().into()));
        let events_a = vec![StreamEvent::Text("from-provider-a".into()), done_event()];
        let events_b = vec![StreamEvent::Text("from-provider-b".into()), done_event()];
        let messages: Vec<Message> = vec![];
        let tools: Vec<ToolDef> = vec![];

        let mut wrapped_a = CachingAdapter::new(adapter(events_a), cache.clone(), false);
        wrapped_a.set_request_scope("provider=a".into());
        let mut rx = wrapped_a.stream(&messages, &tools).await.unwrap();
        while (rx.recv().await).is_some() {}

        let mut wrapped_b = CachingAdapter::new(adapter(events_b.clone()), cache.clone(), false);
        wrapped_b.set_request_scope("provider=b".into());
        let mut rx = wrapped_b.stream(&messages, &tools).await.unwrap();
        let mut got = Vec::new();
        while let Some(ev) = rx.recv().await {
            got.push(ev);
        }
        assert_eq!(
            got, events_b,
            "different provider scope must miss the cache and hit the inner adapter"
        );
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
                    cache_write_tokens: None,
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

        // WO 38.5: replay zeroes the usage — a cache hit must not
        // re-bill the original turn's tokens in CostStats. This test
        // previously pinned the re-billing behavior verbatim.
        let mut rx = wrapped.stream(&messages, &tools).await.unwrap();
        let mut got = Vec::new();
        while let Some(ev) = rx.recv().await {
            got.push(ev);
        }
        assert_eq!(got.len(), events.len());
        match got.last() {
            Some(StreamEvent::Done { usage, .. }) => {
                assert_eq!(usage, &None, "replay must not re-bill usage");
            }
            other => panic!("expected Done on replay, got {other:?}"),
        }
        assert_eq!(got[0], events[0]);
    }

    /// WO 38.5: a stream that contains an Error event but ends with a
    /// synthesized Done must not be cached — replaying it would replay
    /// the error forever. (Anthropic used to always synthesize Done,
    /// so mixed error streams passed the old last-is-Done check.)
    #[tokio::test]
    async fn caching_adapter_skips_stream_containing_error() {
        let tmp = tempfile::tempdir().unwrap();
        let cache = ResponseCache::new(true, Some(tmp.path().into()));
        let events = vec![
            StreamEvent::Text("partial".into()),
            StreamEvent::Error("transport reset".into()),
            StreamEvent::Done {
                finish_reason: FinishReason::Stop,
                usage: None,
            },
        ];
        let inner = adapter(events.clone());
        let wrapped = CachingAdapter::new(inner, cache.clone(), false);
        let messages: Vec<Message> = vec![];
        let tools: Vec<ToolDef> = vec![];
        let mut rx = wrapped.stream(&messages, &tools).await.unwrap();
        while let Some(_ev) = rx.recv().await {}
        for _ in 0..8 {
            tokio::task::yield_now().await;
        }
        assert!(
            cache
                .get(&wrapped.request_fingerprint(), &messages, &tools, None)
                .is_none(),
            "error-carrying stream must not be cached"
        );
    }

    /// WO 38.5: Done{Error} (the truncation terminal since WO 38.5) is
    /// not a complete stream — it must not be cached.
    #[tokio::test]
    async fn caching_adapter_skips_done_with_error_reason() {
        let tmp = tempfile::tempdir().unwrap();
        let cache = ResponseCache::new(true, Some(tmp.path().into()));
        let events = vec![
            StreamEvent::Text("half a reply".into()),
            StreamEvent::Done {
                finish_reason: FinishReason::Error,
                usage: None,
            },
        ];
        let inner = adapter(events.clone());
        let wrapped = CachingAdapter::new(inner, cache.clone(), false);
        let messages: Vec<Message> = vec![];
        let tools: Vec<ToolDef> = vec![];
        let mut rx = wrapped.stream(&messages, &tools).await.unwrap();
        while let Some(_ev) = rx.recv().await {}
        for _ in 0..8 {
            tokio::task::yield_now().await;
        }
        assert!(
            cache
                .get(&wrapped.request_fingerprint(), &messages, &tools, None)
                .is_none(),
            "Done{{Error}} stream must not be cached"
        );
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

        // Let the forwarder task observe the closed receiver and abort. The
        // forwarder's `select!` breaks on `tx_out.closed()`. Yielding a
        // handful of times lets the runtime poll the forwarder to completion
        // (it drains the inner channel to None, breaks, sees no Done event,
        // and skips the cache write). Replaces a 50ms wall-clock sleep.
        for _ in 0..8 {
            tokio::task::yield_now().await;
        }

        // The cache must remain empty for this key; a truncated stream
        // must not be replayed on a later identical request.
        assert!(
            cache
                .get(&wrapped.request_fingerprint(), &messages, &tools, None)
                .is_none(),
            "partial stream should not be cached"
        );
    }

    #[tokio::test]
    async fn caching_adapter_aborts_forwarder_on_consumer_drop() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc;
        // Inner adapter emits events one at a time, gated by a Notify so the
        // test controls exactly when each event fires — no wall-clock sleep.
        // `emitted` counts how many events the forwarder drained. The test
        // allows exactly one event through, receives it, drops the consumer,
        // and the forwarder's `tx_out.closed()` arm aborts the pull loop.
        let emitted = Arc::new(AtomicUsize::new(0));
        let gate = Arc::new(tokio::sync::Notify::new());
        struct CountingAdapter {
            emitted: Arc<AtomicUsize>,
            gate: Arc<tokio::sync::Notify>,
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
                let gate = self.gate.clone();
                let n = self.n;
                tokio::spawn(async move {
                    for i in 0..n {
                        gate.notified().await;
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
            gate: gate.clone(),
            n: 10,
        });
        let wrapped = CachingAdapter::new(inner, cache, false);
        let messages: Vec<Message> = vec![];
        let tools: Vec<ToolDef> = vec![];

        let mut rx = wrapped.stream(&messages, &tools).await.unwrap();
        // Allow exactly one event through, receive it, then drop the consumer.
        // The forwarder's select! breaks on tx_out.closed(); the spawned
        // task's next gate.notified().await never fires, so emitted stays at 1.
        gate.notify_one();
        let _first = rx.recv().await;
        drop(rx);

        // Yield a few times to let the forwarder task observe the closed
        // receiver and abort. No wall-clock sleep needed — the forwarder
        // breaks on the next poll after tx_out closes.
        for _ in 0..8 {
            tokio::task::yield_now().await;
        }

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
        // The stream completed normally (inner closed). The forwarder breaks
        // when `inner.recv()` returns None, then checks for a Done event and
        // skips the cache write. Yield a few times to let the forwarder task
        // finish its post-loop logic before asserting. Replaces a 50ms sleep.
        for _ in 0..8 {
            tokio::task::yield_now().await;
        }
        assert!(
            cache
                .get(&wrapped.request_fingerprint(), &messages, &tools, None)
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
