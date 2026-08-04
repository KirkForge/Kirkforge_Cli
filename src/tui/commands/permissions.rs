//! `/permissions` slash-command ops layer — pure functions over
//! `&Config` / `&mut Config` for listing, revoking, and clearing the
//! `permission_rules` created by the approval dialog's `[A]lways` key.
//!
//! Mirrors the `plugin_ops` split (WO 11.0): the ops layer is pure
//! (no I/O) so it is unit-testable without a TUI, and the TUI match
//! arm (or a future `kf-code permissions` CLI) is a thin wrapper
//! that calls these and persists via `save_config`.
//!
//! `list` is the source of truth for the 1-indexed position the user
//! passes to `revoke`. Indices are stable within a single `list` →
//! `revoke` pair; mutating between calls shifts indices.

use crate::shared::permission::{PermissionAction, PermissionRule};
use crate::shared::Config;

/// Format `permission_rules` as `#<i>  <tool>:<key>=<pattern> -> <action>`
/// rows, 1-indexed. Empty config returns a header noting no rules.
pub fn list(cfg: &Config) -> String {
    let rules = &cfg.security.permission_rules;
    if rules.is_empty() {
        return "No permission rules configured. Use [A]lways in the approval dialog to add one."
            .to_string();
    }
    let mut out = format!("Permission rules ({}):\n", rules.len());
    for (i, rule) in rules.iter().enumerate() {
        out.push_str(&format!("  #{}  {}\n", i + 1, format_rule(rule)));
    }
    out.push_str("Use /permissions revoke <i> to remove a rule, /permissions clear to remove all.");
    out
}

/// Remove the rule at 1-indexed position `index`. Returns a summary
/// like `"Revoked rule #3: bash:command=cargo test -> allow"`.
/// Out-of-bounds → `anyhow!("rule {i} does not exist; {n} rules configured")`.
pub fn revoke(cfg: &mut Config, index: usize) -> anyhow::Result<String> {
    let rules = &mut cfg.security.permission_rules;
    let n = rules.len();
    if index == 0 || index > n {
        anyhow::bail!("rule {index} does not exist; {n} rules configured");
    }
    let removed = rules.remove(index - 1);
    Ok(format!("Revoked rule #{index}: {}", format_rule(&removed)))
}

/// Remove all permission rules. Returns `"Cleared N rule(s)"`.
pub fn clear(cfg: &mut Config) -> String {
    let n = cfg.security.permission_rules.drain(..).count();
    format!("Cleared {n} rule(s).")
}

/// Format a single rule as `<tool>:<key>=<pattern> -> <action>`.
/// Matches the `(tool, key, pattern, action)` shape of `PermissionRule`
/// and the `bash:command=...` style the approval path builds.
fn format_rule(rule: &PermissionRule) -> String {
    let action = match rule.action {
        PermissionAction::Allow => "allow",
        PermissionAction::Ask => "ask",
        PermissionAction::Deny => "deny",
    };
    format!("{}:{}={} -> {}", rule.tool, rule.key, rule.pattern, action)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shared::permission::{PermissionAction, PermissionRule};

    fn rule(tool: &str, key: &str, pattern: &str, action: PermissionAction) -> PermissionRule {
        PermissionRule {
            tool: tool.into(),
            key: key.into(),
            pattern: pattern.into(),
            action,
        }
    }

    fn config_with_rules(rules: Vec<PermissionRule>) -> Config {
        let mut cfg = Config::default();
        cfg.security.permission_rules = rules;
        cfg
    }

    #[test]
    fn list_empty_config_returns_header_only() {
        let cfg = Config::default();
        let out = list(&cfg);
        assert!(out.contains("No permission rules"), "got: {out}");
        assert!(!out.contains("#1"), "empty list should have no rows: {out}");
    }

    #[test]
    fn list_populated_config_shows_rules_with_indices() {
        let cfg = config_with_rules(vec![
            rule("bash", "command", "cargo test", PermissionAction::Allow),
            rule("edit_file", "path", "src/*.rs", PermissionAction::Ask),
            rule("bash", "command", "rm -rf **", PermissionAction::Deny),
        ]);
        let out = list(&cfg);
        assert!(out.contains("Permission rules (3)"), "got: {out}");
        assert!(
            out.contains("#1  bash:command=cargo test -> allow"),
            "got: {out}"
        );
        assert!(
            out.contains("#2  edit_file:path=src/*.rs -> ask"),
            "got: {out}"
        );
        assert!(
            out.contains("#3  bash:command=rm -rf ** -> deny"),
            "got: {out}"
        );
        assert!(
            out.contains("/permissions revoke"),
            "list should mention revoke: {out}"
        );
    }

    #[test]
    fn revoke_in_bounds_removes_and_returns_summary() {
        let mut cfg = config_with_rules(vec![
            rule("bash", "command", "cargo test", PermissionAction::Allow),
            rule("edit_file", "path", "src/*.rs", PermissionAction::Ask),
        ]);
        let msg = revoke(&mut cfg, 1).unwrap();
        assert!(msg.contains("Revoked rule #1"), "got: {msg}");
        assert!(
            msg.contains("bash:command=cargo test -> allow"),
            "got: {msg}"
        );
        assert_eq!(cfg.security.permission_rules.len(), 1);
        assert_eq!(
            cfg.security.permission_rules[0].pattern, "src/*.rs",
            "revoke(1) should have shifted rule 2 into position 1"
        );
    }

    #[test]
    fn revoke_out_of_bounds_errors() {
        let mut cfg = config_with_rules(vec![rule(
            "bash",
            "command",
            "cargo test",
            PermissionAction::Allow,
        )]);
        let err = revoke(&mut cfg, 0).unwrap_err();
        assert!(
            err.to_string().contains("rule 0 does not exist"),
            "zero is not a valid 1-indexed position: {err}"
        );
        assert!(
            err.to_string().contains("1 rules configured"),
            "error should report the configured count: {err}"
        );
        let err = revoke(&mut cfg, 5).unwrap_err();
        assert!(
            err.to_string().contains("rule 5 does not exist"),
            "got: {err}"
        );
        assert_eq!(
            cfg.security.permission_rules.len(),
            1,
            "failed revoke must not mutate"
        );
    }

    #[test]
    fn clear_removes_all_and_returns_count() {
        let mut cfg = config_with_rules(vec![
            rule("bash", "command", "cargo test", PermissionAction::Allow),
            rule("edit_file", "path", "src/*.rs", PermissionAction::Ask),
            rule("bash", "command", "rm -rf **", PermissionAction::Deny),
        ]);
        let msg = clear(&mut cfg);
        assert!(msg.contains("Cleared 3 rule(s)"), "got: {msg}");
        assert!(cfg.security.permission_rules.is_empty());
    }

    #[test]
    fn clear_empty_returns_zero() {
        let mut cfg = Config::default();
        let msg = clear(&mut cfg);
        assert!(msg.contains("Cleared 0 rule(s)"), "got: {msg}");
        assert!(cfg.security.permission_rules.is_empty());
    }
}
