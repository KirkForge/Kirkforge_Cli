#!/usr/bin/env bash
set -euo pipefail
# Full gate — all workspace tests. CI-only or pre-merge.
export PATH="$HOME/.cargo/bin:$PATH"
cargo test --locked --workspace --no-fail-fast
