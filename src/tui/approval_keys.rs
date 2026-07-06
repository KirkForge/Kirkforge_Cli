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
//!
//! **`handle_bang_approval_key`** is the parallel handler for the
//! `!` bash passthrough gate (review.md arch concern #1). It mirrors
//! the y/n/Esc surface but on Y runs the command locally and pushes
//! the formatted result into the chat. No executor round trip.

use crate::session::executor::ApprovalResponse;
use crate::shared::permission::push_rule_unique;
use crate::tui::app::{AppState, ConversationEntry};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// How many lines a PageUp / PageDown jumps. Mirrors the chat-view
/// page size (10 lines) so the muscle memory carries over.
const APPROVAL_PAGE_SIZE: usize = 10;

/// Handle a Y/N/Esc key for the `!` approval gate. The bang command
/// lives on `state.pending_bang`; on Y we run it, on N/Esc we just
/// clear the gate with a system message. Scroll keys (PageUp/
/// PageDown/Up/Down/Home/End) bubble through to the dialog's
/// args-preview scroll state (shared with the regular approval flow
/// — same dialog, same renderer).
///
/// This is async so that approving a bang command yields the TUI
/// event loop while the shell command runs, instead of freezing the
/// UI with `block_in_place`/`block_on`.
pub async fn handle_bang_approval_key(key: KeyEvent, state: &mut AppState) {
    match key.code {
        KeyCode::PageUp => {
            state.approval_scroll = state.approval_scroll.saturating_sub(APPROVAL_PAGE_SIZE);
            return;
        }
        KeyCode::PageDown => {
            state.approval_scroll = state
                .approval_scroll
                .saturating_add(APPROVAL_PAGE_SIZE)
                .min(state.approval_max_scroll);
            return;
        }
        KeyCode::Up => {
            state.approval_scroll = state.approval_scroll.saturating_sub(1);
            return;
        }
        KeyCode::Down => {
            let next = state.approval_scroll + 1;
            state.approval_scroll = next.min(state.approval_max_scroll);
            return;
        }
        KeyCode::Home => {
            state.approval_scroll = 0;
            return;
        }
        KeyCode::End => {
            state.approval_scroll = state.approval_max_scroll;
            return;
        }
        _ => {}
    }

    // Decision keys.
    let decision = match key.code {
        KeyCode::Char('y') | KeyCode::Char('Y') => Some(true),
        KeyCode::Char('n') | KeyCode::Char('N') => Some(false),
        KeyCode::Esc => Some(false),
        _ => None,
    };
    let Some(approved) = decision else {
        return;
    };

    // Take the bang command out of state — we own it now.
    let Some(bang) = state.pending_bang.take() else {
        return;
    };
    // Reset scroll for the next approval of any kind.
    state.approval_scroll = 0;
    state.approval_max_scroll = 0;

    if approved {
        // The `!` runner is async; await it here so the TUI event
        // loop keeps draining executor events, spinner ticks, and
        // shutdown signals while the shell command runs. The prior
        // `block_in_place` + `Handle::block_on` froze the UI for the
        // duration of the command (up to the 30s bang timeout).
        let cmd = bang.cmd;
        let config = crate::shared::read_shared_config(&state.config).clone();
        let result = crate::tui::commands::handle_bang_command(&cmd, &config).await;
        // Split into summary / full for the collapse UX. Mirrors
        // the split rule in `keys.rs::split_bang_summary` — first
        // two lines are the summary, the whole thing is the full
        // output. Kept inline rather than re-exported to keep the
        // approval module self-contained.
        let mut lines = result.splitn(3, '\n');
        let first = lines.next().unwrap_or("").to_string();
        let second = lines.next().unwrap_or("").to_string();
        let _rest = lines.next();
        let summary = format!("{first}\n{second}");
        state
            .messages
            .push(ConversationEntry::tool(summary, result));
    } else {
        state.messages.push(ConversationEntry::new(
            "system",
            format!("🚫 Cancelled: !{}", bang.cmd),
        ));
    }
}

pub fn handle_approval_key(key: KeyEvent, state: &mut AppState) {
    let approval = match state.pending_approval.take() {
        Some(a) => a,
        None => return,
    };

    // Ctrl+C while a model approval dialog is open: deny the pending
    // operation and signal the app to exit. This mirrors the idle Ctrl+C
    // behavior and prevents the executor from blocking on the oneshot
    // response while the TUI is trying to shut down.
    if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
        deny_pending_approval_and_exit(approval, state);
        return;
    }

    let response = match key.code {
        KeyCode::Char('y') | KeyCode::Char('Y') => Some(ApprovalResponse::Approved),
        KeyCode::Char('n') | KeyCode::Char('N') => Some(ApprovalResponse::Denied),
        KeyCode::Char('a') | KeyCode::Char('A') => Some(ApprovalResponse::AlwaysApprove),
        KeyCode::Char('q') | KeyCode::Char('Q') => Some(ApprovalResponse::Denied),
        KeyCode::Esc => Some(ApprovalResponse::Denied),
        KeyCode::Tab => {
            state.pending_approval = Some(approval);
            state.approval_diff_side_by_side = !state.approval_diff_side_by_side;
            return;
        }
        // Scroll keys — operate on the args preview, NOT the chat.
        // (Approval-mode keys never reach the chat-view scroll handler.)
        KeyCode::PageUp => {
            state.pending_approval = Some(approval);
            state.approval_scroll = state.approval_scroll.saturating_sub(APPROVAL_PAGE_SIZE);
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
            let rule = crate::shared::permission::suggest_rule(&approval.tool_name, &approval.args);
            if let Ok(mut cfg) = state.config.write() {
                push_rule_unique(&mut cfg.permission_rules, rule);
            }
            let cfg = crate::shared::read_shared_config(&state.config);
            if let Err(e) = crate::session::config::save_config(&cfg) {
                tracing::warn!(error = %e, "Failed to save auto-approve rule to config");
            }
        }
        // The user just decided — clear the scroll state so the next
        // approval (if any) starts fresh at the top.
        state.approval_scroll = 0;
        state.approval_max_scroll = 0;
        if let Some(tx) = approval.responder {
            if let Err(e) = tx.send(resp) {
                tracing::warn!(
                    tool = %approval.tool_name,
                    error = ?e,
                    "approval responder dropped before user-decision send"
                );
            }
        }
    }
}

/// Deny the pending approval and request a TUI shutdown.
///
/// Used when the user aborts the dialog with Ctrl+C or when the app is
/// exiting with an unresolved approval in flight. Sends a reasoned denial
/// so the model sees why the operation did not run, then sets the exit
/// flag so the event loop terminates.
fn deny_pending_approval_and_exit(
    approval: crate::tui::app::PendingApproval,
    state: &mut AppState,
) {
    state.approval_scroll = 0;
    state.approval_max_scroll = 0;
    if let Some(tx) = approval.responder {
        if let Err(e) = tx.send(ApprovalResponse::DeniedWithReason(
            "User cancelled the approval dialog (Ctrl+C / exit)".into(),
        )) {
            tracing::warn!(
                tool = %approval.tool_name,
                error = ?e,
                "approval responder dropped during exit-deny"
            );
        }
    }
    state.should_exit = true;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shared::Config;
    use crate::tui::app::PendingApproval;
    use crossterm::event::KeyModifiers;
    use serde_json::json;

    fn make_state_with_approval(args: serde_json::Value) -> AppState {
        let mut s = AppState::new(std::sync::Arc::new(std::sync::RwLock::new(
            Config::default(),
        )));
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

    fn cfg_mut(s: &mut AppState) -> std::sync::RwLockWriteGuard<'_, Config> {
        s.config.write().unwrap_or_else(|e| e.into_inner())
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

    /// Tab toggles side-by-side diff mode without consuming the approval.
    #[test]
    fn test_tab_toggles_side_by_side() {
        let mut s = make_state_with_approval(json!({"command": "ls"}));
        assert!(!s.approval_diff_side_by_side);
        handle_approval_key(key(KeyCode::Tab), &mut s);
        assert!(s.pending_approval.is_some());
        assert!(s.approval_diff_side_by_side);
        handle_approval_key(key(KeyCode::Tab), &mut s);
        assert!(s.pending_approval.is_some());
        assert!(!s.approval_diff_side_by_side);
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
        cfg_mut(&mut s).auto_approve = false;
        cfg_mut(&mut s).permission_rules.clear();
        assert!(!cfg_mut(&mut s).auto_approve);
        assert!(cfg_mut(&mut s).permission_rules.is_empty());

        // Capture config before [A]lways, because save_config would
        // write to the real config path; the test only checks the
        // in-memory state (the path-writing is exercised in integration
        // tests, not unit tests).
        handle_approval_key(key(KeyCode::Char('a')), &mut s);

        // **The new rule should be in permission_rules.**
        assert_eq!(
            cfg_mut(&mut s).permission_rules.len(),
            1,
            "[A]lways should have appended exactly one rule"
        );
        {
            let cfg = cfg_mut(&mut s);
            let r = &cfg.permission_rules[0];
            assert_eq!(r.tool, "bash");
            assert_eq!(r.key, "command");
            assert_eq!(r.pattern, "cargo test --release");
            assert_eq!(r.action, crate::shared::permission::PermissionAction::Allow);
        }

        // **auto_approve must NOT have been flipped.** The user
        // asked for "always this specific command", not "always
        // everything". The new rule is the user's intent.
        assert!(
            !cfg_mut(&mut s).auto_approve,
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
        let mut s = AppState::new(std::sync::Arc::new(std::sync::RwLock::new(
            Config::default(),
        )));
        s.pending_approval = Some(PendingApproval {
            tool_name: "edit_file".into(),
            args: json!({
                "path": "src/main.rs",
                "old_string": "old",
                "new_string": "new"
            }),
            responder: None,
        });
        cfg_mut(&mut s).permission_rules.clear();

        handle_approval_key(key(KeyCode::Char('A')), &mut s);

        assert_eq!(cfg_mut(&mut s).permission_rules.len(), 1);
        {
            let cfg = cfg_mut(&mut s);
            let r = &cfg.permission_rules[0];
            assert_eq!(r.tool, "edit_file");
            assert_eq!(
                r.key, "path",
                "edit_file approvals should build a rule keyed on `path`, not `command`"
            );
            assert_eq!(r.pattern, "src/main.rs");
        }
    }

    /// `[A]` twice on the same call should NOT add duplicate rules.
    /// Regression guard for the `push_rule_unique` dedup.
    #[test]
    fn test_always_approves_dedups_repeated_calls() {
        let mut s = make_state_with_approval(json!({"command": "ls"}));
        cfg_mut(&mut s).permission_rules.clear();

        handle_approval_key(key(KeyCode::Char('a')), &mut s);
        // First push: one rule.
        assert_eq!(cfg_mut(&mut s).permission_rules.len(), 1);

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
            cfg_mut(&mut s).permission_rules.len(),
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
        {
            let mut cfg = cfg_mut(&mut s);
            cfg.permission_rules.clear();
            cfg.permission_rules
                .push(crate::shared::permission::PermissionRule {
                    tool: "bash".into(),
                    key: "command".into(),
                    pattern: "rm -rf build".into(),
                    action: crate::shared::permission::PermissionAction::Deny,
                });
        }

        handle_approval_key(key(KeyCode::Char('a')), &mut s);

        // Still exactly one rule, and it's still Deny.
        assert_eq!(cfg_mut(&mut s).permission_rules.len(), 1);
        {
            let cfg = cfg_mut(&mut s);
            assert_eq!(
                cfg.permission_rules[0].action,
                crate::shared::permission::PermissionAction::Deny,
                "Existing Deny should not be overwritten by [A]lways's Allow on the same pattern"
            );
        }
    }

    // ── Bang approval gate (review.md arch concern #1) ───────────
    //
    // These tests exercise the parallel handler that the ! bash
    // passthrough uses when `bang_requires_approval` is set. Without
    // the gate, the previous code in `keys.rs::Enter` ran the
    // command unconditionally, bypassing PathGuard, deny lists, and
    // the sandbox. The new flow parks the command on AppState and
    // shows the same dialog until the user decides.

    fn make_state_with_bang(cmd: &str) -> AppState {
        let mut s = AppState::new(std::sync::Arc::new(std::sync::RwLock::new(
            Config::default(),
        )));
        s.pending_bang = Some(crate::tui::app::PendingBangCommand { cmd: cmd.into() });
        s
    }

    /// Y on a bang approval runs the command and pushes a tool
    /// entry into the chat. This is the "approve" path — the same
    /// path the user took when the gate was off, but now through
    /// the explicit Y keystroke.
    ///
    /// We use `echo hi` so the test is fast and deterministic.
    /// `handle_bang_approval_key` is async, so this test runs on the
    /// Tokio runtime via `#[tokio::test]`.
    #[tokio::test]
    async fn test_bang_y_runs_command_and_pushes_tool_entry() {
        let mut s = make_state_with_bang("echo hi");
        handle_bang_approval_key(key(KeyCode::Char('y')), &mut s).await;

        // The gate is consumed.
        assert!(s.pending_bang.is_none());
        // A tool entry was pushed.
        assert_eq!(s.messages.len(), 1);
        let entry = &s.messages[0];
        assert_eq!(entry.role, "tool");
        // The full output is stored in the sidecar; the summary
        // is the first ~2 lines.
        assert!(entry.tool_output.is_some());
        let full = entry.tool_output.as_ref().unwrap();
        assert!(full.contains("hi"), "echo output missing: {full}");
    }

    /// N on a bang approval clears the gate WITHOUT running the
    /// command. A system message records the cancellation.
    #[tokio::test]
    async fn test_bang_n_clears_gate_without_running() {
        let mut s = make_state_with_bang("touch /tmp/should-not-exist");
        // The path we're testing for is whether the command ran.
        // We can't easily prove a non-event in a unit test, so the
        // strongest assertion is: gate is cleared, system message
        // is pushed, and the run-method is never called (we'd see
        // a tool entry with a "touch" output, which we don't).
        handle_bang_approval_key(key(KeyCode::Char('n')), &mut s).await;
        assert!(s.pending_bang.is_none());
        assert_eq!(s.messages.len(), 1);
        assert_eq!(s.messages[0].role, "system");
        assert!(s.messages[0].content.contains("Cancelled"));
    }

    /// Esc has the same effect as N — clears the gate, no run.
    #[tokio::test]
    async fn test_bang_esc_clears_gate() {
        let mut s = make_state_with_bang("rm -rf /");
        handle_bang_approval_key(key(KeyCode::Esc), &mut s).await;
        assert!(s.pending_bang.is_none());
        assert_eq!(s.messages.len(), 1);
        assert!(s.messages[0].content.contains("Cancelled"));
    }

    /// Unknown keys leave the gate intact so the user can still
    /// type a decision. (Mirrors the regular approval flow's
    /// "unknown key preserves state" test.)
    #[tokio::test]
    async fn test_bang_unknown_key_preserves_gate() {
        let mut s = make_state_with_bang("echo hi");
        handle_bang_approval_key(key(KeyCode::Char('z')), &mut s).await;
        assert!(s.pending_bang.is_some());
        assert!(s.messages.is_empty());
    }

    /// Scroll keys bubble through to the shared scroll state. The
    /// dialog renders identically for bang and model-approval, so
    /// the scroll plumbing is shared — these are the regression
    /// guards for that sharing.
    #[tokio::test]
    async fn test_bang_scroll_keys_share_state() {
        let mut s = make_state_with_bang("echo hi");
        s.approval_max_scroll = 50;
        handle_bang_approval_key(key(KeyCode::PageDown), &mut s).await;
        assert_eq!(s.approval_scroll, 10);
        handle_bang_approval_key(key(KeyCode::End), &mut s).await;
        assert_eq!(s.approval_scroll, 50);
        handle_bang_approval_key(key(KeyCode::Home), &mut s).await;
        assert_eq!(s.approval_scroll, 0);
    }
}
