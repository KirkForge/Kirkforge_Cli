# ADR-0004: OffloadStore reuse from Stratum

- **Status:** Accepted
- **Date:** 2026-06-24

## Context

Plugin3 needs to store the *middle* of a sliced tool output so
the user can retrieve it on demand. Stratum already has an
`OffloadStore` trait that does exactly this for Reformat
offloads. Plugin3 has three choices:

1. **Hard-depend on Stratum** — `plugin3-core` imports
   `stratum-core`. Tight coupling; release cadence is shared.
2. **Reimplement** — duplicate the trait in `plugin3-core`.
   Drift risk: Stratum adds a backend, Plugin3 must add it too.
3. **Reimplement with byte-compat markers** — duplicate the
   trait shape, share the marker format, run a drift test.

Choice 3 is the right one. Plugin3 and Stratum are released
independently (the user may install only one) but the *wire
format* of the offload markers must be identical so a user can
copy a Plugin3 slice marker into a Stratum retrieval command
and get the same content.

## Decision

### Trait shape (mirrors Stratum ADR-0004)

```rust
// crates/plugin3-core/src/store.rs

pub trait OffloadStore: Send + Sync {
    /// Persist `bytes` and return a content-addressed key.
    fn put(&self, bytes: &[u8]) -> Result<String, StoreError>;

    /// Retrieve content by key.
    fn get(&self, key: &str) -> Result<Vec<u8>, StoreError>;

    /// Number of entries currently held.
    fn len(&self) -> usize;

    /// True if the store is empty.
    fn is_empty(&self) -> bool { self.len() == 0 }

    /// Stable name for diagnostics ("memory", "file"). ponytail:
    /// "sqlite" was listed in the earlier draft; the MVP has no
    /// SQLite backend, so the set of valid names is {"memory",
    /// "file"} until a future ADR re-introduces SQLite.
    fn backend_name(&self) -> &'static str;
}

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("invalid key: {0}")]
    InvalidKey(String),
    #[error("key not found: {0}")]
    NotFound(String),
    #[error("backend error: {0}")]
    Backend(String),
}
```

### Key format: BLAKE3, 24 hex chars

```rust
pub fn make_key(bytes: &[u8]) -> String {
    let hash = blake3::hash(bytes);
    let hex = hash.to_hex();
    // First 24 hex chars = 12 bytes = 96 bits. Collision
    // probability is ~10^-15 at 10^9 entries — fine for the
    // MVP. Bumping to 32 hex (full 128 bits) is a future ADR.
    hex.as_str()[..24].to_string()
}

pub fn validate_key(key: &str) -> Result<(), StoreError> {
    if key.len() != 24 || !key.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(StoreError::InvalidKey(key.to_string()));
    }
    Ok(())
}
```

`make_key` is byte-compatible with Stratum's
`make_offload_key`. The drift test (ADR-0016) cross-checks
the two implementations against a known corpus.

### Backends

Two backends, mirroring Stratum's `file` and `memory`
backends (Stratum's `sqlite` is not mirrored — see below):

```rust
// crates/plugin3-core/src/store.rs (single file, two backends)

pub struct InMemoryOffloadStore { /* Mutex<HashMap<String, Vec<u8>>> */ }
// used in tests and as the runtime default if no persistent
// store is configured.

pub struct FileOffloadStore { /* directory of files named by key */ }
// smallest persistent backend; no SQLite dependency.
```

ponytail: the earlier draft listed a third backend,
`SqliteOffloadStore`, gated on a `sqlite` feature. The MVP
ships without SQLite — `rusqlite` is not a workspace
dependency (ADR-0017 § Feature gates (none today)). Adding
SQLite is a future ADR with a `rusqlite = { version = "0.31",
features = ["bundled"], optional = true }` dep; until then
the `store.rs` module exposes only the in-memory and file
backends. The drift test in
`tests/offload_store_spec_drift.rs` pins the absence of any
SQLite claim in the § Backends block so a contributor who
re-pastes the SQLite backend surfaces here.

### Marker format

```rust
pub const SLICE_MARKER_PREFIX: &str = "<<plugin3:slice:";
pub const SLICE_MARKER_SUFFIX: &str = ">>";

pub fn format_slice_marker(key: &str) -> String {
    format!("{}{}{}", SLICE_MARKER_PREFIX, key, SLICE_MARKER_SUFFIX)
}

pub fn parse_slice_marker(s: &str) -> Option<&str> {
    s.strip_prefix(SLICE_MARKER_PREFIX)?
        .strip_suffix(SLICE_MARKER_SUFFIX)
}
```

The marker is human-greppable: `grep -F '<<plugin3:slice:'` finds
all slice markers in a session log.

### Loud failure on init

A `FileOffloadStore` whose directory cannot be created returns
`StoreError::Backend` and aborts startup at the store-open call
itself — the backend does not silently truncate the data or
return a partial store. The CLI's `open_store` helper (the
sole caller today, in `crates/plugin3-cli/src/main.rs`) catches
that error, prints a one-line warning to stderr, and falls
back to an `InMemoryOffloadStore` so the host hook does not
crash on a transient permission error (ADR-0009 § Error
contract — a hook handler must not crash the host). The
fallback accepts that slice data is lost on restart for the
affected session: the in-memory store evaporates when the
process exits, and the slice markers emitted during the
session will fail to resolve on the next run.

ponytail: an earlier draft of this section forbade any
fallback to in-memory, on the grounds that a silent
fallback would lose data on restart. That pre-fix wording
contradicted the runtime, which already fell back. The
runtime falls back precisely because the alternative is
worse for the user: a host crash on a permission error means
the user loses the entire session, not just the slice
markers. The fallback is loud in the sense that the eprintln
is the only diagnostic the host's stderr will see, but it is
not loud in the sense of changing the hook's stdout contract
— the host still sees a successful hook response on stdout.
The drift test
`offload_store_spec_drift.rs::adr_0004_loud_failure_block_acknowledges_hook_fallback`
pins both the carve-out and the warning-to-stderr contract so
a contributor who reverts to the pre-fix wording without
re-justifying the hook-survival carve-out fails CI.

```rust
impl FileOffloadStore {
    pub fn open(dir: impl Into<PathBuf>) -> Result<Self, StoreError> {
        let dir = dir.into();
        std::fs::create_dir_all(&dir)
            .map_err(|e| StoreError::Backend(
                format!("create_dir_all {}: {e}", dir.display())
            ))?;
        Ok(Self { dir })
    }
}
```

ponytail: the earlier draft's example used `parking_lot::Mutex`
and `rusqlite::Connection::open`. The workspace does not depend
on `parking_lot` (ADR-0017 § Workspace Cargo.toml — it was
trimmed along with the other aspirational deps) and does not
depend on `rusqlite` (no SQLite backend). The current backend
holds its state behind `std::sync::Mutex`, which is sufficient
for the MVP's per-process access pattern (the offload store is
written by the PostToolUse hook and read by the retrieval
command — no cross-process contention). The drift test
`offload_store_spec_drift.rs::adr_0004_no_parking_lot_mutex`
pins the absence of `parking_lot::Mutex` in this example.

## Consequences

Negative first:

- Two `OffloadStore` implementations across two plugins is a
  drift hazard. The drift test (ADR-0016) catches divergence.
- The 24-hex key is shorter than Stratum's 32-hex key in some
  builds. The byte-compat contract is 24 hex chars (96 bits);
  any future bump to 32 must be coordinated.
- ponytail: no SQLite backend today means a session that needs
  cross-process offload (e.g. a long-running daemon plus a CLI
  that retrieves markers) loses the connection between the
  two — the file backend is per-process-locked. A SQLite
  backend is a future ADR with a binary-size budget to
  negotiate.

Positive:

- A user who installs both Plugin2 and Plugin3 can copy a
  Plugin3 slice marker into `stratum cat <marker>` and get the
  same content.
- The trait shape is small (~30 lines). Two implementations is
  cheap.
- The persistent store survives a session restart, so a user
  who sliced an old tool output can still retrieve it after
  the agent exits.

## Implementation notes

The `OffloadStore` trait and both backends live in a single
file at `crates/plugin3-core/src/store.rs` (ponytail: the
earlier draft specified a multi-file `store/` module tree
with the three backends as sibling submodules — the SQLite
submodule was removed when the SQLite backend was deferred,
and the in-memory + file backends stayed consolidated as the
split isn't load-bearing at two backends). The drift test
`offload_store_spec_drift.rs::adr_0004_implementation_path_is_store_rs`
pins the path. If a future contributor re-introduces a
`store/` module tree, they update both the ADR and the drift
test together.

The drift test fixture lives at
`crates/plugin3-core/tests/store_drift.rs` and references a
known corpus of inputs whose expected keys are checked
against the BLAKE3 spec test vectors (and, by extension,
Stratum's `make_offload_key` — both implementations reduce
to `blake3::hash(bytes).to_hex()[..24]`, which is the
contract).

A user who wants only Plugin3 (no Stratum) gets the same
behaviour. A user who has both installed sees a single
content-addressed namespace; the keys are interchangeable.