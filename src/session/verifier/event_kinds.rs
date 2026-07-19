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
