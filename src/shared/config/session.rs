use serde::{Deserialize, Serialize};

// WO 45.37: typed artifact policy. Replaces the implicit
// `worktree_enabled: bool` + `auto_apply_patch: bool` pair (WO 35.2 +
// 41.1) whose 4th combination (worktree off + auto-apply on) was
// silently meaningless. The enum makes the 3 valid states explicit and
// the invalid 4th state unrepresentable. Backward compat: the
// `SessionConfig` custom Deserialize also accepts the legacy two-bool
// TOML form (`worktree_enabled = true` + `auto_apply_patch = false` →
// `PatchOnly`); see `LegacyArtifactBools` below.
// `ApprovalToApply` and `Commit` (reviewer's proposed variants) are OUT
// OF SCOPE — they are new features (interactive approval gate,
// auto-commit) with no current implementation.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactPolicy {
    // No worktree; coder writes directly to parent working tree.
    #[default]
    DirectWrite,
    // Worktree isolation; patch surfaced as text for manual application.
    PatchOnly,
    // Worktree isolation; patch `git apply`'d to parent working tree.
    AutoApply,
}

impl ArtifactPolicy {
    // Mirrors the old `worktree_enabled` gate. True for PatchOnly/AutoApply.
    pub fn is_worktree_enabled(self) -> bool {
        matches!(self, ArtifactPolicy::PatchOnly | ArtifactPolicy::AutoApply)
    }

    // Mirrors the old `auto_apply_patch` gate. True only for AutoApply.
    pub fn is_auto_apply(self) -> bool {
        matches!(self, ArtifactPolicy::AutoApply)
    }
}

// Legacy two-bool TOML form, captured as sibling keys of `artifact_policy`
// in the `[session]` table. `worktree_enabled=false` → DirectWrite
// (auto_apply ignored, matching the old "Only meaningful with
// worktree_enabled" doc). `worktree_enabled=true, auto_apply_patch=false`
// → PatchOnly. `worktree_enabled=true, auto_apply_patch=true` → AutoApply.
// Only consulted when `artifact_policy` is absent (None) — an explicit
// `artifact_policy` string always wins.
#[derive(Deserialize)]
struct LegacyArtifactBools {
    #[serde(default)]
    worktree_enabled: bool,
    #[serde(default)]
    auto_apply_patch: bool,
}

impl LegacyArtifactBools {
    fn resolve(self) -> ArtifactPolicy {
        if self.worktree_enabled && self.auto_apply_patch {
            ArtifactPolicy::AutoApply
        } else if self.worktree_enabled {
            ArtifactPolicy::PatchOnly
        } else {
            ArtifactPolicy::DirectWrite
        }
    }
}

fn default_carryover_enabled() -> bool {
    true
}

fn default_preserve_recent_messages() -> usize {
    2
}

fn default_checkpoint_interval_messages() -> usize {
    // WO 45.42: default-on periodic checkpoints. 0 meant only tool-batch
    // checkpoints ran; a long no-tool stretch that crashed lost everything
    // back to the last tool batch. 20 closes the gap at sub-ms fsync cost
    // per batch on a hot SSD (10-50ms on NFS — operators can lower via
    // KF_CODE_CHECKPOINT_INTERVAL_MESSAGES).
    20
}

fn default_compaction_use_heuristic() -> bool {
    false
}

fn default_compaction_drop_threshold() -> f64 {
    0.5
}

#[derive(Debug, Clone, Serialize)]
pub struct SessionConfig {
    pub carryover_enabled: bool,
    pub preserve_recent_messages: usize,
    pub checkpoint_interval_messages: usize,
    /// WO 45.37: typed artifact policy replacing the
    /// `worktree_enabled` + `auto_apply_patch` bool pair (WO 35.2 +
    /// 41.1). Default `DirectWrite` (no worktree). The custom
    /// `Deserialize` also accepts the legacy two-bool TOML form for
    /// backward compat with existing configs.
    pub artifact_policy: ArtifactPolicy,
    // ponytail: renamed from compaction_use_llm in WO 21.6-R5 — the
    // actual impl is heuristic keyword extraction, not LLM
    // summarization. The serde alias preserves backward compat with
    // existing configs that use the old name.
    pub compaction_use_heuristic: bool,
    /// Fraction of content that must be dropped by the heuristic before
    /// the LLM summarizer is tried. Default 0.5 (50%). Only used when
    /// `compaction_use_heuristic` is `true`.
    pub compaction_drop_threshold: f64,
    /// Maximum number of file stems included in the system prompt context.
    /// Files exceeding `stem_file_cap` bytes are truncated. Defaults to 4096
    /// when `None` (see `STEM_FILE_CAP` in `session/executor/turn.rs`).
    pub stem_file_cap: Option<usize>,
    /// Seconds to wait for the executor task to shut down gracefully before
    /// aborting it. Defaults to 3 s.
    pub shutdown_timeout_secs: Option<u64>,
}

// Helper for the custom Deserialize: mirrors SessionConfig but with
// `artifact_policy: Option<ArtifactPolicy>` (to detect an explicit
// string) and the two legacy bools as private serde-only captures.
// `#[serde(default)]` on the struct means every field fills from its
// Default when absent, so partial configs still load.
#[derive(Deserialize)]
#[serde(default)]
struct SessionConfigHelper {
    carryover_enabled: bool,
    preserve_recent_messages: usize,
    checkpoint_interval_messages: usize,
    artifact_policy: Option<ArtifactPolicy>,
    worktree_enabled: bool,
    auto_apply_patch: bool,
    #[serde(alias = "compaction_use_llm")]
    compaction_use_heuristic: bool,
    compaction_drop_threshold: f64,
    stem_file_cap: Option<usize>,
    shutdown_timeout_secs: Option<u64>,
}

impl Default for SessionConfigHelper {
    fn default() -> Self {
        Self {
            carryover_enabled: default_carryover_enabled(),
            preserve_recent_messages: default_preserve_recent_messages(),
            checkpoint_interval_messages: default_checkpoint_interval_messages(),
            artifact_policy: None,
            worktree_enabled: false,
            auto_apply_patch: false,
            compaction_use_heuristic: default_compaction_use_heuristic(),
            compaction_drop_threshold: default_compaction_drop_threshold(),
            stem_file_cap: None,
            shutdown_timeout_secs: None,
        }
    }
}

impl<'de> Deserialize<'de> for SessionConfig {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let h = SessionConfigHelper::deserialize(deserializer)?;
        // An explicit `artifact_policy` string wins; otherwise fall back
        // to the legacy two-bool form (which defaults to DirectWrite when
        // both are absent).
        let artifact_policy = h.artifact_policy.unwrap_or_else(|| {
            LegacyArtifactBools {
                worktree_enabled: h.worktree_enabled,
                auto_apply_patch: h.auto_apply_patch,
            }
            .resolve()
        });
        Ok(SessionConfig {
            carryover_enabled: h.carryover_enabled,
            preserve_recent_messages: h.preserve_recent_messages,
            checkpoint_interval_messages: h.checkpoint_interval_messages,
            artifact_policy,
            compaction_use_heuristic: h.compaction_use_heuristic,
            compaction_drop_threshold: h.compaction_drop_threshold,
            stem_file_cap: h.stem_file_cap,
            shutdown_timeout_secs: h.shutdown_timeout_secs,
        })
    }
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            carryover_enabled: default_carryover_enabled(),
            preserve_recent_messages: default_preserve_recent_messages(),
            checkpoint_interval_messages: default_checkpoint_interval_messages(),
            artifact_policy: ArtifactPolicy::DirectWrite,
            compaction_use_heuristic: default_compaction_use_heuristic(),
            compaction_drop_threshold: default_compaction_drop_threshold(),
            stem_file_cap: None,
            shutdown_timeout_secs: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ponytail: ceiling — the invalid 4th state (worktree off + auto-apply
    // on) is now unrepresentable: the legacy-bool fallback maps it to
    // DirectWrite (auto_apply ignored, matching the old "Only meaningful
    // with worktree_enabled" doc). This test pins that contract.
    #[test]
    fn legacy_bool_pair_maps_to_enum() {
        // New string form.
        assert_eq!(
            toml::from_str::<SessionConfig>(r#"artifact_policy = "direct_write""#)
                .unwrap()
                .artifact_policy,
            ArtifactPolicy::DirectWrite
        );
        assert_eq!(
            toml::from_str::<SessionConfig>(r#"artifact_policy = "patch_only""#)
                .unwrap()
                .artifact_policy,
            ArtifactPolicy::PatchOnly
        );
        assert_eq!(
            toml::from_str::<SessionConfig>(r#"artifact_policy = "auto_apply""#)
                .unwrap()
                .artifact_policy,
            ArtifactPolicy::AutoApply
        );

        // Legacy two-bool form.
        assert_eq!(
            toml::from_str::<SessionConfig>(
                r#"worktree_enabled = true
                auto_apply_patch = false"#
            )
            .unwrap()
            .artifact_policy,
            ArtifactPolicy::PatchOnly
        );
        assert_eq!(
            toml::from_str::<SessionConfig>(
                r#"worktree_enabled = true
                auto_apply_patch = true"#
            )
            .unwrap()
            .artifact_policy,
            ArtifactPolicy::AutoApply
        );
        // Invalid 4th state: worktree off + auto-apply on → DirectWrite
        // (auto_apply silently ignored, as documented).
        assert_eq!(
            toml::from_str::<SessionConfig>(
                r#"worktree_enabled = false
                auto_apply_patch = true"#
            )
            .unwrap()
            .artifact_policy,
            ArtifactPolicy::DirectWrite
        );
        // Both absent → default DirectWrite.
        assert_eq!(
            toml::from_str::<SessionConfig>("").unwrap().artifact_policy,
            ArtifactPolicy::DirectWrite
        );
    }

    #[test]
    fn accessors_mirror_old_bool_gates() {
        assert!(!ArtifactPolicy::DirectWrite.is_worktree_enabled());
        assert!(!ArtifactPolicy::DirectWrite.is_auto_apply());
        assert!(ArtifactPolicy::PatchOnly.is_worktree_enabled());
        assert!(!ArtifactPolicy::PatchOnly.is_auto_apply());
        assert!(ArtifactPolicy::AutoApply.is_worktree_enabled());
        assert!(ArtifactPolicy::AutoApply.is_auto_apply());
    }
}
