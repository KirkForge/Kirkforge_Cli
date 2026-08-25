//! Cache serialization/deserialization + incremental rebuild.
//!
//! `save` writes a `CachedIndex` as JSON atomically (temp + rename);
//! `load` reads it back and rejects stale format versions. `is_current`
//! compares the stored git HEAD to the working tree. `incremental_rebuild`
//! and `mtime_rebuild` diff changed files since the cache and re-index
//! only the changed paths.
//!
//! ponytail: disk caching uses serde_json (not bincode — bincode is unmaintained).
//! The upgrade path is a compact binary format if JSON size becomes a concern.

use crate::{build_embeddings, CachedIndex, ContextIndex, CURRENT_FORMAT_VERSION};

impl ContextIndex {
    /// Save the index to a JSON file, along with the current git HEAD
    /// and file modification times.
    pub fn save(&self, path: &std::path::Path, head: &str) -> anyhow::Result<()> {
        let embeddings = build_embeddings(self);
        let mut file_mtimes = std::collections::HashMap::new();
        for sym in &self.symbols {
            if let Ok(meta) = std::fs::metadata(&sym.file) {
                if let Ok(mtime) = meta.modified() {
                    let key = sym.file.to_string_lossy().to_string();
                    let ts = mtime
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs();
                    file_mtimes.entry(key).or_insert(ts);
                }
            }
        }
        let cached = CachedIndex {
            format_version: CURRENT_FORMAT_VERSION,
            head: head.to_string(),
            symbols: self.symbols.clone(),
            edges: self.edges.clone(),
            call_edges: self.call_edges.clone(),
            embeddings,
            file_mtimes,
        };
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_string(&cached)?;
        // Atomic write: temp + rename so a crash mid-write cannot leave the
        // cache truncated (WO 43.21). Mirrors carryover.rs:247-251.
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, &json)?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    }

    /// Load a cached index from a JSON file. Returns the cached index
    /// if the file exists, parses as JSON, AND has a matching
    /// `format_version`. A version mismatch returns `Err` so the caller
    /// rebuilds from scratch instead of trusting an old format (WO 43.21).
    pub fn load(path: &std::path::Path) -> anyhow::Result<CachedIndex> {
        let json = std::fs::read_to_string(path)?;
        let cached: CachedIndex = serde_json::from_str(&json)?;
        if cached.format_version != CURRENT_FORMAT_VERSION {
            anyhow::bail!(
                "context index cache format_version {} != current {}; rebuilding",
                cached.format_version,
                CURRENT_FORMAT_VERSION
            );
        }
        Ok(cached)
    }

    /// Check whether the cached index is current by comparing the
    /// stored git HEAD with the current HEAD in `repo_root`.
    pub fn is_current(cached: &CachedIndex, repo_root: &std::path::Path) -> bool {
        match current_head(repo_root) {
            Some(head) => head == cached.head,
            None => false,
        }
    }

    /// Incremental rebuild: diff changed files since the cached HEAD,
    /// remove stale symbols/edges for those files, and re-index only
    /// the changed files.
    pub fn incremental_rebuild(cached: CachedIndex, repo_root: &std::path::Path) -> (Self, usize) {
        let changed_files = git_diff_files(&cached.head, repo_root);
        let changed_count = changed_files.len();
        if changed_files.is_empty() {
            let idx = Self::from_cached(cached);
            return (idx, 0);
        }

        let changed_set: std::collections::HashSet<String> = changed_files
            .iter()
            .map(|p| p.to_string_lossy().to_string())
            .collect();

        let mut idx =
            Self::from_symbols_and_edges_and_calls(cached.symbols, cached.edges, cached.call_edges);

        idx.symbols
            .retain(|s| !changed_set.contains(&s.file.to_string_lossy().to_string()));
        idx.edges.retain(|e| {
            !changed_set.contains(&e.source_file.to_string_lossy().to_string())
                && e.resolved_file
                    .as_ref()
                    .is_none_or(|rf| !changed_set.contains(&rf.to_string_lossy().to_string()))
        });
        idx.call_edges.retain(|e| {
            !changed_set.contains(&e.caller_file.to_string_lossy().to_string())
                && e.callee_file
                    .as_ref()
                    .is_none_or(|cf| !changed_set.contains(&cf.to_string_lossy().to_string()))
        });

        for path in &changed_files {
            if path.is_file() {
                if let Ok(content) = std::fs::read_to_string(path) {
                    let _ = idx.index_file(path, &content);
                }
            }
        }

        idx.resolve_imports(repo_root);
        idx.resolve_call_edges();
        (idx, changed_count)
    }

    /// Mtime-based incremental rebuild: compares current file mtimes
    /// against the cached mtime map. Only re-indexes files whose mtime
    /// changed. Falls back to git-diff-based rebuild if no mtime data.
    ///
    /// Returns `(index, changed_file_count)`.
    pub fn mtime_rebuild(cached: CachedIndex, repo_root: &std::path::Path) -> (Self, usize) {
        if cached.file_mtimes.is_empty() {
            let changed = git_diff_files(&cached.head, repo_root);
            let count = changed.len();
            let idx = if changed.is_empty() {
                Self::from_cached(cached)
            } else {
                let (idx, c) = Self::incremental_rebuild(cached, repo_root);
                debug_assert_eq!(c, count);
                idx
            };
            return (idx, count);
        }

        let mut changed_paths: Vec<std::path::PathBuf> = Vec::new();
        for (file_str, cached_mtime) in &cached.file_mtimes {
            let path = std::path::PathBuf::from(file_str);
            if let Ok(meta) = std::fs::metadata(&path) {
                if let Ok(mtime) = meta.modified() {
                    let current_ts = mtime
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs();
                    if current_ts != *cached_mtime {
                        changed_paths.push(path);
                    }
                } else {
                    changed_paths.push(path);
                }
            } else {
                changed_paths.push(path);
            }
        }

        if changed_paths.is_empty() {
            return (Self::from_cached(cached), 0);
        }

        let changed_set: std::collections::HashSet<String> = changed_paths
            .iter()
            .map(|p| p.to_string_lossy().to_string())
            .collect();

        let mut idx =
            Self::from_symbols_and_edges_and_calls(cached.symbols, cached.edges, cached.call_edges);

        idx.symbols
            .retain(|s| !changed_set.contains(&s.file.to_string_lossy().to_string()));
        idx.edges.retain(|e| {
            !changed_set.contains(&e.source_file.to_string_lossy().to_string())
                && e.resolved_file
                    .as_ref()
                    .is_none_or(|rf| !changed_set.contains(&rf.to_string_lossy().to_string()))
        });
        idx.call_edges.retain(|e| {
            !changed_set.contains(&e.caller_file.to_string_lossy().to_string())
                && e.callee_file
                    .as_ref()
                    .is_none_or(|cf| !changed_set.contains(&cf.to_string_lossy().to_string()))
        });

        for path in &changed_paths {
            if path.is_file() {
                if let Ok(content) = std::fs::read_to_string(path) {
                    let _ = idx.index_file(path, &content);
                }
            }
        }

        idx.resolve_imports(repo_root);
        idx.resolve_call_edges();
        (idx, changed_paths.len())
    }
}

/// Get the current git HEAD SHA for a repository root.
pub fn current_head(repo_root: &std::path::Path) -> Option<String> {
    std::process::Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(repo_root)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
}

/// Get files changed between `old_head` and current HEAD that are
/// indexable source files.
fn git_diff_files(old_head: &str, repo_root: &std::path::Path) -> Vec<std::path::PathBuf> {
    let output = std::process::Command::new("git")
        .args(["diff", "--name-only", old_head, "HEAD"])
        .current_dir(repo_root)
        .output();
    let output = match output {
        Ok(o) if o.status.success() => o,
        _ => return Vec::new(),
    };
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|l| !l.is_empty())
        .filter_map(|l| {
            let p = repo_root.join(l);
            if p.is_file() && crate::detect_language(&p).is_some() {
                Some(p)
            } else {
                None
            }
        })
        .collect()
}
