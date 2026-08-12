//! Per-session Stratum compression mode — process-global shared state.
//!
//! WO 28.2: extracted from `session::stratum` so the budget→stratum
//! production dependency edge (budget's auto-escalation path mutating the
//! session mode) no longer points into the stratum module. Both subsystems
//! read/mutate this global; neither owns it now. Stratum re-exports the
//! accessors for back-compat.

use kf_compress_core::mode::Mode;
use std::sync::{Mutex, OnceLock};

/// Per-session Stratum mode. Distinct from the config-derived
/// `active_mode()`: `SESSION_MODE` is the *resolved* mode for the current
/// session and can be mutated by the budget's auto-escalation path. The
/// config-derived mode is read-only; the session mode wins when both are
/// consulted.
///
/// ceiling: process-global OnceLock. Intentional for env-driven config: a
/// single CLI process has one active session, and auto-escalation must
/// outlive `StratumSessionStartHook`. Multi-session support would require
/// scoping into SessionStores.
static SESSION_MODE: OnceLock<Mutex<Mode>> = OnceLock::new();

fn session_mode() -> &'static Mutex<Mode> {
    SESSION_MODE.get_or_init(|| Mutex::new(Mode::Full))
}

/// Read the current per-session Stratum mode.
pub fn current_session_mode() -> Mode {
    *session_mode().lock().unwrap_or_else(|e| e.into_inner())
}

/// Set the per-session Stratum mode. Intended for the budget's
/// auto-escalation path. The new mode takes effect for the next
/// compression call.
pub fn set_session_mode(mode: Mode) {
    *session_mode().lock().unwrap_or_else(|e| e.into_inner()) = mode;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_mode_round_trip() {
        set_session_mode(Mode::Lite);
        assert_eq!(current_session_mode(), Mode::Lite);
        set_session_mode(Mode::Full);
        assert_eq!(current_session_mode(), Mode::Full);
        set_session_mode(Mode::Ultra);
        assert_eq!(current_session_mode(), Mode::Ultra);
        set_session_mode(Mode::Full);
    }
}
