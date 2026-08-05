//! E2E regression scenarios.
//!
//! Each scenario pins a user-visible behavior, tied to a past bug where
//! one exists.  The harness spawns the real `kf-code` binary against a
//! mock provider in an isolated HOME.  Scenarios that need TUI
//! interaction require `tmux` and are gated behind `#[ignore]` plus a
//! runtime tmux check.

pub mod adapter_routing;
pub mod auto_approve_skip;
pub mod config_isolation;
pub mod daemon_ping;
pub mod mock_error_response;
pub mod plain_chat;
pub mod retry_5xx;
pub mod tool_approval;
pub mod tui_chat;
