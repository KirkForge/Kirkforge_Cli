# WO 14.3 lessons

## What I learned about this codebase
- The main binary depends non-optionally on `kirkforge-plugin` and
  `kirkforge-plugin-host`. Both re-export `thiserror`-derived error enums
  at their crate roots: `kirkforge_plugin::ManifestError` (Parse/Io/
  UnsupportedApiVersion) and `kirkforge_plugin_host::{ToolError,HookError,
  VerifierError}` (NotFound/Io). These impl `std::error::Error`, so they
  survive `anyhow::Error::from` and are reachable via `downcast_ref`.
- `src/shared/mod.rs::ToolError` and `src/session/bash_runner/mod.rs::
  ShellError` do NOT impl `std::error::Error` (Clone/Debug only). They
  cannot be `downcast_ref`'d inside an `anyhow::Error`. Follow-up if we
  want typed classification for tool/sandbox failures: add
  `impl std::error::Error` (or thiserror) to those enums first.
- `McpError` in `src/session/mcp_client/error.rs` is `pub(super)` — not
  reachable from `src/main/mod.rs`. Not a downcast candidate without a
  visibility change (out of WO scope).
- The model/provider adapters (`src/adapters/*`) return bare `anyhow`
  with no typed connection error. So `ModelUnreachable` has no typed
  source to downcast — it stays on string matching. Follow-up: a typed
  `ModelConnectionError` in the adapter layer would let WO 14.3 finish.
- Test ergonomics: the `src/main/mod.rs` tests live in the `kirkforge`
  BIN target (path `src/main/mod.rs`), NOT the lib target. `cargo test
  -p kirkforge --lib` runs `src/lib.rs` unit tests; the WO's gate command
  `cargo test -p kirkforge --lib main -- --exact kirkforge_error` is a
  no-op (matches 0 tests) because (a) `--lib` selects the lib, not the
  bin, and (b) there's no `kirkforge_error` test module. The substantive
  run is `cargo test -p kirkforge --bin kirkforge`. Flagging this as a
  WO gate mismatch; the WO author likely intended the bin target. I
  ran the bin tests (6/6 pass) as the real verification.
- Build times: `cargo test -p kirkforge --lib --no-run` took ~15 min,
  `--bin kirkforge --no-run` another ~8 min, on this machine. The full
  `cargo test --workspace` + clippy gate is a 25-40 min budget. Run
  clippy in background with nohup + poll; don't block the shell.

## What I tried that didn't work
- Initially wrote `anyhow!("...")` in tests without importing the macro.
  The `use super::*;` brings the `anyhow` *crate* into scope but not the
  `anyhow!` *macro*. Fix: `use anyhow::anyhow;` in the tests module.
- Considered downcasting `shared::ToolError` for the AccessDenied
  branch, but it doesn't impl `std::error::Error` (can't downcast).
  Picked `kirkforge_plugin_host::ToolError::NotFound` instead — it's a
  thiserror type and maps to AccessDenied (command not found at the
  sandboxed plugin root = path-availability outcome after root-gating).

## What I'd do differently next time
- For a WO whose gate command references a specific test filter
  (`--exact kirkforge_error`), check whether that module path exists
  BEFORE writing tests — if not, name the tests to match the intended
  filter or note the mismatch up front. I caught it at gate time.
- If downcast coverage matters, lobby for typed errors at the adapter
  layer first; the classifier is only as typed as its sources.