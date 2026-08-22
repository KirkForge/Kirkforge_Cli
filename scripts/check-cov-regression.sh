#!/usr/bin/env bash
# Coverage regression gate (WO 28.7).
#
# Runs `cargo llvm-cov --workspace --lcov`, parses per-crate line coverage,
# and compares against docs/coverage-baseline.md with a -1% tolerance per
# crate. Exits 1 if any tracked crate drops below its floor.
#
# Standalone: if cargo-llvm-cov is not installed, warns and exits 0 so the
# gate is non-fatal on hosts without the tool (mirrors the tarpaulin skip
# in scripts/ci-local.sh).
#
# COV_TEST_ARGS: optional extra args forwarded after `--` to the test runner
# (e.g. COV_TEST_ARGS="--skip bash_job" to skip tests that are incompatible
# with the host kernel/sandbox). Defaults to none — CI and ci-local.sh run
# the full suite. Used locally when a host can't run sandbox-spawn tests.
#
# The baseline file (docs/coverage-baseline.md) is the single source of
# truth for the PER-CRATE floor enforced here. A separate per-directory
# tarpaulin gate in scripts/ci-local.sh covers src/session,
# src/tools, src/adapters — a different granularity, not compared here.

set -euo pipefail

cd "$(dirname "$0")/.."

BASELINE="docs/coverage-baseline.md"
TOLERANCE=1.0  # percentage points; absorbs CI runner noise

if ! command -v cargo-llvm-cov >/dev/null 2>&1; then
    echo "WARNING: cargo-llvm-cov not installed; skipping coverage gate."
    echo "         Install it: cargo install cargo-llvm-cov && rustup component add llvm-tools-preview"
    exit 0
fi

if [ ! -f "$BASELINE" ]; then
    echo "FAILED: baseline file $BASELINE not found."
    exit 1
fi

LCOV="$(mktemp -t cov-regression-XXXXXX.lcov)"
trap 'rm -f "$LCOV"' EXIT

echo "==> Generating coverage (cargo llvm-cov --workspace --lcov)"
# COV_TEST_ARGS (optional) forwards extra test-runner flags (e.g. --skip).
# Unquoted on purpose so multiple --skip flags word-split; CI leaves it empty.
# shellcheck disable=SC2086
if [ -n "${COV_TEST_ARGS:-}" ]; then
    cargo llvm-cov --workspace --lcov --output-path "$LCOV" -- $COV_TEST_ARGS
else
    cargo llvm-cov --workspace --lcov --output-path "$LCOV"
fi

echo "==> Comparing against $BASELINE (tolerance -${TOLERANCE}%)"
python3 - "$BASELINE" "$LCOV" "$TOLERANCE" <<'PY'
import re, sys

baseline_path, lcov_path, tolerance = sys.argv[1], sys.argv[2], float(sys.argv[3])

# --- parse baseline table rows: | `crate-name` | NN.N | notes |  (or TBD) ---
baselines = {}  # crate -> (float|None, raw_str)
with open(baseline_path) as f:
    for line in f:
        m = re.match(r"\|\s*`([a-z0-9_-]+)`\s*\|\s*([^|]+?)\s*\|", line)
        if not m:
            continue
        crate = m.group(1)
        raw = m.group(2).strip()
        if raw.upper() == "TBD":
            baselines[crate] = (None, raw)
        else:
            try:
                baselines[crate] = (float(raw), raw)
            except ValueError:
                # Not a number column (header/separator handled by regex above);
                # skip silently.
                continue

if not baselines:
    print("FAILED: no crate rows parsed from", baseline_path)
    sys.exit(1)

# --- map an lcov SF: path to a crate name ---
# crates/<name>/src/...  -> <name>   (relative OR absolute; llvm-cov emits
#                                     absolute paths, lcov merges may be relative)
# <root>/src/...         -> kf-code  (the binary crate lives at repo root)
# anything else (tests/, benches/, build.rs, fuzz/) -> None (skip)
def crate_of(sf_path):
    p = sf_path.replace("\\", "/")
    m = re.search(r"(?:^|/)crates/([A-Za-z0-9_-]+)/src/", p)
    if m:
        return m.group(1)
    # root crate: a src/ segment NOT nested inside any crates/ dir
    if re.search(r"(?:^|/)src/", p) and "/crates/" not in p and not p.startswith("crates/"):
        return "kf-code"
    return None

# --- aggregate lcov DA: lines per crate ---
# lcov records: SF:<path> / DA:<line>,<hits>[,...] / end_of_record
cov = {}  # crate -> [covered, total]
cur_crate = None
for line in open(lcov_path):
    line = line.rstrip("\n")
    if line.startswith("SF:"):
        cur_crate = crate_of(line[3:])
        if cur_crate and cur_crate not in cov:
            cov[cur_crate] = [0, 0]
    elif line.startswith("DA:") and cur_crate and cur_crate in cov:
        # DA:<line>,<hits>
        try:
            hits = int(line[3:].split(",")[1])
        except (IndexError, ValueError):
            continue
        cov[cur_crate][1] += 1
        if hits > 0:
            cov[cur_crate][0] += 1
    elif line == "end_of_record":
        cur_crate = None

regressions = []
missing_baseline = []
not_measured = []
for crate in sorted(baselines):
    floor, raw = baselines[crate]
    measured = cov.get(crate)
    if measured is None or measured[1] == 0:
        not_measured.append(crate)
        print(f"  {crate}: NOT MEASURED (no src/ lines in lcov)")
        continue
    covered, total = measured
    rate = covered / total * 100.0
    if floor is None:
        missing_baseline.append(crate)
        print(f"  {crate}: {covered}/{total} = {rate:.1f}%  (baseline TBD — non-fatal)")
        continue
    threshold = floor - tolerance
    status = "OK" if rate >= threshold else "REGRESSION"
    delta = rate - floor
    print(f"  {crate}: {covered}/{total} = {rate:.1f}%  floor {floor:.1f}% (≥{threshold:.1f}%)  "
          f"Δ{delta:+.1f}  [{status}]")
    if rate < threshold:
        regressions.append((crate, floor, rate))

print()
if not_measured:
    print(f"NOTE: {len(not_measured)} baseline crate(s) produced no coverage; "
          f"check the lcov SF: paths: {', '.join(not_measured)}")
if missing_baseline:
    print(f"NOTE: {len(missing_baseline)} crate(s) still at TBD; "
          f"fill in docs/coverage-baseline.md from this run.")
if regressions:
    for crate, floor, rate in regressions:
        print(f"REGRESSION: crate {crate} floor {floor:.1f}% now {rate:.1f}%")
    sys.exit(1)
print("Coverage gate: OK")
PY
