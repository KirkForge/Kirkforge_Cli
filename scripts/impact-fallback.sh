#!/usr/bin/env bash
# Lightweight impact-analysis fallback used when no code-intel index is
# available (e.g. fresh clone, or git clean).
#
# This is NOT a call-graph analysis. It is a regression smoke-net:
# it lists changed Rust files, runs a full check/test/clippy matrix,
# and highlights tests that likely exercise the changed modules.
#
# Usage:
#   scripts/impact-fallback.sh        # run against working-tree changes
#   scripts/impact-fallback.sh HEAD~1 # run against a specific ref

set -euo pipefail

BASE_REF="${1:-HEAD}"
FAIL_FAST="${FAIL_FAST:-1}"
cd "$(dirname "$0")/.."

YELLOW='\033[1;33m'
GREEN='\033[0;32m'
RED='\033[0;31m'
NC='\033[0m'

if [ "$BASE_REF" = "HEAD" ]; then
    CHANGED_FILES=$(git diff --name-only --diff-filter=ACMR -- '*.rs' || true)
else
    CHANGED_FILES=$(git diff --name-only --diff-filter=ACMR "$BASE_REF" -- '*.rs' || true)
fi

if [ -z "$CHANGED_FILES" ]; then
    echo -e "${GREEN}No Rust files changed${NC}; nothing to impact-check."
    exit 0
fi

echo "Changed Rust files:"
echo "$CHANGED_FILES" | sed 's/^/  - /'
echo

# Build a regex of changed module basenames so we can suggest relevant tests.
MODULES=$(echo "$CHANGED_FILES" | xargs -n1 basename | sed 's/\.rs$//' | sort -u | tr '\n' '|' | sed 's/|$//')
if [ -n "$MODULES" ]; then
    echo "Suggested test/module keywords: $MODULES"
    echo
fi

run_step() {
    local name="$1"
    shift
    echo "==> $name"
    if "$@"; then
        echo -e "${GREEN}OK${NC}: $name"
    else
        echo -e "${RED}FAILED${NC}: $name"
        if [ "$FAIL_FAST" = "1" ]; then
            exit 1
        fi
    fi
}

run_step "Cargo check" cargo check --all-targets --locked
run_step "Unit tests" cargo test --locked --workspace
run_step "Clippy" cargo clippy --all-targets -- -D warnings

echo
echo -e "${GREEN}Impact fallback passed.${NC}"
echo "Reminder: this checks for regressions; it does not replace a full"
echo "call-graph impact analysis. Review the diff manually before"
echo "committing large refactors."
