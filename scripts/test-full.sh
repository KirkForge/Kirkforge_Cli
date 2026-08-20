#!/usr/bin/env bash
set -euo pipefail
# Full gate — all workspace tests. CI-only or pre-merge.
# Uses the ci-full nextest profile (60s timeout, no-fail-fast, skip #[ignore]).
# Falls back to `cargo test` if nextest is absent.
export PATH="$HOME/.cargo/bin:$PATH"
if command -v cargo-nextest >/dev/null 2>&1; then
    cargo nextest run --profile ci-full --workspace --no-fail-fast --locked
else
    # Cap test threads: the workspace fans out ~38 integration test binaries and
    # each spawns a tokio runtime; uncapped `cargo test --workspace` OOMs the
    # host (this killed a prior session — see lessons.md). test-fast.sh already
    # caps at min(nproc,8); mirror that here.
    THREADS="${KF_TEST_THREADS:-$(nproc)}"
    if [ "$THREADS" -gt 8 ]; then THREADS=8; fi
    cargo test --locked --workspace --no-fail-fast -- --test-threads="$THREADS"
fi