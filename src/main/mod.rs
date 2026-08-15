// Stabilization lint: holding a std::sync lock guard across an .await point
// is a classic async Rust foot-gun that can deadlock the executor. The
// codebase currently passes this check; deny it to keep it that way.
#![deny(clippy::await_holding_lock)]
// Stabilization lint: unwrap() in production code can crash the TUI. Tests
// are allowed to unwrap for brevity; production code must use proper error
// handling or explicit expect() with a justification.
#![cfg_attr(not(test), deny(clippy::unwrap_used))]

// WO 15.12: the binary root was a 2,508-line monolith mixing CLI dispatch,
// session setup, line-mode loop, and five independent command-handler
// groups. It is now a thin router that re-exports `main` from
// `cli_dispatch` and declares the feature-aligned sub-modules. Every
// function moved verbatim — pure refactor, no behaviour change.
mod chrome_launcher;
mod cli_dispatch;
mod error;
mod handle_bench;
mod handle_doctor;
mod handle_plugin;
mod handle_replay;
mod handle_sessions;
mod handle_update;
mod line_mode;
mod run_session;
mod turn_events;

// Crate-root bindings so submodules can keep using the original
// `crate::tools::computer_use` / `crate::shared::…` paths verbatim
// (notably `chrome_launcher.rs`, which is unchanged by this split).
// Referenced via `crate::tools` / `crate::shared` from those submodules.
use kf_code::{shared, tools};

pub use cli_dispatch::main;
