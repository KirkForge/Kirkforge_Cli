# lessons.md — WO 43.24 session

- Cargo config dispatches `cargo test` to cargo-nextest (`-E` filters seen in pgrep).
- libtest accepts multiple positional filters after `--` (all my filters matched in one run).
- `MakeWriter` closure HRTB inference fails in this toolchain (1.88): impl
  `MakeWriter` on the buffer type instead of passing a closure to `with_writer`.
- A tool-timeout kill of a command that launched a background build can kill the
  build AND corrupt a build-script OUT_DIR artifact (headless_chrome protocol.rs
  "unclosed delimiter"). Fix: rm -rf that build dir; always launch long builds
  with `setsid nohup ... < /dev/null &` in a fast-exiting command.
- `$?` after `cmd | tail` reports tail's exit — use ${PIPESTATUS[0]} in gates.
- Load-avg 11 (3 concurrent worktree gates) flaked
  attached_cancel_token_kills_inflight_bash_promptly; passes on quiet machine.
- `run_with_context` fires `post_hooks` (not `in_process_hooks` — those fire in
  `run_decision_inner`); naming in tests is misleading.
- scope creep: none. access/mod.rs :879 test strengthened alongside :889/:899
  (same region, needed to validate the capture harness — disclosed in WO Done).
