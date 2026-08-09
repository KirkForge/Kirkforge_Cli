#!/usr/bin/env bash
set -euo pipefail
# Fast gate — unit/lib tests only. Use before every commit.
# Target: under 60 seconds on warm cache.
THREADS=$(nproc)
if [ "$THREADS" -gt 8 ]; then THREADS=8; fi
export PATH="$HOME/.cargo/bin:$PATH"
cargo test --locked --workspace --lib --bins --no-fail-fast -- --skip integration --test-threads="$THREADS"
