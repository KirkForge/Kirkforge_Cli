use std::collections::HashMap;
use std::fmt;
use std::sync::RwLock;
use tracing::warn;

/// Backend that stores offloaded payloads and returns stable reference keys.
pub trait OffloadStore: Send + Sync {
    /// Store `payload` and return a key that can be used to retrieve it.
    #[must_use]
    fn put(&self, payload: &str) -> String;
    /// Retrieve a previously stored payload by `key`.
    #[must_use]
    fn get(&self, key: &str) -> Option<String>;
    /// Number of distinct payloads currently stored.
    #[must_use]
    fn len(&self) -> usize;
    /// Whether the store contains no payloads.
    #[must_use]
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
    /// Remove all stored payloads.
    ///
    /// The default implementation is a no-op for stores that do not support
    /// reset; override it when the backend can be cleared.
    fn clear(&self) {}
    /// Short identifier used in logs and dry-run reports.
    #[must_use]
    fn backend_name(&self) -> &'static str;
}

/// In-memory offload store keyed by the full 32-byte BLAKE3 hash.
#[must_use]
pub struct InMemoryOffloadStore {
    data: RwLock<HashMap<String, String>>,
}

impl InMemoryOffloadStore {
    /// Create a new empty in-memory store.
    ///
    /// # Examples
    ///
    /// ```
    /// use kirkstratum_core::store::InMemoryOffloadStore;
    ///
    /// let store = InMemoryOffloadStore::new();
    /// assert!(store.is_empty());
    /// ```
    pub fn new() -> Self {
        Self {
            data: RwLock::new(HashMap::new()),
        }
    }
}

impl Default for InMemoryOffloadStore {
    fn default() -> Self {
        Self::new()
    }
}

impl InMemoryOffloadStore {
    /// Number of distinct payloads currently stored.
    ///
    /// Available as an inherent method so callers do not need to import the
    /// [`OffloadStore`] trait for the common case.
    #[must_use]
    pub fn len(&self) -> usize {
        OffloadStore::len(self)
    }

    /// Whether the store contains no payloads.
    ///
    /// Available as an inherent method so callers do not need to import the
    /// [`OffloadStore`] trait for the common case.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        OffloadStore::is_empty(self)
    }

    /// Remove all stored payloads.
    ///
    /// # Examples
    ///
    /// ```
    /// use kirkstratum_core::store::{InMemoryOffloadStore, OffloadStore};
    ///
    /// let store = InMemoryOffloadStore::new();
    /// let key = store.put("hello");
    /// assert_eq!(store.len(), 1);
    ///
    /// store.clear();
    /// assert!(store.is_empty());
    /// assert_eq!(store.get(&key), None);
    /// ```
    pub fn clear(&self) {
        match self.data.write() {
            Ok(mut guard) => guard.clear(),
            Err(poisoned) => {
                warn!("recovered offload store from poisoned write lock; continuing");
                poisoned.into_inner().clear();
            }
        }
    }
}

impl fmt::Debug for InMemoryOffloadStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("InMemoryOffloadStore")
            .field("backend", &self.backend_name())
            .field("len", &self.len())
            .finish()
    }
}

impl OffloadStore for InMemoryOffloadStore {
    fn put(&self, payload: &str) -> String {
        let key = derive_key(payload);
        match self.data.write() {
            Ok(mut guard) => {
                guard.insert(key.clone(), payload.to_string());
            }
            Err(poisoned) => {
                warn!("recovered offload store from poisoned write lock; continuing");
                let mut guard = poisoned.into_inner();
                guard.insert(key.clone(), payload.to_string());
            }
        }
        key
    }

    fn get(&self, key: &str) -> Option<String> {
        match self.data.read() {
            Ok(guard) => guard.get(key).cloned(),
            Err(poisoned) => {
                warn!("recovered offload store from poisoned read lock; continuing");
                poisoned.into_inner().get(key).cloned()
            }
        }
    }

    fn len(&self) -> usize {
        match self.data.read() {
            Ok(guard) => guard.len(),
            Err(poisoned) => {
                warn!("recovered offload store from poisoned read lock; continuing");
                poisoned.into_inner().len()
            }
        }
    }

    fn backend_name(&self) -> &'static str {
        "memory"
    }
}

fn derive_key(payload: &str) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(payload.as_bytes());
    // Use the full 64-character BLAKE3 hash so the key is effectively
    // collision-free. This keeps the in-memory store from ever overwriting
    // distinct payloads due to a prefix collision.
    hasher.finalize().to_hex().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inmemory_store_roundtrips() {
        let store = InMemoryOffloadStore::new();
        let key = store.put("hello world");
        assert_eq!(store.get(&key), Some("hello world".to_string()));
    }

    #[test]
    fn duplicate_payload_shares_key() {
        let store = InMemoryOffloadStore::new();
        let a = store.put("duplicate");
        let b = store.put("duplicate");
        assert_eq!(a, b);
        assert_eq!(store.len(), 1);
    }

    #[test]
    fn store_recovers_from_poisoned_lock() {
        let store = InMemoryOffloadStore::new();

        // Poison the lock by panicking while holding the write guard.
        let result = std::panic::catch_unwind(|| {
            let _guard = store.data.write().expect("lock is fresh");
            panic!("intentional panic to poison the lock")
        });
        assert!(result.is_err(), "panic should have poisoned the lock");

        // Recovery path: put/get/len should still work after poisoning.
        let key = store.put("after poison");
        assert_eq!(store.get(&key), Some("after poison".to_string()));
        assert_eq!(store.len(), 1);
    }

    #[test]
    fn inherent_len_and_is_empty_delegate_to_trait() {
        let store = InMemoryOffloadStore::new();
        assert!(store.is_empty());
        assert_eq!(store.len(), 0);

        let _key = store.put("hello");
        assert!(!store.is_empty());
        assert_eq!(store.len(), 1);
    }

    #[test]
    fn clear_removes_all_payloads() {
        let store = InMemoryOffloadStore::new();
        let key = store.put("hello");
        let _ = store.put("world");
        assert_eq!(store.len(), 2);

        store.clear();
        assert!(store.is_empty());
        assert_eq!(store.len(), 0);
        assert_eq!(store.get(&key), None);
    }

    #[test]
    fn clear_recover_from_poisoned_lock() {
        let store = InMemoryOffloadStore::new();
        let _ = store.put("before");

        let result = std::panic::catch_unwind(|| {
            let _guard = store.data.write().expect("lock is fresh");
            panic!("intentional panic to poison the lock")
        });
        assert!(result.is_err(), "panic should have poisoned the lock");

        store.clear();
        assert!(store.is_empty());
        assert_eq!(store.len(), 0);
    }

    #[test]
    fn store_debug_shows_backend_and_len() {
        let store = InMemoryOffloadStore::new();
        assert!(format!("{store:?}").contains("backend: \"memory\""));
        assert!(format!("{store:?}").contains("len: 0"));

        let _key = store.put("hello world");
        let debug = format!("{store:?}");
        assert!(debug.contains("backend: \"memory\""));
        assert!(debug.contains("len: 1"));
        assert!(debug.starts_with("InMemoryOffloadStore {"));
    }
}
