# Lessons — WO 8.0 session

## What I learned
- Tarpaulin 0.32.0 cannot measure coverage in a per-worktree cold target dir within the
  WO's 5-minute budget. Cold `cargo build` of the kirkforge workspace + all crates takes
  ~8 minutes; tarpaulin's instrumented build (the `--cfg=tarpaulin -Cinstrument-coverage`
  rustc flags in the output) is heavier. On CI this is faster because the action caches
  `$CARGO_HOME` and the target dir between runs. The WO's documented fallback is the
  right call: bump to the minimum and let CI enforce on every push.
- The repo's `kirkforge-cli.testdoctor` crate is `default-members`-excluded; the workspace
  has ~12 member crates that all get built. Even `--lib` builds them all.
- `cargo check --workspace --all-targets` took 8m09s on this machine (cold target dir).
  `cargo clippy --all-targets` took 2m07s incremental. The two `cargo test` runs each
  took ~10–14 minutes. Total gate time on a fresh worktree is ~35 minutes. Worth knowing
  for future WO scheduling.

## What surprised me
- The CI threshold is enforced by an inline Python XML parser, not a tarpaulin-internal
  flag. Easy to bump — no tarpaulin version pinning issue.
- The CHANGELOG.md had 21 lines of "Unreleased / Changed" already in flight from prior
  WOs. Inserted my entry at the top, not the bottom — convention here is newest-first
  within a section. Verified by reading the surrounding entries.

## What I would do differently
- For any future WO that asks "run tarpaulin", pre-warm the worktree's target dir with a
  plain `cargo check --workspace --all-targets` (or even a no-op `cargo build --tests`)
  before invoking tarpaulin. The cold-build cost is the bottleneck, not the coverage
  instrumentation itself.
- If tarpaulin is critical, dispatch it as a separate task against a pre-warmed target
  dir, not as part of the WO itself.
