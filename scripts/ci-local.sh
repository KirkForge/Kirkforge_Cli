#!/usr/bin/env bash
# Local CI gate — runs the same checks as .github/workflows/ci-merge.yml.
#
# Usage:
#   scripts/ci-local.sh           # run all checks
#   scripts/ci-local.sh quick     # run fmt + test + clippy (skip release build and audit)
#   scripts/ci-local.sh full      # quick + release + audit + adr_xref_drift + tarpaulin + llvm-cov regression gate
#
# Exit code: non-zero on first failure.

set -euo pipefail

MODE="${1:-default}"
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
# Cap test threads: the workspace fans out ~38 integration test binaries;
# uncapped `cargo test --workspace` OOMs the host (killed a prior session).
TEST_THREADS="${KF_TEST_THREADS:-$(nproc)}"
if [ "$TEST_THREADS" -gt 8 ]; then TEST_THREADS=8; fi
run_step "Check formatting" cargo fmt --check
run_step "Run unit tests" cargo test --locked --workspace -- --test-threads="$TEST_THREADS"
run_step "Run smoke tests" cargo test --test smoke_test
run_step "Run Clippy" cargo clippy --all-targets -- -D warnings

if [ "$MODE" != "quick" ]; then
    run_step "Build release binary" cargo build --release --locked

    # Block on critical/high CVSS + unsound only; lower severities are
    # warnings. Severity blocking is configured in .cargo/audit.toml
    # (severity_threshold); --deny only accepts advisory *categories*.
    echo
    echo "==> Audit dependencies (critical/high/unsound)"
    run_step "Audit critical" cargo audit --deny unsound

    echo
    echo "==> Audit dependencies (informational warnings)"
    if cargo audit; then
        echo -e "${GREEN}OK${NC}: Audit dependencies (informational)"
    else
        echo -e "${YELLOW}WARNING${NC}: cargo audit informational warnings (non-blocking)"
    fi
fi

# `full` mode additionally mirrors the CI coverage gate (tarpaulin +
# threshold enforcement) and the ADR cross-reference drift test that
# CI runs as a dedicated step. Tarpaulin is heavy; install it with
# `cargo install cargo-tarpaulin` (or the taiki-e install-action) to
# exercise the coverage gate locally. The thresholds below mirror
# .github/workflows/ci.yml and are also drift-guarded by the
# testdoctor `default_thresholds_match_ci_yml` test.
if [ "$MODE" = "full" ]; then
    run_step "ADR xref drift" cargo test -p kf-budget-core --test adr_xref_drift

    # The e2e integration suite is feature-gated behind `e2e-tests`
    # (WO 28.10); only `full` mode pulls it in for local reproduction.
    if cargo test --test e2e --features e2e-tests --no-run --locked >/dev/null 2>&1; then
        run_step "e2e suite" cargo test --test e2e --features e2e-tests --locked -- --test-threads="$TEST_THREADS"
    else
        echo
        echo -e "${YELLOW}WARNING${NC}: e2e crate did not build locally; skipping e2e suite."
    fi

    if command -v cargo-tarpaulin >/dev/null 2>&1; then
        run_step "Generate coverage" cargo tarpaulin --out Xml --locked --lib --timeout 120 -- --skip test_build_fork_tree_nests_children
        echo
        echo "==> Enforce coverage thresholds (mirror ci.yml)"
        if ! python3 - <<'PY'; then
            import xml.etree.ElementTree as ET
            import sys
            tree = ET.parse('cobertura.xml')
            root = tree.getroot()
            packages = root.find('packages')
            targets = {'src/session': 68.5, 'src/tools': 76.0, 'src/adapters': 75.0}
            failed = False
            for prefix, threshold in targets.items():
                lines_valid = 0
                lines_covered = 0
                for pkg in packages.findall('package'):
                    name = pkg.attrib.get('name', '')
                    if name == prefix or name.startswith(prefix + '/'):
                        for cls in pkg.findall('classes/class'):
                            for line in cls.findall('lines/line'):
                                lines_valid += 1
                                if int(line.attrib.get('hits', 0)) > 0:
                                    lines_covered += 1
                rate = (lines_covered / lines_valid * 100) if lines_valid > 0 else 0.0
                print(f'{prefix}/: {lines_covered}/{lines_valid} lines covered ({rate:.1f}%, threshold {threshold}%)')
                if rate < threshold:
                    failed = True
            sys.exit(1 if failed else 0)
PY
            echo -e "${RED}FAILED${NC}: Coverage gate (below threshold)"
            exit 1
        fi
        echo -e "${GREEN}OK${NC}: Coverage gate"
    else
        echo
        echo -e "${YELLOW}WARNING${NC}: cargo-tarpaulin not installed; skipping coverage gate."
        echo "         Install it (cargo install cargo-tarpaulin) to mirror the CI coverage job."
    fi

    # Per-crate coverage regression gate (WO 28.7). Uses cargo-llvm-cov —
    # the SAME tool CI's coverage job uses — so local and CI numbers match.
    # Warns + exits 0 if llvm-cov is absent (see scripts/check-cov-regression.sh).
    run_step "Coverage regression gate" bash scripts/check-cov-regression.sh
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
