#!/usr/bin/env bash
set -euo pipefail
# Full gate — all workspace tests. CI-only or pre-merge.
export PATH="$HOME/.cargo/bin:$PATH"
# Cap test threads: the workspace fans out ~38 integration test binaries and
# each spawns a tokio runtime; uncapped `cargo test --workspace` OOMs the
# host (this killed a prior session — see lessons.md). test-fast.sh already
# caps at min(nproc,8); mirror that here.
THREADS="${KF_TEST_THREADS:-$(nproc)}"
if [ "$THREADS" -gt 8 ]; then THREADS=8; fi
cargo test --locked --workspace --no-fail-fast -- --test-threads="$THREADS"
