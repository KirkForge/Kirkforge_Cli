//! Trust-tier enforcement for individual capabilities.
//!
//! A plugin's manifest may declare capabilities that require different trust
//! tiers (e.g. a `network` skill inside a plugin that also declares a `shell`
//! hook). The host's effective trust tier caps which capabilities are exposed
//! to the rest of the system.

use crate::sdk::{Capability, TrustTier};

/// Policy that maps each capability kind to the minimum trust tier required
/// to run it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SandboxPolicy;

// ponytail: rlimits + trust-tier only, no OS isolation. Landlock lives in
// the bin's src/session/bash_runner/landlock.rs (shipped WO 27.1, graduated
// to the plugin tool spawn path in WO 43.11). This lib crate cannot import
// the bin's landlock module, and duplicating ~150 lines of syscall wrappers
// for the standalone host crate (tool.rs/hook.rs/verifier.rs) is not earned.
// ceiling: the standalone host spawn paths stay rlimits-only until landlock
// is extracted into a shared crate. Seccomp stays opt-in per ADR-054
// future-work (default-off; flip after real-workload allowlist tuning).
impl SandboxPolicy {
    /// Minimum trust tier required to use a capability.
    pub fn required_tier(cap: &Capability) -> TrustTier {
        match cap {
            Capability::Skill { .. } => TrustTier::ReadOnly,
            // v1 tools are shell commands; treat them as shell-equivalent.
            Capability::Tool { .. } => TrustTier::Shell,
            Capability::Hook { .. } => TrustTier::Shell,
            Capability::Verifier { .. } => TrustTier::ReadOnly,
        }
    }

    /// True if `tier` is sufficient to use `cap`.
    pub fn permits(tier: TrustTier, cap: &Capability) -> bool {
        tier.permits(Self::required_tier(cap))
    }

    /// Filter capabilities to only those permitted by `tier`.
    pub fn filter(tier: TrustTier, caps: &[Capability]) -> Vec<Capability> {
        caps.iter()
            .filter(|c| Self::permits(tier, c))
            .cloned()
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sdk::Capability;

    #[test]
    fn skill_requires_readonly() {
        let cap = Capability::Skill {
            trigger: "/x".into(),
            prompt: "x".into(),
            skill_file: None,
            model_hint: None,
        };
        assert_eq!(SandboxPolicy::required_tier(&cap), TrustTier::ReadOnly);
        assert!(SandboxPolicy::permits(TrustTier::ReadOnly, &cap));
    }

    #[test]
    fn hook_requires_shell() {
        let cap = Capability::Hook {
            event: "pre-turn".into(),
            command: std::path::PathBuf::from("hook.sh"),
        };
        assert_eq!(SandboxPolicy::required_tier(&cap), TrustTier::Shell);
        assert!(!SandboxPolicy::permits(TrustTier::ReadOnly, &cap));
        assert!(SandboxPolicy::permits(TrustTier::Shell, &cap));
    }
}
