#!/usr/bin/env bash
set -euo pipefail
# Fast gate — unit/lib tests only. Use before every commit.
# Target: under 60 seconds on warm cache.
# Uses the ci-fast nextest profile (30s timeout, fail-fast, skip #[ignore]).
# --lib --bins mirrors the PR gate scope (lib + bin unit tests, not
# integration tests). Falls back to `cargo test` if nextest is absent.
export PATH="$HOME/.cargo/bin:$PATH"
if command -v cargo-nextest >/dev/null 2>&1; then
    cargo nextest run --profile ci-fast --workspace --lib --bins --locked
else
    THREADS=$(nproc)
    if [ "$THREADS" -gt 8 ]; then THREADS=8; fi
    cargo test --locked --workspace --lib --bins --no-fail-fast -- --skip integration --test-threads="$THREADS"
fi