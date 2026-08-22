use std::collections::{HashMap, VecDeque};
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

/// Insertion-ordered payload set with O(1) lookup and a running byte total.
struct StoreData {
    map: HashMap<String, String>,
    order: VecDeque<String>,
    total_bytes: u64,
}

impl StoreData {
    fn new() -> Self {
        Self {
            map: HashMap::new(),
            order: VecDeque::new(),
            total_bytes: 0,
        }
    }

    fn insert(&mut self, key: String, value: String) {
        let len = value.len() as u64;
        if let Some(old) = self.map.insert(key.clone(), value) {
            self.total_bytes = self
                .total_bytes
                .wrapping_sub(old.len() as u64)
                .wrapping_add(len);
            return;
        }
        self.order.push_back(key);
        self.total_bytes = self.total_bytes.wrapping_add(len);
    }

    fn remove(&mut self, key: &str) -> Option<String> {
        let value = self.map.remove(key)?;
        self.total_bytes = self.total_bytes.wrapping_sub(value.len() as u64);
        // ponytail: O(n) retain per remove; fine for cap~1000, swap to a
        // HashMap<key, index> + swap_remove if eviction volume gets heavy.
        self.order.retain(|k| k != key);
        Some(value)
    }

    fn get(&self, key: &str) -> Option<&String> {
        self.map.get(key)
    }

    fn len(&self) -> usize {
        self.map.len()
    }

    fn clear(&mut self) {
        self.map.clear();
        self.order.clear();
        self.total_bytes = 0;
    }
}

/// In-memory offload store keyed by the full 32-byte BLAKE3 hash.
#[must_use]
pub struct InMemoryOffloadStore {
    data: RwLock<StoreData>,
    max_entries: Option<usize>,
    max_bytes: Option<u64>,
}

impl InMemoryOffloadStore {
    /// Create a new empty in-memory store.
    ///
    /// # Examples
    ///
    /// ```
    /// use kf_compress_core::store::InMemoryOffloadStore;
    ///
    /// let store = InMemoryOffloadStore::new();
    /// assert!(store.is_empty());
    /// ```
    pub fn new() -> Self {
        Self {
            data: RwLock::new(StoreData::new()),
            max_entries: None,
            max_bytes: None,
        }
    }

    /// Create a new in-memory store that will evict the oldest entries
    /// whenever it exceeds `max_entries`. The cap is enforced on every
    /// [`Self::evict_if_over_cap`] call — callers should invoke it
    /// after inserts.
    // ponytail: per-session store, cap 1000 entries, FIFO eviction via
    // insertion-ordered VecDeque (was random HashMap order pre-WO 42.7).
    pub fn new_with_cap(max_entries: usize) -> Self {
        Self {
            data: RwLock::new(StoreData::new()),
            max_entries: Some(max_entries),
            max_bytes: None,
        }
    }

    /// Create a new in-memory store bounded by both an entry count and a
    /// total byte size. On every [`Self::evict_if_over_cap`] call the
    /// oldest entries are evicted until the store is under both caps.
    pub fn new_with_byte_cap(max_entries: usize, max_bytes: u64) -> Self {
        Self {
            data: RwLock::new(StoreData::new()),
            max_entries: Some(max_entries),
            max_bytes: Some(max_bytes),
        }
    }

    /// Remove the oldest entries when the store exceeds its entry cap
    /// and/or its byte cap. Call this after `put` to keep the store
    /// bounded. No-op when the store has no caps or is within limits.
    /// Eviction is FIFO: the front of the insertion order is dropped first.
    pub fn evict_if_over_cap(&self) {
        let max_entries = self.max_entries;
        let max_bytes = self.max_bytes;
        if max_entries.is_none() && max_bytes.is_none() {
            return;
        }
        let mut guard = match self.data.write() {
            Ok(g) => g,
            Err(poisoned) => {
                warn!("recovered offload store from poisoned write lock; continuing");
                poisoned.into_inner()
            }
        };
        while guard.len() > 0 {
            let over_entries = matches!(max_entries, Some(m) if guard.len() > m);
            let over_bytes = matches!(max_bytes, Some(m) if guard.total_bytes > m);
            if !over_entries && !over_bytes {
                return;
            }
            // FIFO: drop the oldest (front of insertion order).
            if let Some(key) = guard.order.front().cloned() {
                guard.remove(&key);
            } else {
                return;
            }
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
    /// use kf_compress_core::store::{InMemoryOffloadStore, OffloadStore};
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
        self.evict_if_over_cap();
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

    #[test]
    fn evict_if_over_cap_is_fifo() {
        let store = InMemoryOffloadStore::new_with_cap(2);
        let ka = store.put("alpha");
        let kb = store.put("beta");
        let kc = store.put("gamma");
        // cap=2, so after the third put + eviction the oldest ("alpha") is gone.
        assert_eq!(store.get(&ka), None, "oldest entry must be evicted first");
        assert!(store.get(&kb).is_some(), "newer entries survive");
        assert!(store.get(&kc).is_some(), "newest entry survives");
        assert_eq!(store.len(), 2);
    }

    #[test]
    fn byte_cap_evicts_oldest_first() {
        // 3 distinct entries of 100 bytes each, cap 250 bytes => the oldest is
        // evicted to bring the total under the cap (200 <= 250).
        let store = InMemoryOffloadStore::new_with_byte_cap(100, 250);
        let payload = |n: usize| "x".repeat(100) + &n.to_string();
        let ka = store.put(&payload(0));
        let kb = store.put(&payload(1));
        let kc = store.put(&payload(2));
        // total after 3 inserts = ~300 > 250 => evict oldest.
        assert_eq!(store.get(&ka), None, "oldest evicted by byte cap");
        assert!(store.get(&kb).is_some());
        assert!(store.get(&kc).is_some());
        assert!(store.len() <= 2, "byte cap reduces entry count");
    }

    #[test]
    fn byte_cap_evicts_multiple_until_under_cap() {
        // cap 150 bytes, entries ~100 bytes each: need to evict down to 1.
        let store = InMemoryOffloadStore::new_with_byte_cap(100, 150);
        let payload = |n: usize| "y".repeat(100) + &n.to_string();
        let ka = store.put(&payload(0));
        let kb = store.put(&payload(1));
        let kc = store.put(&payload(2));
        assert_eq!(store.get(&ka), None, "oldest evicted");
        assert_eq!(store.get(&kb), None, "second-oldest also evicted");
        assert!(store.get(&kc).is_some(), "newest survives");
        assert_eq!(store.len(), 1);
    }

    #[test]
    fn both_caps_enforced_together() {
        // entry cap 2 binds before byte cap could ever bind (byte cap is huge).
        let store = InMemoryOffloadStore::new_with_byte_cap(2, u64::MAX);
        let ka = store.put("first");
        let _kb = store.put("second");
        let _kc = store.put("third");
        assert_eq!(store.get(&ka), None, "entry cap evicts oldest");
        assert_eq!(store.len(), 2);

        // now flip: tiny byte cap binds even with one entry.
        let store = InMemoryOffloadStore::new_with_byte_cap(100, 3);
        let ka = store.put("abc");
        let _kb = store.put("def");
        // 6 bytes > 3 => evict oldest.
        assert_eq!(store.get(&ka), None);
        assert_eq!(store.len(), 1);
    }

    #[test]
    fn duplicate_put_does_not_grow_order() {
        let store = InMemoryOffloadStore::new_with_cap(2);
        let _ = store.put("dup");
        let _ = store.put("dup");
        assert_eq!(
            store.len(),
            1,
            "duplicate key does not create a second slot"
        );
    }
}
