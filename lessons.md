# Lessons — WO 28.8 + 28.14 session (branch `wo28b`)

## What worked

- **`start_paused = true` is gated behind tokio's `test-util` feature**, which is NOT in `tokio = { features = ["full"] }` (full explicitly excludes test-util). Enabling it workspace-wide would risk binary bloat. Pragmatic alternative: a test-only env-var override (`KF_TEST_DAEMON_READ_TIMEOUT_MS`) read by a tiny `read_timeout()` helper in client.rs. Production callers never set it, so behaviour is unchanged; tests get to pin the timeout firing in ~100ms instead of 30s. Pattern worth reusing for any hard-coded timeout that needs a real pin without bloating the suite.
- **`Result<T, E>::unwrap_err()` requires `T: Debug`.** DaemonClient doesn't derive Debug and shouldn't (it holds a stream). Use `match result { Err(e) => format!("{e}"), Ok(_) => unreachable!() }` instead — same outcome, no derive.
- **Auth-token mismatch test via file-content swap.** The server caches `expected_token` at startup in `DaemonState::new()`; the client re-reads the file on every call via `read_auth_token()`. So swapping the file contents mid-connection makes the next client request carry a wrong token while the server still expects the old one. Cleaner than threading a second env var.
- **Stub-listener pattern for handshake-error tests (R4, R5).** `UnixListener::bind` + `tokio::spawn` a task that drains the request line via `read_line_limited`, writes the synthetic response, then `std::future::pending::<()>().await` to keep the stream open until the client errors. The task is aborted at the end of the test.
- **OnceLock for per-var one-shot warnings.** `set(())` returns Ok only on the first caller across all threads — race-safe without a Mutex. Three statics (one per renamed var) is the minimum; a single static would under-warn (one var silences the others).

## What didn't work / gotchas

- **R1 "drops if 5s timeout removed" done-condition is unenforceable for Unix-domain sockets.** `UnixStream::connect` to a nonexistent path returns ENOENT immediately; there's no slow-connect path. Documented in the ponytail comment on R1 — the test still pins the connect-Err contract, just not the timeout firing specifically.
- **`adr_xref_drift::status_counts_match_index_table_summary` is pre-existing red on `dev`** (Accepted: 76 vs 75 + Accepted (WO 27.1 added landlock...): 1). Verify by `git stash && cargo test -p kf-budget-core --test adr_xref_drift && git stash pop`. The fix is to either change the index table for the landlock ADR to "Accepted (WO 27.1 added landlock — see amendment below)" OR drop the parenthetical from the file header — out of scope for either WO.
- **`cargo check -p kf-code --lib --tests` is slow (~2-3 min cold).** Same for clippy (`4m37s`). Budget for full gate runs.

## What I'd do differently

- For WO 28.8 R1 specifically: if a future WO wants the timeout firing actually pinned, the cleanest path is to make `connect_at` injectable with a `connect_timeout()` helper like the one I added for read — already done in this commit, so R1 could be retrofitted by setting `KF_TEST_DAEMON_CONNECT_TIMEOUT_MS=50` and pointing at a path that blocks. But Unix-domain connect doesn't block in practice; the env-var hook is dead weight for R1. Left it in for symmetry.

## Codebase patterns confirmed

- `test_data_dir_lock()` in `src/session/mod.rs:54` is the cross-test mutex for any test that mutates `KF_CODE_DATA_DIR`. Already used by `server.rs::client_server_round_trip` — reused in the R3 auth test and the R6 happy-path.
- The `kf-budget-core` EnvGuard (test_support.rs) uses a process-global reentrant mutex; PLUGIN3_*_DIR writes go through the same lock as KF_BUDGET_*_DIR, so the new shim tests are race-safe with the existing concurrency tests without additional serialization.
- ADR drift is a two-source-of-truth system (file headers + index table) — but only when the Status line itself changes. Adding prose inside an ADR does not trigger the drift test.
