//! Trust-tier enforcement for individual capabilities.
//!
//! A plugin's manifest may declare capabilities that require different trust
//! tiers (e.g. a `network` skill inside a plugin that also declares a `shell`
//! hook). The host's effective trust tier caps which capabilities are exposed
//! to the rest of the system.

use kirkforge_plugin::{Capability, TrustTier};

/// Policy that maps each capability kind to the minimum trust tier required
/// to run it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SandboxPolicy;

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
    use kirkforge_plugin::Capability;

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
