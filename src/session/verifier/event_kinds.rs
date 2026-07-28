use crate::session::event_bus::EventKind;

/// Determine which event kinds a verifier should subscribe to.
/// Convenience helper for creating event-bus subscriptions.
pub fn verifier_event_kinds(verifier_name: &str) -> Vec<EventKind> {
    match verifier_name {
        "lint" => vec![EventKind::Edit, EventKind::FileWrite],
        "type-check" => vec![EventKind::Edit, EventKind::FileWrite],
        "git" => vec![EventKind::GitOperation, EventKind::BashExec],
        "security" => vec![EventKind::FileWrite, EventKind::Edit, EventKind::BashExec],
        _ => vec![],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lint_subscribes_to_edit_and_file_write() {
        let kinds = verifier_event_kinds("lint");
        assert_eq!(kinds.len(), 2);
        assert!(kinds.contains(&EventKind::Edit));
        assert!(kinds.contains(&EventKind::FileWrite));
    }

    #[test]
    fn type_check_matches_lint_subscription_set() {
        let kinds = verifier_event_kinds("type-check");
        assert_eq!(kinds, verifier_event_kinds("lint"));
    }

    #[test]
    fn git_subscribes_to_gitop_and_bash() {
        let kinds = verifier_event_kinds("git");
        assert_eq!(kinds.len(), 2);
        assert!(kinds.contains(&EventKind::GitOperation));
        assert!(kinds.contains(&EventKind::BashExec));
    }

    #[test]
    fn security_subscribes_to_three_kinds() {
        let kinds = verifier_event_kinds("security");
        assert_eq!(kinds.len(), 3);
        assert!(kinds.contains(&EventKind::FileWrite));
        assert!(kinds.contains(&EventKind::Edit));
        assert!(kinds.contains(&EventKind::BashExec));
    }

    #[test]
    fn unknown_verifier_returns_empty_kinds() {
        assert!(verifier_event_kinds("rustfmt").is_empty());
        assert!(verifier_event_kinds("").is_empty());
        assert!(verifier_event_kinds("build").is_empty());
    }

    #[test]
    fn names_are_case_sensitive() {
        assert!(
            verifier_event_kinds("LINT").is_empty(),
            "lookup is case-sensitive — capital L should not match"
        );
        assert!(
            verifier_event_kinds("Lint").is_empty(),
            "lookup is case-sensitive — mixed case should not match"
        );
    }
}
