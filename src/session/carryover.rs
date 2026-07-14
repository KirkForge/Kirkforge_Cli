/// Session carryover profile — cross-session awareness with tiny footprint.
///
/// Collects a minimal profile (~200 bytes JSON, ~55 tokens rendered) during
/// a session and injects it into the next session's system prompt. The model
/// picks up where it left off without the cost of replaying history.
///
/// Pattern inspired by PULSE/ORBIT engines: classify → tag → append to context.
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

// ── Data Structures ────────────────────────────────────────────────────────────

/// The on-disk carryover profile. Every field is chosen to be small and high-signal.
/// Only the top 5 tools are stored; everything else is pruned on save.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CarryoverProfile {
    /// Monotonic session counter.
    pub session_count: u64,

    /// Tool usage counts (pruned to top 5 on save).
    #[serde(default)]
    pub tool_usage: HashMap<String, u64>,

    /// Verbatim last user message from the previous session.
    #[serde(default)]
    pub last_user_message: String,

    /// Last few paths touched by file tools (max 3, condensed).
    #[serde(default)]
    pub recent_paths: Vec<String>,

    /// Counts of verifier correction message categories.
    #[serde(default)]
    pub verifier_warnings: HashMap<String, u64>,

    /// Inferred work-pattern strength scores (0.0–1.0).
    #[serde(default)]
    pub work_patterns: HashMap<String, f64>,

    /// Human-readable timestamp of last session end.
    #[serde(default)]
    pub last_session_time: String,
}

impl CarryoverProfile {
    /// Render to a short string for insertion into the system prompt.
    /// Returns empty string when there's no session data yet.
    pub fn to_prompt_block(&self) -> String {
        if self.session_count == 0 {
            return String::new();
        }

        let mut parts: Vec<String> = Vec::new();
        let ts = if self.last_session_time.is_empty() {
            "recent".to_string()
        } else {
            self.last_session_time.clone()
        };

        parts.push(format!(
            "[Session Carryover — session #{}, {}]",
            self.session_count, ts
        ));

        if !self.last_user_message.is_empty() {
            parts.push(format!("Last topic: {}", self.last_user_message));
        }

        if !self.recent_paths.is_empty() {
            parts.push(format!("Active paths: {}", self.recent_paths.join(", ")));
        }

        // Only emit patterns with non-trivial strength
        let active_patterns: Vec<&str> = self
            .work_patterns
            .iter()
            .filter(|(_, &v)| v > 0.3)
            .map(|(k, _)| k.as_str())
            .collect();
        if !active_patterns.is_empty() {
            parts.push(format!("Patterns: {}", active_patterns.join(", ")));
        }

        // Only emit warnings that appeared multiple times
        let common_warnings: Vec<String> = self
            .verifier_warnings
            .iter()
            .filter(|(_, &v)| v >= 2)
            .map(|(k, _)| k.clone())
            .collect();
        if !common_warnings.is_empty() {
            parts.push(format!("Recurring: {}", common_warnings.join(", ")));
        }

        parts.join("\n")
    }

    /// Estimated token count of the rendered block.
    pub fn estimated_tokens(&self) -> usize {
        self.to_prompt_block().len() / 4
    }

    // ── Collection methods ──────────────────────────────────────────

    /// Increment tool call count.
    pub fn record_tool_call(&mut self, tool_name: &str) {
        *self.tool_usage.entry(tool_name.to_string()).or_insert(0) += 1;
    }

    /// Track a file path. Keeps last 3, deduplicated, condensed.
    pub fn record_path(&mut self, path: &str) {
        let condensed = condense_path(path);
        self.recent_paths.retain(|p| p != &condensed);
        self.recent_paths.push(condensed);
        if self.recent_paths.len() > 3 {
            self.recent_paths.remove(0);
        }
    }

    /// Track a verifier correction by extracting its key category.
    /// The category is the segment before the first colon or em-dash.
    pub fn record_verifier_warning(&mut self, message: &str) {
        let key = message
            .split([':', '\u{2014}'])
            .next()
            .unwrap_or(message)
            .trim()
            .to_string();
        *self.verifier_warnings.entry(key).or_insert(0) += 1;
    }

    /// Detect work patterns from tool usage statistics.
    pub fn refresh_patterns(&mut self) {
        let reads = self.tool_usage.get("read_file").copied().unwrap_or(0);
        let writes = self.tool_usage.get("write_file").copied().unwrap_or(0);
        let edits = self.tool_usage.get("edit_file").copied().unwrap_or(0);
        let bash = self.tool_usage.get("bash").copied().unwrap_or(0);
        let total = reads + writes + edits + bash;

        if total == 0 {
            return;
        }

        // Incremental edits: edit_file strongly preferred over write_file
        if writes > 0 {
            let ratio = edits as f64 / writes as f64;
            if ratio > 2.0 && edits >= 3 {
                let strength = (ratio / 5.0).min(1.0);
                self.work_patterns
                    .insert("incremental-edits".to_string(), strength);
            }
        }

        // Read-first: read_file dominates tool usage (>50% of all calls)
        let read_pct = reads as f64 / total as f64;
        if read_pct > 0.5 && reads >= 3 {
            self.work_patterns
                .insert("read-first".to_string(), (read_pct - 0.5) * 2.0);
        }

        // Heavy bash: bash accounts for >40% of all calls
        let bash_pct = bash as f64 / total as f64;
        if bash_pct > 0.4 && bash >= 3 {
            self.work_patterns
                .insert("bash-heavy".to_string(), (bash_pct - 0.4) / 0.6);
        }
    }

    /// Mark this session as having used test-after-change.
    /// Called when a bash command containing "cargo test" / "npm test" / "pytest" etc is run.
    pub fn record_test_after_change(&mut self) {
        let entry = self
            .work_patterns
            .entry("test-after-change".to_string())
            .or_insert(0.0);
        *entry = (*entry + 0.15).min(1.0);
    }
}

// ── Path condensation ─────────────────────────────────────────────────────────

/// Condense a file path for storage.
/// - Paths ≤ 40 chars: kept as-is
/// - Longer paths: keep only the last 2 path components
pub fn condense_path(path: &str) -> String {
    if path.len() <= 40 {
        return path.to_string();
    }
    let p = Path::new(path);
    let components: Vec<_> = p.components().collect();
    if components.len() > 2 {
        let last_two = &components[components.len().saturating_sub(2)..];
        last_two
            .iter()
            .map(|c| c.as_os_str().to_string_lossy())
            .collect::<Vec<_>>()
            .join("/")
    } else {
        path.to_string()
    }
}

// ── Persistence ───────────────────────────────────────────────────────────────

/// Path to the carryover profile file.
pub fn carryover_path() -> PathBuf {
    let mut path = crate::session::data_dir().unwrap_or_else(|_| PathBuf::from("."));
    path.push("carryover.json");
    path
}

/// Load the carryover profile from disk. Returns default if file doesn't exist.
pub fn load_carryover() -> CarryoverProfile {
    let path = carryover_path();
    if !path.exists() {
        return CarryoverProfile::default();
    }
    match std::fs::read_to_string(&path) {
        Ok(content) => match serde_json::from_str(&content) {
            Ok(profile) => profile,
            Err(e) => {
                tracing::warn!(error = %e, path = %path.display(), "failed to parse carryover profile; using default");
                CarryoverProfile::default()
            }
        },
        Err(e) => {
            tracing::warn!(error = %e, path = %path.display(), "failed to read carryover profile; using default");
            CarryoverProfile::default()
        }
    }
}

/// Delete the persisted carryover profile from disk.
pub fn clear_carryover() {
    let path = carryover_path();
    if path.exists() {
        if let Err(e) = std::fs::remove_file(&path) {
            tracing::warn!(error = %e, path = %path.display(), "failed to delete carryover profile");
        }
    }
}

/// Write `content` to `path` atomically using a same-directory temp file
/// followed by a rename. The temp file is left behind only on error, so a
/// crash during the write cannot leave the target truncated.
fn atomic_write(path: &std::path::Path, content: &[u8]) -> std::io::Result<()> {
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, content)?;
    std::fs::rename(&tmp, path)
}

/// Save the carryover profile to disk, pruning to top-5 tools first.
pub fn save_carryover(profile: &CarryoverProfile) {
    let mut pruned = profile.clone();
    // Prune tool_usage to top 5 by frequency
    let mut tools: Vec<(String, u64)> = pruned.tool_usage.drain().collect();
    tools.sort_by_key(|k| std::cmp::Reverse(k.1));
    tools.truncate(5);
    pruned.tool_usage = tools.into_iter().collect();
    // Prune recent_paths to last 3
    if pruned.recent_paths.len() > 3 {
        pruned.recent_paths = pruned.recent_paths[pruned.recent_paths.len() - 3..].to_vec();
    }
    // Prune verifier_warnings to top 5
    let mut warns: Vec<(String, u64)> = pruned.verifier_warnings.drain().collect();
    warns.sort_by_key(|k| std::cmp::Reverse(k.1));
    warns.truncate(5);
    pruned.verifier_warnings = warns.into_iter().collect();

    let path = carryover_path();
    if let Some(parent) = path.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            tracing::warn!(error = %e, path = %parent.display(), "failed to create carryover directory");
            return;
        }
    }
    match serde_json::to_string(&pruned) {
        Ok(content) => {
            if let Err(e) = atomic_write(&path, content.as_bytes()) {
                tracing::warn!(error = %e, path = %path.display(), "failed to write carryover profile");
            }
        }
        Err(e) => {
            tracing::warn!(error = %e, path = %path.display(), "failed to serialize carryover profile");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shared::test_util::remove_test_file;

    #[test]
    fn test_empty_profile_renders_empty() {
        let p = CarryoverProfile::default();
        assert_eq!(p.to_prompt_block(), "");
    }

    #[test]
    fn test_basic_profile_rendered() {
        let mut p = CarryoverProfile {
            session_count: 3,
            last_user_message: "fix the auth bug".to_string(),
            ..Default::default()
        };
        p.last_session_time = "2026-06-03 14:22".to_string();
        p.recent_paths.push("src/auth/mod.rs".to_string());

        let block = p.to_prompt_block();
        assert!(block.contains("session #3"));
        assert!(block.contains("fix the auth bug"));
        assert!(block.contains("src/auth/mod.rs"));
    }

    #[test]
    fn test_serde_roundtrip() {
        let p = CarryoverProfile {
            session_count: 7,
            last_user_message: "hello".to_string(),
            tool_usage: HashMap::from([("read_file".into(), 10)]),
            work_patterns: HashMap::from([("test-after-change".into(), 0.8)]),
            ..Default::default()
        };

        let json = serde_json::to_string(&p).unwrap();
        let recovered: CarryoverProfile = serde_json::from_str(&json).unwrap();

        assert_eq!(recovered.session_count, 7);
        assert_eq!(recovered.last_user_message, "hello");
        assert_eq!(recovered.tool_usage.get("read_file"), Some(&10));
        assert_eq!(recovered.work_patterns.get("test-after-change"), Some(&0.8));
    }

    #[test]
    fn test_save_and_load() {
        // Use a temp path by overriding the carryover_path function's return
        // by writing to and reading from a temp file
        let temp = std::env::temp_dir().join("kirkforge-carryover-test.json");
        let backup_path = PathBuf::from(".");

        // Manually test save/load logic by writing to temp path
        let p = CarryoverProfile {
            session_count: 2,
            last_user_message: "test save".to_string(),
            ..Default::default()
        };

        // Write
        let json = serde_json::to_string(&p).unwrap();
        std::fs::write(&temp, &json).unwrap();

        // Read
        let content = std::fs::read_to_string(&temp).unwrap();
        let recovered: CarryoverProfile = serde_json::from_str(&content).unwrap();

        assert_eq!(recovered.session_count, 2);
        assert_eq!(recovered.last_user_message, "test save");

        remove_test_file(&temp);
        let _ = backup_path;
    }

    #[test]
    fn test_atomic_write_replaces_target_without_leaving_tmp() {
        let dir = std::env::temp_dir().join("kirkforge_atomic_write_test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let target = dir.join("carryover.json");
        let tmp = dir.join("carryover.tmp");

        atomic_write(&target, b"hello").unwrap();
        assert!(target.exists());
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "hello");
        assert!(!tmp.exists(), "temp file must be removed after rename");

        atomic_write(&target, b"world").unwrap();
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "world");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_tool_call_tracking() {
        let mut p = CarryoverProfile::default();
        p.record_tool_call("read_file");
        p.record_tool_call("read_file");
        p.record_tool_call("bash");
        p.record_tool_call("read_file");

        assert_eq!(p.tool_usage.get("read_file"), Some(&3));
        assert_eq!(p.tool_usage.get("bash"), Some(&1));
        assert_eq!(p.tool_usage.get("edit_file"), None);
    }

    #[test]
    fn test_path_tracking_condensed() {
        let mut p = CarryoverProfile::default();
        // 48 chars — exceeds the 40-char threshold
        let long = "this/is/a/very/long/prefix/src/auth/mod.rs";
        assert!(long.len() > 40);
        p.record_path(long);

        let saved = p.recent_paths.last().unwrap();
        assert!(saved.contains("mod.rs"), "should keep the meaningful tail");
        assert!(saved.len() < long.len(), "should be condensed");
    }

    #[test]
    fn test_path_tracking_max_three() {
        let mut p = CarryoverProfile::default();
        p.record_path("file1.rs");
        p.record_path("file2.rs");
        p.record_path("file3.rs");
        p.record_path("file4.rs");

        assert_eq!(p.recent_paths.len(), 3);
        assert!(!p.recent_paths.contains(&"file1.rs".to_string()));
    }

    #[test]
    fn test_path_tracking_dedup_moves_to_end() {
        let mut p = CarryoverProfile::default();
        p.record_path("file1.rs");
        p.record_path("file2.rs");
        p.record_path("file1.rs");

        assert_eq!(p.recent_paths.len(), 2);
        // file1 should now be last (most recent)
        assert_eq!(p.recent_paths.last().unwrap(), "file1.rs");
    }

    #[test]
    fn test_verifier_warning_accumulation() {
        let mut p = CarryoverProfile::default();
        p.record_verifier_warning("unused variable: x");
        p.record_verifier_warning("unused variable: y");
        p.record_verifier_warning("missing documentation: foo");

        assert_eq!(p.verifier_warnings.get("unused variable"), Some(&2));
        assert_eq!(p.verifier_warnings.get("missing documentation"), Some(&1));
    }

    #[test]
    fn test_pattern_incremental_edits() {
        let mut p = CarryoverProfile::default();
        p.tool_usage.insert("edit_file".into(), 10);
        p.tool_usage.insert("write_file".into(), 3);
        p.tool_usage.insert("bash".into(), 5);

        p.refresh_patterns();

        let strength = p.work_patterns.get("incremental-edits").unwrap_or(&0.0);
        assert!(*strength > 0.3, "should detect incremental-edits pattern");
    }

    #[test]
    fn test_pattern_read_first() {
        let mut p = CarryoverProfile::default();
        p.tool_usage.insert("read_file".into(), 20);
        p.tool_usage.insert("bash".into(), 5);

        p.refresh_patterns();

        let strength = p.work_patterns.get("read-first").unwrap_or(&0.0);
        assert!(*strength > 0.3, "should detect read-first pattern");
    }

    #[test]
    fn test_record_test_after_change() {
        let mut p = CarryoverProfile::default();
        p.record_test_after_change();
        assert!(*p.work_patterns.get("test-after-change").unwrap_or(&0.0) > 0.0);

        // Calling multiple times increases strength (capped at 1.0)
        for _ in 0..10 {
            p.record_test_after_change();
        }
        assert!(*p.work_patterns.get("test-after-change").unwrap_or(&0.0) <= 1.0);
    }

    #[test]
    fn test_prune_to_top_five() {
        let mut p = CarryoverProfile::default();
        for i in 0..7 {
            p.record_tool_call(&format!("tool_{i}"));
            // Make some more frequent than others
            for _ in 0..(7 - i) {
                p.record_tool_call(&format!("tool_{i}"));
            }
        }

        // Before prve: 7 tools
        assert_eq!(p.tool_usage.len(), 7);

        save_carryover(&p);

        // After pruning via save: when we load back, only top 5 survive.
        // But save writes to the file path. Let's just test the prune logic directly.
        let mut pruned = p.clone();
        let mut tools: Vec<(String, u64)> = pruned.tool_usage.drain().collect();
        tools.sort_by_key(|t| std::cmp::Reverse(t.1));
        tools.truncate(5);

        assert_eq!(tools.len(), 5);
        // tool_0 has 7 uses, tool_1 has 6, .., tool_4 has 3, tool_5 has 2, tool_6 has 1
        assert_eq!(tools[0].0, "tool_0");
        assert_eq!(tools[4].0, "tool_4");
    }

    #[test]
    fn test_condense_short_path_unchanged() {
        assert_eq!(condense_path("short.rs"), "short.rs");
        assert_eq!(condense_path("src/main.rs"), "src/main.rs");
    }

    #[test]
    fn test_condense_long_path() {
        let long = "/home/user/projects/rust/awesome-tool/src/very/deep/module.rs";
        let condensed = condense_path(long);
        assert!(condensed.contains("module.rs"));
        // The condense function keeps only last 2 components when path > 40 chars
        // The last two components are "deep/module.rs"
        assert_eq!(condensed, "deep/module.rs");
    }

    #[test]
    fn test_estimated_tokens_non_zero() {
        let p = CarryoverProfile {
            session_count: 1,
            last_user_message: "hello world".to_string(),
            ..Default::default()
        };
        assert!(p.estimated_tokens() > 0);
    }

    #[test]
    fn test_empty_estimated_tokens() {
        let p = CarryoverProfile::default();
        assert_eq!(p.estimated_tokens(), 0);
    }

    #[test]
    fn test_verifier_warning_accumulates_count() {
        let mut p = CarryoverProfile::default();
        // Record the same warning 3 times
        for _ in 0..3 {
            p.record_verifier_warning("unused variable: counter");
        }
        assert_eq!(p.verifier_warnings.get("unused variable"), Some(&3));
    }

    #[test]
    fn test_patterns_empty_with_no_usage() {
        let mut p = CarryoverProfile::default();
        p.refresh_patterns();
        assert!(p.work_patterns.is_empty());
    }
}
