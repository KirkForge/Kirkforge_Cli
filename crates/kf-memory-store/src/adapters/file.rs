//! JSON-file-backed memory adapter. Port of
//! `memory-palace/src/adapters/file.ts`.
//!
//! Concurrency is handled by a `.lock` file with an `AlreadyExists` retry
//! loop; the data file is written to a `NamedTempFile` and atomically
//! renamed so partial writes never corrupt the file. If the file is found
//! in a corrupt state on load it is copied to `<file>.corrupt` and an empty
//! cache is returned (subsequent operations return the load error).

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use serde_json::Value;
use tempfile::NamedTempFile;

use crate::adapters::MemoryAdapter;
use crate::types::{MemoryObject, MemoryQuery, MemoryStats};

pub struct FileAdapter {
    file_path: PathBuf,
    lock_path: PathBuf,
    state: Mutex<LoadState>,
}

#[derive(Default)]
struct LoadState {
    objects: Vec<MemoryObject>,
    loaded: bool,
    load_error: Option<String>,
}

/// Removes the `.lock` file on drop so a panic mid-write still releases.
struct LockGuard(PathBuf);

impl Drop for LockGuard {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

impl FileAdapter {
    pub fn new<P: Into<PathBuf>>(path: P) -> Self {
        let file_path = path.into();
        let lock_path = {
            let mut s = file_path.as_os_str().to_owned();
            s.push(".lock");
            PathBuf::from(s)
        };
        Self {
            file_path,
            lock_path,
            state: Mutex::new(LoadState::default()),
        }
    }

    pub fn file_path(&self) -> &Path {
        &self.file_path
    }

    fn ensure_loaded(&self, state: &mut LoadState) {
        if state.loaded {
            return;
        }
        match Self::load_from_disk(&self.file_path) {
            Ok(objects) => {
                state.objects = objects;
                state.loaded = true;
                state.load_error = None;
            }
            Err(LoadErr::Missing) => {
                state.objects.clear();
                state.loaded = true;
            }
            Err(LoadErr::Corrupt(msg)) => {
                let corrupt_path = {
                    let mut s = self.file_path.as_os_str().to_owned();
                    s.push(".corrupt");
                    PathBuf::from(s)
                };
                let _ = fs::copy(&self.file_path, &corrupt_path);
                state.objects.clear();
                state.loaded = true;
                state.load_error = Some(msg);
            }
        }
    }

    fn load_from_disk(path: &Path) -> std::result::Result<Vec<MemoryObject>, LoadErr> {
        let raw = match fs::read_to_string(path) {
            Ok(s) => s,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(LoadErr::Missing),
            Err(e) => return Err(LoadErr::Corrupt(e.to_string())),
        };
        let parsed: Value = serde_json::from_str(&raw)
            .map_err(|e| LoadErr::Corrupt(format!("JSON parse failed: {e}")))?;
        let arr = parsed
            .as_array()
            .ok_or_else(|| LoadErr::Corrupt("not an array".to_string()))?;
        let mut objects = Vec::with_capacity(arr.len());
        for (i, obj) in arr.iter().enumerate() {
            let o: MemoryObject = serde_json::from_value(obj.clone())
                .map_err(|e| LoadErr::Corrupt(format!("malformed object at index {i}: {e}")))?;
            objects.push(o);
        }
        Ok(objects)
    }

    fn acquire_lock(&self, timeout: Duration) -> Option<LockGuard> {
        let started = Instant::now();
        loop {
            match OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&self.lock_path)
            {
                Ok(mut f) => {
                    let _ = writeln!(f, "{}", std::process::id());
                    return Some(LockGuard(self.lock_path.clone()));
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(_) => return None,
            }
            if started.elapsed() >= timeout {
                return None;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    fn flush(&self, state: &mut LoadState) -> Result<()> {
        if let Some(err) = &state.load_error {
            return Err(anyhow!("FileAdapter unusable: {err}"));
        }
        let parent = self.file_path.parent().unwrap_or_else(|| Path::new("."));
        fs::create_dir_all(parent)?;
        let data = serde_json::to_string(&state.objects)?;
        let tmp = NamedTempFile::new_in(parent)?;
        tmp.as_file().write_all(data.as_bytes())?;
        tmp.as_file().sync_all()?;
        tmp.persist(&self.file_path)
            .map_err(|e| anyhow!("rename failed: {e}"))?;
        Ok(())
    }

    /// Read the raw file contents (test helper for corrupt-backup assertions).
    #[doc(hidden)]
    pub fn read_raw_for_test(&self) -> std::io::Result<String> {
        let mut buf = String::new();
        File::open(&self.file_path)?.read_to_string(&mut buf)?;
        Ok(buf)
    }
}

enum LoadErr {
    Missing,
    Corrupt(String),
}

impl MemoryAdapter for FileAdapter {
    fn write(&self, obj: &MemoryObject) -> Result<()> {
        let _lock = self
            .acquire_lock(Duration::from_secs(3))
            .ok_or_else(|| anyhow!("FileAdapter: could not acquire lock for write after 3s"))?;
        let mut state = self.state.lock().expect("file state lock poisoned");
        self.ensure_loaded(&mut state);
        if let Some(err) = &state.load_error {
            return Err(anyhow!("FileAdapter unusable: {err}"));
        }
        state.objects.push(obj.clone());
        self.flush(&mut state)
    }

    fn read(&self, id: &str) -> Result<Option<MemoryObject>> {
        let mut state = self.state.lock().expect("file state lock poisoned");
        self.ensure_loaded(&mut state);
        if let Some(err) = &state.load_error {
            return Err(anyhow!("FileAdapter unusable: {err}"));
        }
        Ok(state.objects.iter().find(|o| o.id == id).cloned())
    }

    fn query(&self, q: &MemoryQuery) -> Result<Vec<MemoryObject>> {
        let mut state = self.state.lock().expect("file state lock poisoned");
        self.ensure_loaded(&mut state);
        if let Some(err) = &state.load_error {
            return Err(anyhow!("FileAdapter unusable: {err}"));
        }
        let mut results: Vec<MemoryObject> = state
            .objects
            .iter()
            .filter(|o| q.kind.as_deref().is_none_or(|k| o.kind == k))
            .filter(|o| {
                q.tags
                    .as_deref()
                    .is_none_or(|tags| tags.iter().any(|t| o.tags.iter().any(|ot| ot == t)))
            })
            .filter(|o| q.since.as_deref().is_none_or(|s| o.timestamp.as_str() >= s))
            .cloned()
            .collect();
        results.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
        if let Some(limit) = q.limit {
            results.truncate(limit);
        }
        Ok(results)
    }

    fn stats(&self) -> Result<MemoryStats> {
        let mut state = self.state.lock().expect("file state lock poisoned");
        self.ensure_loaded(&mut state);
        if let Some(err) = &state.load_error {
            return Err(anyhow!("FileAdapter unusable: {err}"));
        }
        let last_write = state
            .objects
            .last()
            .map(|o| o.timestamp.clone())
            .unwrap_or_else(|| "never".to_string());
        Ok(MemoryStats {
            total_objects: state.objects.len(),
            last_write,
        })
    }

    fn persist(&self) -> Result<()> {
        let mut state = self.state.lock().expect("file state lock poisoned");
        self.ensure_loaded(&mut state);
        if state.load_error.is_none() {
            self.flush(&mut state)?;
        }
        Ok(())
    }
}
