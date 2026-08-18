// Sandbox posture (WO 35.4): the set of sandbox layers active for this
// process, derived purely from the config plus compile-time cfg so /status
// can show the real layer state. The status-bar `⚠️ UNSANDBOXED` flag covers
// PathGuard only; this surfaces the bash-runner layers (Landlock, seccomp,
// network namespace) that were previously invisible in the TUI.

use crate::shared::Config;

pub struct SandboxPosture {
    pub path_guard: bool,
    pub landlock: bool,
    pub seccomp: bool,
    pub network_namespace: bool,
    pub worktree: bool,
}

impl SandboxPosture {
    pub fn from_config(config: &Config) -> Self {
        Self {
            // Mirrors PathGuard::is_sandboxed fed by access_from_config:
            // a non-empty sandbox_dir or any non-empty allowed_write_dirs entry.
            path_guard: config
                .security
                .sandbox_dir
                .as_deref()
                .is_some_and(|s| !s.is_empty())
                || config
                    .security
                    .allowed_write_dirs
                    .iter()
                    .any(|s| !s.is_empty()),
            // The landlock module is compiled exactly on Linux
            // (bash_runner/mod.rs `#[cfg(target_os = "linux")]`).
            landlock: cfg!(target_os = "linux"),
            // Opt-in cargo feature (WO 30.4), default-off.
            seccomp: cfg!(feature = "seccomp"),
            // CLONE_NEWNET is applied only when harden && no_network
            // (bash_runner setup_rlimits gate; --no-network requires --harden).
            network_namespace: config.security.sandbox.harden && config.security.sandbox.no_network,
            worktree: config.session.worktree_enabled,
        }
    }

    // The five /status checklist rows (WO 35.4 format), without the
    // "Sandbox:" header. Hints appear only on the ✗ rows the WO pins
    // them to (seccomp, network ns).
    pub fn checklist_lines(&self, sandbox_dir: Option<&str>) -> Vec<String> {
        let path_guard = if self.path_guard {
            match sandbox_dir.filter(|s| !s.is_empty()) {
                Some(dir) => format!("✓ (sandbox_dir={dir})"),
                None => "✓ (allowed_write_dirs)".to_string(),
            }
        } else {
            "✗".to_string()
        };
        vec![
            format!("  {:<13}{}", "PathGuard", path_guard),
            format!("  {:<13}{}", "Landlock", mark(self.landlock, "")),
            format!(
                "  {:<13}{}",
                "seccomp",
                mark(self.seccomp, "build with --features seccomp")
            ),
            format!(
                "  {:<13}{}",
                "network ns",
                mark(self.network_namespace, "pass --no-network")
            ),
            format!("  {:<13}{}", "worktree", mark(self.worktree, "")),
        ]
    }
}

fn mark(on: bool, hint_off: &str) -> String {
    if on {
        "✓".to_string()
    } else if hint_off.is_empty() {
        "✗".to_string()
    } else {
        format!("✗ ({hint_off})")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg_with(
        sandbox_dir: Option<&str>,
        allowed_write_dirs: &[&str],
        harden: bool,
        no_network: bool,
        worktree: bool,
    ) -> Config {
        let mut c = Config::default();
        c.security.sandbox_dir = sandbox_dir.map(str::to_string);
        c.security.allowed_write_dirs = allowed_write_dirs.iter().map(|s| s.to_string()).collect();
        c.security.sandbox.harden = harden;
        c.security.sandbox.no_network = no_network;
        c.session.worktree_enabled = worktree;
        c
    }

    #[test]
    fn default_config_posture() {
        // Default feature-off Linux build: Landlock compiled in, seccomp off,
        // no PathGuard scope, no netns, no worktree.
        let p = SandboxPosture::from_config(&Config::default());
        assert!(!p.path_guard);
        assert_eq!(p.landlock, cfg!(target_os = "linux"));
        assert_eq!(p.seccomp, cfg!(feature = "seccomp"));
        assert!(!p.network_namespace);
        assert!(!p.worktree);
    }

    #[test]
    fn seccomp_tracks_feature_flag() {
        // The layer must mirror the compile-time feature exactly, whichever
        // way this test binary was built.
        let p = SandboxPosture::from_config(&Config::default());
        assert_eq!(p.seccomp, cfg!(feature = "seccomp"));
    }

    #[test]
    fn network_namespace_requires_harden_and_flag() {
        // --no-network alone (e.g. set in config.toml without harden) must
        // NOT report the netns layer — bash_runner gates on harden && no_network.
        let p = SandboxPosture::from_config(&cfg_with(None, &[], false, true, false));
        assert!(!p.network_namespace);
        let p = SandboxPosture::from_config(&cfg_with(None, &[], true, true, false));
        assert!(p.network_namespace);
    }

    #[test]
    fn no_sandbox_case() {
        let p = SandboxPosture::from_config(&cfg_with(None, &[], false, false, false));
        assert!(!p.path_guard);
        assert!(!p.network_namespace);
        assert!(!p.worktree);
    }

    #[test]
    fn path_guard_via_sandbox_dir_or_write_dirs() {
        let p = SandboxPosture::from_config(&cfg_with(Some("."), &[], false, false, false));
        assert!(p.path_guard);
        let p = SandboxPosture::from_config(&cfg_with(None, &["/tmp/x"], false, false, false));
        assert!(p.path_guard);
        // empty strings don't count (access_from_config filters them)
        let p = SandboxPosture::from_config(&cfg_with(Some(""), &[""], false, false, false));
        assert!(!p.path_guard);
    }

    #[test]
    fn checklist_contains_all_five_layers() {
        let p = SandboxPosture::from_config(&cfg_with(Some("/repo"), &[], false, false, true));
        let lines = p.checklist_lines(Some("/repo"));
        let joined = lines.join("\n");
        for layer in ["PathGuard", "Landlock", "seccomp", "network ns", "worktree"] {
            assert!(
                joined.contains(layer),
                "missing layer {layer} in:\n{joined}"
            );
        }
        assert!(joined.contains("✓ (sandbox_dir=/repo)"));
        assert!(joined.contains("✗ (pass --no-network)"));
        if !cfg!(feature = "seccomp") {
            assert!(joined.contains("✗ (build with --features seccomp)"));
        }
        assert_eq!(lines.len(), 5);
    }

    #[test]
    fn checklist_path_guard_shows_write_dirs_when_no_sandbox_dir() {
        let p = SandboxPosture::from_config(&cfg_with(None, &["/tmp/x"], false, false, false));
        let joined = p.checklist_lines(None).join("\n");
        assert!(joined.contains("✓ (allowed_write_dirs)"));
    }
}
