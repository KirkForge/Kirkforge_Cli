#!/usr/bin/env bash
# Local CI gate — runs the same checks as .github/workflows/ci.yml.
#
# Usage:
#   scripts/ci-local.sh           # run all checks
#   scripts/ci-local.sh quick     # run fmt + test + clippy (skip release build and audit)
#
# Exit code: non-zero on first failure.

set -euo pipefail

QUICK="${1:-}"
cd "$(dirname "$0")/.."

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

failures=()

run_step() {
    local name="$1"
    shift
    echo
    echo "==> $name"
    if "$@"; then
        echo -e "${GREEN}OK${NC}: $name"
    else
        echo -e "${RED}FAILED${NC}: $name"
        failures+=("$name")
        return 1
    fi
}

# Core checks always run.
run_step "Check formatting" cargo fmt --check
run_step "Run unit tests" cargo test --locked --workspace
run_step "Run smoke tests" cargo test --test smoke_test
run_step "Run Clippy" cargo clippy --all-targets -- -D warnings

# Optional Node SDK pass when the vendored package is present.
if [ -d "npm/kirkforge-plugin" ] && [ -f "npm/kirkforge-plugin/package.json" ]; then
    if [ "$QUICK" = "quick" ]; then
        run_step "Build Node SDK" bash -c 'cd npm/kirkforge-plugin && npm run build'
    else
        run_step "Run Node SDK tests" bash -c 'cd npm/kirkforge-plugin && npm test'
    fi
fi

if [ "$QUICK" != "quick" ]; then
    run_step "Build release binary" cargo build --release --locked

    # Audit is advisory because network failures / new advisories can break
    # an otherwise-good build.
    echo
    echo "==> Audit dependencies (advisory)"
    if cargo audit; then
        echo -e "${GREEN}OK${NC}: Audit dependencies"
    else
        echo -e "${YELLOW}WARNING${NC}: cargo audit failed (advisory only)"
    fi
fi

echo
if [ ${#failures[@]} -eq 0 ]; then
    echo -e "${GREEN}All local CI checks passed.${NC}"
    exit 0
else
    echo -e "${RED}Local CI failed:${NC}"
    for f in "${failures[@]}"; do
        echo "  - $f"
    done
    exit 1
fi
