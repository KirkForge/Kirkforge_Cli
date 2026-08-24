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
# nextest manages its own thread pool, but the cargo-test fallback below
# still needs the cap (WO 40.3: test steps now use nextest profiles).
TEST_THREADS="${KF_TEST_THREADS:-$(nproc)}"
if [ "$TEST_THREADS" -gt 8 ]; then TEST_THREADS=8; fi
run_step "Check formatting" cargo fmt --check

# Test steps use nextest profiles (WO 40.3): ci-fast for the quick gate,
# ci-full for the full gate. Fall back to `cargo test` if nextest is absent
# so ci-local.sh still works on a bare toolchain.
NEXTEST_AVAILABLE=0
if command -v cargo-nextest >/dev/null 2>&1; then
    NEXTEST_AVAILABLE=1
fi

if [ "$NEXTEST_AVAILABLE" = "1" ]; then
    run_step "Run unit tests (ci-fast)" cargo nextest run --profile ci-fast --workspace --lib --bins --locked
    run_step "Run smoke tests" cargo nextest run --profile ci-fast --locked --test smoke_test
else
    run_step "Run unit tests" cargo test --locked --workspace -- --test-threads="$TEST_THREADS"
    run_step "Run smoke tests" cargo test --test smoke_test
fi
run_step "Run Clippy" cargo clippy --all-targets -- -D warnings

# Windows cross-compile clippy (WO 40.2). Linux clippy is blind to cfg(unix)
# gaps — unix-only imports/consts/test helpers used ungated compile fine here
# but break the Windows build. This step catches every such gap before push.
# The 25+ fix(windows) commits in Aug 2026 were all caused by skipping this.
# Requires the x86_64-pc-windows-gnu rustup target + mingw-w64 (x86_64-w64-mingw32-gcc).
run_step "Run Clippy (Windows cross-compile)" cargo clippy --target x86_64-pc-windows-gnu --workspace --all-targets -- -D warnings

# WO 43.16 no-throw dispatch gate: reject new non-test `unwrap`/`expect`/
# `panic!` in `src/session/executor/dispatch.rs`. The dispatch hub must
# return Result/Failure outcomes, not panic. Lines inside the inline
# `#[cfg(test)] mod tests` block (from its opening to EOF) are exempt.
run_step "Dispatch no-throw grep gate" bash -c '
    awk "
        /^#\[cfg\(test\)\]/ { in_test=1 }
        in_test { next }
        /\.unwrap\(|\.expect\(|panic!\(/ { print FILENAME\":\"NR\":\"\$0; bad=1 }
        END { exit bad+0 }
    " src/session/executor/dispatch.rs
'

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
# .github/workflows/ci.yml (the deleted pre-split ci.yml; the gate now
# lives inline here).
if [ "$MODE" = "full" ]; then
    run_step "ADR xref drift" cargo test -p kf-budget-core --test adr_xref_drift

    # The e2e integration suite is feature-gated behind `e2e-tests`
    # (WO 28.10); only `full` mode pulls it in for local reproduction.
    # Uses the e2e nextest profile (300s slow-timeout) when available.
    # The e2e test binary MUST compile — a build break is a real failure,
    # not environment noise (WO 44.55). Probe with --no-run first; on
    # failure re-run with visible output and record the failure. The
    # re-run uses `|| true` so set -e doesn't exit before failures+=
    # records the step — the gate decision was already made by the probe;
    # the script still exits non-zero via the failures array at the end.
    if [ "$NEXTEST_AVAILABLE" = "1" ]; then
        if cargo test --test e2e --features e2e-tests --no-run --locked >/dev/null 2>&1; then
            run_step "e2e suite" cargo nextest run --profile e2e --features e2e-tests --no-fail-fast --locked
        else
            echo
            echo -e "${RED}FAILED${NC}: e2e crate did not build locally (re-running with output):"
            cargo test --test e2e --features e2e-tests --no-run --locked || true
            failures+=("e2e suite (build)")
        fi
    else
        if cargo test --test e2e --features e2e-tests --no-run --locked >/dev/null 2>&1; then
            run_step "e2e suite" cargo test --test e2e --features e2e-tests --locked -- --test-threads="$TEST_THREADS"
        else
            echo
            echo -e "${RED}FAILED${NC}: e2e crate did not build locally (re-running with output):"
            cargo test --test e2e --features e2e-tests --no-run --locked || true
            failures+=("e2e suite (build)")
        fi
    fi

    if command -v cargo-tarpaulin >/dev/null 2>&1; then
        run_step "Generate coverage" cargo tarpaulin --out Xml --locked --lib --timeout 120 -- --skip test_build_fork_tree_nests_children
        echo
        echo "==> Enforce coverage thresholds (local-only; ci-nightly uploads the report, ADR-074)"
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
