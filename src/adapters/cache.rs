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
        assert_eq!(k.hash.len(), 16);
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
}
