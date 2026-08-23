#!/usr/bin/env bash
set -euo pipefail
# Optional local runner for cargo-mutants on the security-critical modules.
# NOT wired into scripts/ci-local.sh or any pre-commit/pre-merge gate —
# full-mutation runs are slow; nightly-only by design (ADR-074 tiering).
# Mirrors the ci-nightly.yml `mutants` job. Requires cargo-mutants installed
# (`cargo install cargo-mutants` or the install-action in CI).
export PATH="$HOME/.cargo/bin:$PATH"
if ! command -v cargo-mutants >/dev/null 2>&1; then
    echo "cargo-mutants not found. Install: cargo install cargo-mutants" >&2
    exit 1
fi
# Default: run all four target modules. Pass file paths as args to override.
if [ "$#" -gt 0 ]; then
    TARGETS=("$@")
else
    TARGETS=(
        crates/kf-routing/src/path_safety.rs
        src/session/bash_runner/mod.rs
        src/shared/audit.rs
        src/session/executor/sandbox.rs
    )
fi
for f in "${TARGETS[@]}"; do
    echo "=== mutants: $f ==="
    cargo mutants --file "$f" -j 2
done