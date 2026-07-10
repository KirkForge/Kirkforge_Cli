//! `OffloadStore` — content-addressed key/value store for the middle of
//! sliced tool outputs. Per ADR-0004. Two backends: in-memory and file.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;

pub const SLICE_MARKER_PREFIX: &str = "<<plugin3:slice:";
pub const SLICE_MARKER_SUFFIX: &str = ">>";

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("invalid key: {0}")]
    InvalidKey(String),
    #[error("key not found: {0}")]
    NotFound(String),
    #[error("backend error: {0}")]
    Backend(String),
}

pub trait OffloadStore: Send + Sync {
    /// Store `bytes` and return the derived content key.
    ///
    /// # Errors
    ///
    /// Returns `StoreError::Backend` if the underlying backend cannot
    /// persist the data (e.g. disk full, permission denied).
    fn put(&self, bytes: &[u8]) -> Result<String, StoreError>;
    /// Retrieve the payload for `key`.
    ///
    /// # Errors
    ///
    /// Returns `StoreError::InvalidKey` if `key` is not a 24-hex
    /// string, or `StoreError::NotFound` if no payload exists for the
    /// key. Other I/O failures map to `StoreError::Backend`.
    fn get(&self, key: &str) -> Result<Vec<u8>, StoreError>;
    fn len(&self) -> usize;
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
    fn backend_name(&self) -> &'static str;
}

/// 24 hex chars (96 bits) — ADR-0004. Byte-compatible with Stratum.
#[must_use]
pub fn make_key(bytes: &[u8]) -> String {
    let hash = blake3::hash(bytes);
    let hex = hash.to_hex();
    hex.as_str()[..24].to_string()
}

/// Validate that `key` is exactly 24 ASCII hex characters.
///
/// # Errors
///
/// Returns `StoreError::InvalidKey` when the length or character set
/// does not match the ADR-0004 contract.
pub fn validate_key(key: &str) -> Result<(), StoreError> {
    if key.len() != 24 || !key.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(StoreError::InvalidKey(key.to_string()));
    }
    Ok(())
}

#[must_use]
pub fn format_slice_marker(key: &str) -> String {
    format!("{SLICE_MARKER_PREFIX}{key}{SLICE_MARKER_SUFFIX}")
}

#[must_use]
pub fn parse_slice_marker(s: &str) -> Option<&str> {
    s.strip_prefix(SLICE_MARKER_PREFIX)?
        .strip_suffix(SLICE_MARKER_SUFFIX)
}

// ---- InMemoryOffloadStore -----------------------------------------------

pub struct InMemoryOffloadStore {
    map: Mutex<HashMap<String, Vec<u8>>>,
}

impl Default for InMemoryOffloadStore {
    fn default() -> Self {
        Self::new()
    }
}

impl InMemoryOffloadStore {
    #[must_use]
    pub fn new() -> Self {
        Self {
            map: Mutex::new(HashMap::new()),
        }
    }
}

impl OffloadStore for InMemoryOffloadStore {
    fn put(&self, bytes: &[u8]) -> Result<String, StoreError> {
        let key = make_key(bytes);
        self.map.lock().unwrap().insert(key.clone(), bytes.to_vec());
        Ok(key)
    }
    fn get(&self, key: &str) -> Result<Vec<u8>, StoreError> {
        validate_key(key)?;
        self.map
            .lock()
            .unwrap()
            .get(key)
            .cloned()
            .ok_or_else(|| StoreError::NotFound(key.to_string()))
    }
    fn len(&self) -> usize {
        self.map.lock().unwrap().len()
    }
    fn backend_name(&self) -> &'static str {
        "memory"
    }
}

// ---- FileOffloadStore ---------------------------------------------------

/// One file per key in a directory. Smallest persistent backend (ADR-0004).
pub struct FileOffloadStore {
    dir: PathBuf,
}

impl FileOffloadStore {
    /// Open (creating if necessary) a file-backed store at `dir`.
    ///
    /// # Errors
    ///
    /// Returns `StoreError::Backend` if `dir` cannot be created or
    /// opened (e.g. permission denied).
    pub fn open(dir: impl Into<PathBuf>) -> Result<Self, StoreError> {
        let dir = dir.into();
        std::fs::create_dir_all(&dir)
            .map_err(|e| StoreError::Backend(format!("create_dir_all {}: {e}", dir.display())))?;
        Ok(Self { dir })
    }
}

impl OffloadStore for FileOffloadStore {
    fn put(&self, bytes: &[u8]) -> Result<String, StoreError> {
        let key = make_key(bytes);
        let path = self.dir.join(&key);
        std::fs::write(&path, bytes)
            .map_err(|e| StoreError::Backend(format!("write {}: {e}", path.display())))?;
        Ok(key)
    }
    fn get(&self, key: &str) -> Result<Vec<u8>, StoreError> {
        validate_key(key)?;
        let path = self.dir.join(key);
        match std::fs::read(&path) {
            Ok(bytes) => Ok(bytes),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                Err(StoreError::NotFound(key.to_string()))
            }
            Err(e) => Err(StoreError::Backend(format!("read {}: {e}", path.display()))),
        }
    }
    fn len(&self) -> usize {
        // ponytail: B12 fix — `len` counts *valid 24-hex key files*,
        // not every directory entry. Pre-fix, a `README.md` a user
        // dropped into the slices dir, or a `.tmp` file from a
        // hypothetical atomic-write migration (or a crash between
        // `write` and `close`), would inflate `len` and break the
        // content-addressed-dedup assertion in
        // `file_len_counts_files_in_dir_and_names_match_key` for
        // anyone running with a populated slices dir. Filter via
        // `validate_key` — it owns the 24-hex contract.
        let Ok(entries) = std::fs::read_dir(&self.dir) else {
            return 0;
        };
        entries
            .filter_map(std::result::Result::ok)
            .filter(|e| {
                e.file_name()
                    .to_str()
                    .is_some_and(|n| validate_key(n).is_ok())
            })
            .count()
    }
    fn backend_name(&self) -> &'static str {
        "file"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn make_key_is_24_hex() {
        let k = make_key(b"hello");
        assert_eq!(k.len(), 24);
        assert!(k.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn make_key_is_deterministic() {
        assert_eq!(make_key(b"x"), make_key(b"x"));
        assert_ne!(make_key(b"x"), make_key(b"y"));
    }

    #[test]
    fn validate_key_rejects_bad() {
        assert!(validate_key("tooshort").is_err());
        assert!(validate_key("zzzzzzzzzzzzzzzzzzzzzzzz").is_err()); // non-hex
        assert!(validate_key(&"a".repeat(24)).is_ok());
    }

    #[test]
    fn marker_round_trips() {
        let k = "abcdef0123456789abcdef01";
        let m = format_slice_marker(k);
        assert_eq!(parse_slice_marker(&m), Some(k));
        assert!(parse_slice_marker("not a marker").is_none());
    }

    // ponytail: pin the literal wire-format strings. ADR-0004 §
    // Key format claims the marker is byte-compatible with
    // Stratum's, so a Plugin3 slice marker must round-trip in
    // `stratum cat <marker>`. The grep-friendly `<<plugin3:slice:`
    // prefix is a load-bearing tool contract (the README points
    // users at `grep -F '<<plugin3:slice:'`). A contributor who
    // "version" the prefix to `<<plugin3_v2:slice:` silently
    // breaks cross-plugin retrieval AND breaks every user's
    // existing log-grep incantation.
    #[test]
    fn marker_prefix_literal_is_pinned() {
        assert_eq!(SLICE_MARKER_PREFIX, "<<plugin3:slice:");
        assert_eq!(SLICE_MARKER_SUFFIX, ">>");
    }

    // ponytail: pin the full marker shape end-to-end with a
    // real BLAKE3 key (not "abc..."). The drift test below
    // would catch a key-truncation change (24 → 32 hex); this
    // catches a marker-shape change with a realistic-looking
    // payload.
    #[test]
    fn marker_full_shape_end_to_end() {
        // Build a real key from a small payload, then format
        // the marker and assert the byte-level shape.
        let k = make_key(b"example payload");
        let m = format_slice_marker(&k);
        assert!(
            m.starts_with("<<plugin3:slice:"),
            "marker must start with plugin3 prefix, got: {m:?}"
        );
        assert!(
            m.ends_with(">>"),
            "marker must end with >> suffix, got: {m:?}"
        );
        // Strip the prefix+suffix and assert the middle is
        // exactly 24 hex chars (the key length is a contract).
        let inner = parse_slice_marker(&m).expect("marker parses");
        assert_eq!(inner.len(), 24, "marker key must be 24 hex chars");
        assert!(
            inner.chars().all(|c| c.is_ascii_hexdigit()),
            "marker key must be hex, got: {inner:?}"
        );
    }

    // ponytail: pin that parse rejects near-misses — a marker
    // missing the trailing `>>`, or with a wrong prefix, returns
    // None. A contributor who loosens `strip_suffix(SUFFIX)` to
    // `strip_suffix(">")` (single >) surfaces here.
    #[test]
    fn marker_parse_rejects_near_misses() {
        // Missing closing suffix.
        assert_eq!(parse_slice_marker("<<plugin3:slice:abc>>missing"), None);
        // Single > instead of >>.
        assert_eq!(parse_slice_marker("<<plugin3:slice:abc>"), None);
        // Wrong prefix.
        assert_eq!(parse_slice_marker("<<plugin3v2:slice:abc>>"), None);
        // Empty body after stripping prefix+suffix → Some("")
        // (spec: an empty key is technically parseable). A
        // contributor who switches to "key.len() > 0" as a
        // validation step would change semantics here — note
        // that today's `validate_key` rejects empty on hex
        // grounds (all chars must be hexdigit), not on
        // length-0 grounds, so this stays Some("").
        assert_eq!(parse_slice_marker("<<plugin3:slice:>>"), Some(""));
    }

    #[test]
    fn in_memory_round_trip() {
        let s = InMemoryOffloadStore::new();
        let k = s.put(b"payload").unwrap();
        assert_eq!(s.get(&k).unwrap(), b"payload");
        assert_eq!(s.len(), 1);
        assert_eq!(s.backend_name(), "memory");
    }

    #[test]
    fn file_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let s = FileOffloadStore::open(dir.path()).unwrap();
        let k = s.put(b"payload").unwrap();
        assert_eq!(s.get(&k).unwrap(), b"payload");
        assert_eq!(s.backend_name(), "file");
    }

    // ponytail: pin FileOffloadStore::len() — the function counts
    // directory entries, not in-memory map slots. A contributor who
    // switches it to a cached `AtomicUsize` mirror would silently
    // drift away from the on-disk truth on crashed/restored runs.
    // Also pins that the file is named by the 24-hex key (not some
    // hashed subdir layout) so `ls <slices_dir>` is grep-friendly.
    #[test]
    fn file_len_counts_files_in_dir_and_names_match_key() {
        let dir = tempfile::tempdir().unwrap();
        let s = FileOffloadStore::open(dir.path()).unwrap();
        assert_eq!(s.len(), 0, "fresh store is empty");
        let k1 = s.put(b"alpha").unwrap();
        let k2 = s.put(b"beta").unwrap();
        let k3 = s.put(b"alpha").unwrap(); // same bytes as k1
        assert_eq!(k1, k3, "same bytes must collapse to same key");
        assert_ne!(k1, k2, "different bytes must yield different keys");
        assert_eq!(
            s.len(),
            2,
            "content-addressed dedup: 2 distinct payloads → 2 files, \
             not 3; got len={}",
            s.len()
        );
        // File layout: <slices_dir>/<key>, plain filename.
        assert!(
            dir.path().join(&k1).is_file(),
            "file must be named by key directly, got: {k1}"
        );
        assert!(dir.path().join(&k2).is_file());
    }

    // ponytail: B12 fix — `len()` filters to valid 24-hex key
    // files, ignoring everything else in the dir. The
    // pre-fix shape counted every entry, so a README.txt a
    // user dropped into the slices dir would inflate the count
    // and break the dedup assertion above for any caller with a
    // populated dir. Pin each rejection path individually —
    // a contributor who narrows the filter (e.g. drops the
    // `validate_key` length check) would only surface at one
    // of the negative rows below.
    #[test]
    fn file_len_filters_to_24_hex_keys_only() {
        let dir = tempfile::tempdir().unwrap();
        let s = FileOffloadStore::open(dir.path()).unwrap();

        // 1 real key file (via put).
        let k = s.put(b"hello").unwrap();
        assert_eq!(k.len(), 24, "make_key returns 24 hex chars");
        assert_eq!(s.len(), 1, "one real key in the dir → len 1");

        // Drop a non-key file. Pre-fix this would push len to 2;
        // post-fix it must stay 1.
        std::fs::write(dir.path().join("README.md"), b"hi").unwrap();
        assert_eq!(
            s.len(),
            1,
            "README.md must NOT be counted as a slice; got len={}",
            s.len()
        );

        // A 23-char hex (too short) must be rejected.
        std::fs::write(dir.path().join("0123456789abcdef0123456"), b"x").unwrap();
        assert_eq!(
            s.len(),
            1,
            "23-char hex must NOT be counted (off-by-one length); got len={}",
            s.len()
        );

        // A 25-char hex (too long) must be rejected.
        std::fs::write(dir.path().join("0123456789abcdef012345678"), b"x").unwrap();
        assert_eq!(
            s.len(),
            1,
            "25-char hex must NOT be counted; got len={}",
            s.len()
        );

        // A 24-char but non-hex name (one 'z') must be rejected.
        std::fs::write(dir.path().join("zzzzzzzzzzzzzzzzzzzzzzzz"), b"x").unwrap();
        assert_eq!(
            s.len(),
            1,
            "non-hex chars in a 24-char name must NOT be counted; got len={}",
            s.len()
        );

        // A crash-left .tmp file (24-hex prefix but with .tmp suffix)
        // must be rejected — `validate_key` rejects anything with a
        // dot. A contributor who switches to `file_name().len() == 24`
        // (length-only check) would surface here.
        std::fs::write(dir.path().join("0123456789abcdef01234567.tmp"), b"x").unwrap();
        assert_eq!(
            s.len(),
            1,
            ".tmp file (24-char stem with .tmp suffix) must NOT be counted; got len={}",
            s.len()
        );

        // A real second key pushes len to 2.
        let k2 = s.put(b"world").unwrap();
        assert_ne!(k, k2, "different bytes → different keys");
        assert_eq!(
            s.len(),
            2,
            "two distinct payloads → len 2 (decoy files still ignored); got len={}",
            s.len()
        );

        // All the decoy files are still on disk — verify len didn't
        // accidentally delete them.
        assert!(
            dir.path().join("README.md").is_file(),
            "len() must NOT mutate the dir; README.md still present"
        );
    }

    // ponytail: pin the NotFound path on FileOffloadStore. The
    // empty-file error is collapsed into NotFound (no separate
    // "empty file" sentinel). A contributor who switches to
    // `std::fs::read_to_string` + a custom Empty sentinel would
    // change the wire error and break downstream code that
    // matches on `StoreError::NotFound`.
    #[test]
    fn file_get_missing_returns_not_found_with_key() {
        let dir = tempfile::tempdir().unwrap();
        let s = FileOffloadStore::open(dir.path()).unwrap();
        let missing_key = "0123456789abcdef01234567"; // valid hex, absent
        match s.get(missing_key) {
            Err(StoreError::NotFound(k)) => assert_eq!(
                k, missing_key,
                "NotFound must carry the requested key for diagnostics"
            ),
            other => panic!("expected NotFound for missing key, got {other:?}"),
        }
    }

    // ponytail: pin the NotFound path on InMemoryOffloadStore. The
    // file backend has its own NotFound test above; the in-memory
    // backend walks `HashMap::get(...).cloned().ok_or_else(...)` —
    // a contributor who switches to `.ok_or(StoreError::Backend(...))`
    // (treating absence as a backend error) changes the wire error
    // and surfaces here.
    #[test]
    fn in_memory_get_missing_returns_not_found_with_key() {
        let s = InMemoryOffloadStore::new();
        let missing_key = "0123456789abcdef01234567"; // valid hex, never put
        match s.get(missing_key) {
            Err(StoreError::NotFound(k)) => assert_eq!(
                k, missing_key,
                "in-memory NotFound must carry the requested key for diagnostics"
            ),
            other => panic!("expected NotFound for missing key, got {other:?}"),
        }
    }

    // ponytail: pin validate_key propagation through both backends.
    // `get` calls `validate_key` and returns `InvalidKey` (NOT
    // `NotFound`) when the key shape is wrong. A contributor who
    // drops the `validate_key` call (or moves it to a private
    // helper that swallows the error) would silently turn invalid
    // keys into NotFound — masking the diagnostic. Pin the error
    // variant on both backends so the wire contract is symmetric.
    #[test]
    fn get_with_invalid_key_returns_invalid_key_not_not_found() {
        let mem = InMemoryOffloadStore::new();
        let dir = tempfile::tempdir().unwrap();
        let file = FileOffloadStore::open(dir.path()).unwrap();

        // Too short.
        match mem.get("abcd") {
            Err(StoreError::InvalidKey(k)) => assert_eq!(k, "abcd"),
            other => panic!("in-memory: expected InvalidKey for short key, got {other:?}"),
        }
        match file.get("abcd") {
            Err(StoreError::InvalidKey(k)) => assert_eq!(k, "abcd"),
            other => panic!("file: expected InvalidKey for short key, got {other:?}"),
        }
        // Non-hex of correct length.
        let non_hex = "z".repeat(24);
        match mem.get(&non_hex) {
            Err(StoreError::InvalidKey(k)) => assert_eq!(k, non_hex),
            other => panic!("in-memory: expected InvalidKey for non-hex key, got {other:?}"),
        }
        match file.get(&non_hex) {
            Err(StoreError::InvalidKey(k)) => assert_eq!(k, non_hex),
            other => panic!("file: expected InvalidKey for non-hex key, got {other:?}"),
        }
    }

    // ponytail: pin FileOffloadStore end-to-end persistence. The
    // `file_round_trip` test above writes and reads in the same
    // open() call — it cannot detect a contributor who swaps the
    // implementation for an in-memory backing that just happens to
    // match within a session. A real restart round-trip is the only
    // test that proves the bytes actually hit disk under the right
    // filename. Without this, a "fix" that uses an internal
    // `tempfile::tempdir().into()` shadowing `self.dir` would pass
    // `file_round_trip` and break the persistence story silently.
    #[test]
    fn file_persists_payload_across_store_restart() {
        let dir = tempfile::tempdir().unwrap();
        let payload = b"persisted-across-restart";
        let k;
        {
            let s = FileOffloadStore::open(dir.path()).unwrap();
            k = s.put(payload).unwrap();
            assert_eq!(s.get(&k).unwrap(), payload);
            // Store drops here; on-disk file should remain.
        }
        // Re-open against the same dir; expect the same key to
        // still resolve to the same bytes.
        let s2 = FileOffloadStore::open(dir.path()).unwrap();
        assert_eq!(
            s2.len(),
            1,
            "reopened store must observe the file left by the prior instance; \
             got len={}",
            s2.len()
        );
        assert_eq!(
            s2.get(&k).unwrap(),
            payload,
            "reopened store must return the prior payload for the prior key"
        );
        // Same bytes → same key (content-addressed dedup survives restart).
        let k2 = s2.put(payload).unwrap();
        assert_eq!(
            k, k2,
            "identical payload must yield identical key across store instances"
        );
    }
}
