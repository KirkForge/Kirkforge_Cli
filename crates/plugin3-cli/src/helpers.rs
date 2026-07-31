//! Generic CLI helpers — offload store wiring + stdin JSON reading.
//! Per ADR-0002 § Crate layout (`crates/plugin3-cli/src/`).

use std::io::{self, Read};

use plugin3_core::{
    store::FileOffloadStore, store::InMemoryOffloadStore, store::OffloadStore, Paths,
};
use serde::Deserialize;

pub(crate) fn open_store() -> Box<dyn OffloadStore> {
    let dir = Paths::resolve().slices_dir();
    match FileOffloadStore::open(&dir) {
        Ok(s) => Box::new(s),
        Err(e) => {
            eprintln!("plugin3: file store open failed ({e}); falling back to in-memory");
            Box::new(InMemoryOffloadStore::new())
        }
    }
}

// ponytail: ADR-0009 § Error contract — a hook handler must not
// crash the host. Returns None on read or parse failure so the
// caller can emit a safe fallback response and exit 0.
pub(crate) fn read_stdin_json<T: for<'de> Deserialize<'de>>() -> Option<T> {
    let mut s = String::new();
    if let Err(e) = io::stdin().read_to_string(&mut s) {
        eprintln!("plugin3: stdin read failed: {e}");
        return None;
    }
    match serde_json::from_str(&s) {
        Ok(v) => Some(v),
        Err(e) => {
            eprintln!("plugin3: stdin parse failed: {e}");
            None
        }
    }
}

// ponytail: shared by every subprocess test (ADR-0009, ADR-0015).
// `cfg(test)` keeps it out of release builds — the binary's own
// path-lookup isn't a runtime concern.
#[cfg(test)]
pub(crate) fn plugin3_binary_path() -> std::path::PathBuf {
    // CARGO_BIN_EXE_<name> is set when the test is run via the
    // cargo test runner. When the binary path is unknown, fall
    // back to a sibling of the running test executable
    // (target/debug/deps/plugin3-<hash> -> target/debug/plugin3).
    if let Ok(p) = std::env::var("CARGO_BIN_EXE_plugin3") {
        return std::path::PathBuf::from(p);
    }
    let exe = std::env::current_exe().expect("current_exe");
    // exe is target/debug/deps/plugin3-<hash>; the binary lives
    // one level up under the same name without the hash.
    let stem = exe.file_name().unwrap().to_string_lossy();
    let without_hash = stem.split('-').next().unwrap();
    exe.parent()
        .unwrap() // deps/
        .parent()
        .unwrap() // debug/
        .join(without_hash)
}
