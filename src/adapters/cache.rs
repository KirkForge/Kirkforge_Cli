//! Content-addressed cache for model response streams.
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

use crate::shared::{Message, StreamEvent, ToolDef};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
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
        let mut hasher = DefaultHasher::new();
        model.hash(&mut hasher);

        // Serialize deterministically for hashing.
        if let Ok(bytes) = serde_json::to_vec(messages) {
            bytes.hash(&mut hasher);
        }
        if let Ok(bytes) = serde_json::to_vec(
            &tools
                .iter()
                .map(|t| (t.name, t.description, &t.parameters))
                .collect::<Vec<_>>(),
        ) {
            bytes.hash(&mut hasher);
        }
        json_mode.hash(&mut hasher);

        Self {
            hash: format!("{:016x}", hasher.finish()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shared::{FinishReason, TokenUsage};

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
            "qwen2.5:3b",
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
            "qwen2.5:3b",
            &[message(crate::shared::Role::User, "hi")],
            &[],
            false,
            &events,
        );

        let got = cache
            .get(
                "qwen2.5:3b",
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
            "qwen2.5:3b",
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
}
