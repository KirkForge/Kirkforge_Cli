//! Approval-mode keyboard handler.
//!
//! Only invoked when `state.pending_approval.is_some()`. Translates
//! y/n/a/Esc into `ApprovalResponse` and sends it back to the executor
//! over the oneshot channel stored on the pending approval.
//!
//! **v1.2-p11:** also handles PageUp/PageDown/Up/Down/Home/End to
//! scroll the args preview when the approval is taller than the
//! dialog's visible window. Scroll bounds are clamped against
//! `state.approval_max_scroll`, which the renderer writes each
//! frame — same off-by-N avoidance pattern as `max_scroll` for
//! the chat view.

use crate::session::executor::ApprovalResponse;
use crate::tui::app::AppState;
use crossterm::event::{KeyCode, KeyEvent};

/// How many lines a PageUp / PageDown jumps. Mirrors the chat-view
/// page size (10 lines) so the muscle memory carries over.
const APPROVAL_PAGE_SIZE: usize = 10;

pub fn handle_approval_key(key: KeyEvent, state: &mut AppState) {
    let approval = match state.pending_approval.take() {
        Some(a) => a,
        None => return,
    };

    let response = match key.code {
        KeyCode::Char('y') | KeyCode::Char('Y') => Some(ApprovalResponse::Approved),
        KeyCode::Char('n') | KeyCode::Char('N') => Some(ApprovalResponse::Denied),
        KeyCode::Char('a') | KeyCode::Char('A') => Some(ApprovalResponse::AlwaysApprove),
        KeyCode::Esc => Some(ApprovalResponse::Denied),
        // Scroll keys — operate on the args preview, NOT the chat.
        // (Approval-mode keys never reach the chat-view scroll handler.)
        KeyCode::PageUp => {
            state.pending_approval = Some(approval);
            state.approval_scroll = state
                .approval_scroll
                .saturating_sub(APPROVAL_PAGE_SIZE);
            return;
        }
        KeyCode::PageDown => {
            state.pending_approval = Some(approval);
            state.approval_scroll = state
                .approval_scroll
                .saturating_add(APPROVAL_PAGE_SIZE)
                .min(state.approval_max_scroll);
            return;
        }
        KeyCode::Up => {
            state.pending_approval = Some(approval);
            state.approval_scroll = state.approval_scroll.saturating_sub(1);
            return;
        }
        KeyCode::Down => {
            state.pending_approval = Some(approval);
            let next = state.approval_scroll + 1;
            state.approval_scroll = next.min(state.approval_max_scroll);
            return;
        }
        KeyCode::Home => {
            state.pending_approval = Some(approval);
            state.approval_scroll = 0;
            return;
        }
        KeyCode::End => {
            state.pending_approval = Some(approval);
            state.approval_scroll = state.approval_max_scroll;
            return;
        }
        _ => {
            // Unhandled key — put the approval back so the dialog stays.
            state.pending_approval = Some(approval);
            return;
        }
    };

    if let Some(resp) = response {
        if matches!(resp, ApprovalResponse::AlwaysApprove) {
            // **v1.2-p13 — permission rule persistence.** Build a rule
            // from the tool name + args of THIS pending approval (e.g.
            // `allow bash:command=cargo test`), push it into the
            // session's `permission_rules`, and persist to disk so it
            // survives across sessions. This is the actual control
            // point — the user-facing `[A]lways` key triggers this
            // synchronously before the response is sent.
            //
            // The executor side (`executor.rs::run_turn`) ALSO pushes
            // the same rule into its own `permission_rules` clone
            // (Belt + suspenders: handles the headless non-TUI path
            // where the TUI never gets to run, and keeps the rest of
            // the current turn consistent without needing a config
            // reload). The TUI push is the source of truth for
            // **disk persistence**; the executor push is the source of
            // truth for the in-memory session.
            //
            // `push_rule_unique` dedups so mashing `[A]lways` twice
            // doesn't create duplicate rules.
            let rule = crate::shared::permission::suggest_rule(
                &approval.tool_name,
                &approval.args,
            );
            push_rule_unique(&mut state.config.permission_rules, rule);
            let _ = crate::session::config::save_config(&state.config);
        }
        // The user just decided — clear the scroll state so the next
        // approval (if any) starts fresh at the top.
        state.approval_scroll = 0;
        state.approval_max_scroll = 0;
        if let Some(tx) = approval.responder {
            let _ = tx.send(resp);
        }
    }
}

/// Push a permission rule into a `Vec<PermissionRule>`, deduplicating
/// against an existing identical rule by `(tool, key, pattern)`. The
/// action of the existing rule is preserved. Mirrors
/// `executor::push_rule_unique` — keep them in sync.
fn push_rule_unique(
    rules: &mut Vec<crate::shared::permission::PermissionRule>,
    new_rule: crate::shared::permission::PermissionRule,
) {
    let duplicate = rules.iter().any(|r| {
        r.tool == new_rule.tool
            && r.key == new_rule.key
            && r.pattern == new_rule.pattern
    });
    if !duplicate {
        rules.push(new_rule);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shared::Config;
    use crate::tui::app::PendingApproval;
    use crossterm::event::KeyModifiers;
    use serde_json::json;

    fn make_state_with_approval(args: serde_json::Value) -> AppState {
        let mut s = AppState::new(Config::default());
        s.pending_approval = Some(PendingApproval {
            tool_name: "bash".into(),
            args,
            responder: None,
        });
        s
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    /// PageDown moves scroll forward, clamped to max_scroll.
    #[test]
    fn test_pagedown_advances_and_clamps() {
        let mut s = make_state_with_approval(json!({"command": "ls"}));
        s.approval_scroll = 0;
        s.approval_max_scroll = 50;
        handle_approval_key(key(KeyCode::PageDown), &mut s);
        assert_eq!(s.approval_scroll, 10);
        // Far past the max — should clamp.
        s.approval_scroll = 45;
        handle_approval_key(key(KeyCode::PageDown), &mut s);
        assert_eq!(s.approval_scroll, 50);
    }

    /// PageUp moves scroll backward via saturating_sub.
    #[test]
    fn test_pageup_saturates_at_zero() {
        let mut s = make_state_with_approval(json!({"command": "ls"}));
        s.approval_scroll = 3;
        s.approval_max_scroll = 50;
        handle_approval_key(key(KeyCode::PageUp), &mut s);
        assert_eq!(s.approval_scroll, 0);
        // Already at 0 — stays at 0.
        handle_approval_key(key(KeyCode::PageUp), &mut s);
        assert_eq!(s.approval_scroll, 0);
    }

    /// Down arrow advances by 1.
    #[test]
    fn test_down_arrow_advances_one() {
        let mut s = make_state_with_approval(json!({"command": "ls"}));
        s.approval_scroll = 5;
        s.approval_max_scroll = 50;
        handle_approval_key(key(KeyCode::Down), &mut s);
        assert_eq!(s.approval_scroll, 6);
    }

    /// Up arrow retreats by 1 via saturating_sub.
    #[test]
    fn test_up_arrow_saturates_at_zero() {
        let mut s = make_state_with_approval(json!({"command": "ls"}));
        s.approval_scroll = 0;
        s.approval_max_scroll = 50;
        handle_approval_key(key(KeyCode::Up), &mut s);
        assert_eq!(s.approval_scroll, 0);
    }

    /// Home jumps to 0, End jumps to max.
    #[test]
    fn test_home_end_jumps() {
        let mut s = make_state_with_approval(json!({"command": "ls"}));
        s.approval_scroll = 20;
        s.approval_max_scroll = 100;
        handle_approval_key(key(KeyCode::End), &mut s);
        assert_eq!(s.approval_scroll, 100);
        handle_approval_key(key(KeyCode::Home), &mut s);
        assert_eq!(s.approval_scroll, 0);
    }

    /// y/n/a/Esc still work — the approval gets consumed and scroll resets.
    #[test]
    fn test_y_consumes_approval_and_resets_scroll() {
        let mut s = make_state_with_approval(json!({"command": "ls"}));
        s.approval_scroll = 7;
        s.approval_max_scroll = 50;
        handle_approval_key(key(KeyCode::Char('y')), &mut s);
        // Approval is consumed (responder was None so no send — that's fine)
        assert!(s.pending_approval.is_none());
        // Scroll state reset for the next approval.
        assert_eq!(s.approval_scroll, 0);
        assert_eq!(s.approval_max_scroll, 0);
    }

    /// Unknown keys leave both the approval and the scroll state intact.
    #[test]
    fn test_unknown_key_preserves_state() {
        let mut s = make_state_with_approval(json!({"command": "ls"}));
        s.approval_scroll = 7;
        s.approval_max_scroll = 50;
        handle_approval_key(key(KeyCode::Char('z')), &mut s);
        assert!(s.pending_approval.is_some());
        assert_eq!(s.approval_scroll, 7);
        assert_eq!(s.approval_max_scroll, 50);
    }

    /// **v1.2-p13 — `[A]lways` builds a permission rule, NOT a
    /// blanket `auto_approve` flip.** This is the regression guard
    /// for the old `state.config.auto_approve = true;` line.
    #[test]
    fn test_always_approves_saves_permission_rule() {
        let mut s = make_state_with_approval(json!({"command": "cargo test --release"}));
        // Start with auto_approve = false and no rules — realistic state.
        s.config.auto_approve = false;
        s.config.permission_rules.clear();
        assert!(!s.config.auto_approve);
        assert!(s.config.permission_rules.is_empty());

        // Capture state.config before [A]lways, because save_config
        // would write to the real config path; the test only checks
        // the in-memory state (the path-writing is exercised in
        // integration tests, not unit tests).
        handle_approval_key(key(KeyCode::Char('a')), &mut s);

        // **The new rule should be in permission_rules.**
        assert_eq!(
            s.config.permission_rules.len(),
            1,
            "[A]lways should have appended exactly one rule"
        );
        let r = &s.config.permission_rules[0];
        assert_eq!(r.tool, "bash");
        assert_eq!(r.key, "command");
        assert_eq!(r.pattern, "cargo test --release");
        assert_eq!(
            r.action,
            crate::shared::permission::PermissionAction::Allow
        );

        // **auto_approve must NOT have been flipped.** The user
        // asked for "always this specific command", not "always
        // everything". The new rule is the user's intent.
        assert!(
            !s.config.auto_approve,
            "[A]lways should NOT flip auto_approve — the new rule is the user's intent"
        );

        // **The approval is still consumed (responder was None — fine).**
        assert!(s.pending_approval.is_none());
    }

    /// `[A]lways` on an `edit_file` approval should build a rule
    /// keyed on `path`, not `command`. The key selection is in
    /// `permission::suggest_rule` — this test catches the
    /// regression where TUI would hardcode `command`.
    #[test]
    fn test_always_approves_edit_file_uses_path_key() {
        // Build a state with a real edit_file approval (not via the
        // bash-only helper), so suggest_rule's tool-driven key
        // selection gets exercised end-to-end.
        let mut s = AppState::new(Config::default());
        s.pending_approval = Some(PendingApproval {
            tool_name: "edit_file".into(),
            args: json!({
                "path": "src/main.rs",
                "old_string": "old",
                "new_string": "new"
            }),
            responder: None,
        });
        s.config.permission_rules.clear();

        handle_approval_key(key(KeyCode::Char('A')), &mut s);

        assert_eq!(s.config.permission_rules.len(), 1);
        let r = &s.config.permission_rules[0];
        assert_eq!(r.tool, "edit_file");
        assert_eq!(
            r.key, "path",
            "edit_file approvals should build a rule keyed on `path`, not `command`"
        );
        assert_eq!(r.pattern, "src/main.rs");
    }

    /// `[A]` twice on the same call should NOT add duplicate rules.
    /// Regression guard for the `push_rule_unique` dedup.
    #[test]
    fn test_always_approves_dedups_repeated_calls() {
        let mut s = make_state_with_approval(json!({"command": "ls"}));
        s.config.permission_rules.clear();

        handle_approval_key(key(KeyCode::Char('a')), &mut s);
        // First push: one rule.
        assert_eq!(s.config.permission_rules.len(), 1);

        // Synthesise a second approval with the same args (simulating
        // a second `[A]lways` in a later turn). The real flow would
        // have a fresh `pending_approval` from the next destructive call.
        s.pending_approval = Some(PendingApproval {
            tool_name: "bash".into(),
            args: json!({"command": "ls"}),
            responder: None,
        });
        handle_approval_key(key(KeyCode::Char('a')), &mut s);
        // Still one rule — the dedup caught the second push.
        assert_eq!(
            s.config.permission_rules.len(),
            1,
            "Second [A]lways on the same call should not duplicate the rule"
        );
    }

    /// An existing user-written `Deny` rule should NOT be overwritten
    /// by `[A]lways` on the same pattern. The dedup preserves the
    /// existing action — a footgun guard.
    #[test]
    fn test_always_approves_does_not_overwrite_existing_deny() {
        let mut s = make_state_with_approval(json!({"command": "rm -rf build"}));
        s.config.permission_rules.clear();
        s.config.permission_rules.push(crate::shared::permission::PermissionRule {
            tool: "bash".into(),
            key: "command".into(),
            pattern: "rm -rf build".into(),
            action: crate::shared::permission::PermissionAction::Deny,
        });

        handle_approval_key(key(KeyCode::Char('a')), &mut s);

        // Still exactly one rule, and it's still Deny.
        assert_eq!(s.config.permission_rules.len(), 1);
        assert_eq!(
            s.config.permission_rules[0].action,
            crate::shared::permission::PermissionAction::Deny,
            "Existing Deny should not be overwritten by [A]lways's Allow on the same pattern"
        );
    }
}
