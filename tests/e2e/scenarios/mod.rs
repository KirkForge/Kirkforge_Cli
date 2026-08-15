//! E2E regression scenarios.
//!
//! Each scenario pins a user-visible behavior, tied to a past bug where
//! one exists.  The harness spawns the real `kf-code` binary against a
//! mock provider in an isolated HOME.  Scenarios that need TUI
//! interaction require `tmux` and are gated behind `#[ignore]` plus a
//! runtime tmux check.
//!
//! WO 33.14 phase 3 DEFERRED: collapsing binary-spawn E2E assertions
//! into in-process integration tests. `src/session/executor/tests/
//! wiremock_integration.rs` is the canonical in-process layer (adapter
//! + executor turn against WireMock). The scenarios here exercise
//! binary wiring (argv, env, stdin, TUI) that in-process tests
//! structurally cannot cover — that is the point of keeping 2-4 true
//! binary E2Es (the task's target). The TUI scenarios (tui_chat,
//! tui_approval) cannot move in-process by construction. ponytail:
//! ceiling — the current split (in-process wiremock + #[ignore]d
//! binary E2Es) already matches the "leave only 2-4 true binary E2Es"
//! intent; the non-TUI scenarios (adapter_routing, retry_5xx,
//! mock_error_response, plain_chat, tool_approval) are candidates to
//! fold into wiremock_integration if their assertions are moveable.
//! Upgrade path: audit each non-TUI scenario, move moveable
//! assertions into wiremock_integration.rs, delete the binary-spawn
//! version. Tracked in state.md pending.

pub mod adapter_routing;
pub mod auto_approve_skip;
pub mod config_isolation;
pub mod daemon_ping;
pub mod mock_error_response;
pub mod plain_chat;
pub mod retry_5xx;
pub mod tool_approval;
pub mod tui_approval;
pub mod tui_chat;
