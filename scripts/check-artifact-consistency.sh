#!/usr/bin/env bash
set -euo pipefail

echo "Artifact consistency"
echo "─────────────────────"
PASS=0
FAIL=0

# 1. release.yml packages only kf-code
if grep -q 'bins=(kf-code)' .github/workflows/release.yml 2>/dev/null; then
  echo "✓ release.yml packages only kf-code"
  PASS=$((PASS+1))
else
  echo "✗ release.yml binary list mismatch"
  FAIL=$((FAIL+1))
fi

# 2. install.sh installs only kf-code (no plugin3/stratum/kfd/kf-code-video)
STALE_INSTALL=$(grep -c 'plugin3\|stratum\|kf-code-video\|\bkfd\b' scripts/install.sh 2>/dev/null || true)
if [ "$STALE_INSTALL" -eq 0 ]; then
  echo "✓ install.sh contains no retired binaries"
  PASS=$((PASS+1))
else
  echo "✗ install.sh has $STALE_INSTALL retired binary refs"
  FAIL=$((FAIL+1))
fi

# 3. benchmark count matches actual files
EXPECTED_BENCHES=$(ls benches/tasks/*.toml 2>/dev/null | wc -l | tr -d ' ')
if grep -q "$EXPECTED_BENCHES coding tasks" README.md 2>/dev/null; then
  echo "✓ benchmark count ($EXPECTED_BENCHES) matches README"
  PASS=$((PASS+1))
else
  echo "✗ benchmark count mismatch (files=$EXPECTED_BENCHES, README=$(grep -o '[0-9]\+ coding tasks' README.md 2>/dev/null || echo 'none'))"
  FAIL=$((FAIL+1))
fi

# 4. plugin paths exist
for dir in plugins/kf-plugin; do
  if [ -d "$dir" ]; then
    echo "✓ $dir exists"
    PASS=$((PASS+1))
  else
    echo "✗ $dir missing"
    FAIL=$((FAIL+1))
  fi
done

# 5. no retired binaries in active scripts
RETIREDS=$(grep -rn 'plugin3\|kf-code-video\|kf-code-plugin\b' scripts/ .github/workflows/*.yml 2>/dev/null | grep -v '#' | grep -v 'check-artifact-consistency' | wc -l | tr -d ' ' || true)
if [ "$RETIREDS" -eq 0 ]; then
  echo "✓ no retired binary refs in scripts/CI"
  PASS=$((PASS+1))
else
  echo "✗ $RETIREDS retired refs in scripts/CI"
  FAIL=$((FAIL+1))
fi

# 6. release docs match reality
if grep -q 'kf-code' docs/RELEASE.md 2>/dev/null && ! grep -q 'kirkforge\|plugin3\|stratum\|kf-code-video\|\bkfd\b' docs/RELEASE.md 2>/dev/null; then
  echo "✓ docs/RELEASE.md matches current release"
  PASS=$((PASS+1))
else
  echo "✗ docs/RELEASE.md has stale binary refs"
  FAIL=$((FAIL+1))
fi

# 7. Cargo.toml description is provider-agnostic (no "Ollama" in description)
if ! grep -A1 '^description' Cargo.toml | head -2 | grep -qi 'ollama'; then
  echo "✓ Cargo.toml description is provider-agnostic"
  PASS=$((PASS+1))
else
  echo "✗ Cargo.toml still references Ollama"
  FAIL=$((FAIL+1))
fi

# 8. installer platform mappings match release matrix
RELEASE_TARGETS=$(grep -oP 'target:\s*\K[^\s]+' .github/workflows/release.yml | sort)
INSTALLER_TARGETS=$(grep -oP 'target="[^"]*"' scripts/install.sh | sed 's/target="//;s/"//' | sort)
MISSING=0
for t in $INSTALLER_TARGETS; do
  if ! echo "$RELEASE_TARGETS" | grep -qx "$t"; then
    echo "✗ installer maps to '$t' but no release artifact exists"
    FAIL=$((FAIL+1))
    MISSING=1
  fi
done
if [ "$MISSING" -eq 0 ]; then
  echo "✓ all installer targets exist in release matrix"
  PASS=$((PASS+1))
fi

# 9. bench task count matches docs/TECHNICAL.md benchmark table rows
# (WO 28.11 R1). The README count check (#3) already covers README.md;
# this guards the per-task row table in TECHNICAL.md against silent
# add/rename drift.
TECHNICAL_ROWS=$(grep -cE '^\| `[^`]+\.toml` \|' docs/TECHNICAL.md 2>/dev/null || echo 0)
if [ "$TECHNICAL_ROWS" -eq "$EXPECTED_BENCHES" ]; then
  echo "✓ TECHNICAL.md bench row count ($TECHNICAL_ROWS) matches directory"
  PASS=$((PASS+1))
else
  echo "✗ TECHNICAL.md bench row count mismatch (rows=$TECHNICAL_ROWS, files=$EXPECTED_BENCHES)"
  FAIL=$((FAIL+1))
fi

# 10. dead/retired identifier firewall extends to src/ + crates/ (WO 28.12).
# Existing checks #2/#5/#6 cover scripts/, .github/, install.sh, docs/RELEASE.md.
# This check covers ACTIVE SOURCE. To avoid false positives on historical
# prose (comments, doc-tests, string literals, test fn names that mention a
# retired name), the grep is restricted to identifier positions that are
# unambiguous live refs: `use`/`mod`/`extern crate` declarations, and
# Cargo.toml dep entries under crates/.
# `stratum` is intentionally NOT in the dead set — it is a live feature.
DEAD_IDENTS='plugin3|kfd|kf_code_video|kf_code_draw|kf_code_plugin'
DEAD_CARGO_NAMES='plugin3|kfd|kf-code-video|kf-code-draw|kf-code-plugin'
LIVE_HITS=$(grep -rEn "^[[:space:]]*(use|mod|extern[[:space:]]+crate)[[:space:]]+(${DEAD_IDENTS})\b" src/ crates/ 2>/dev/null | wc -l | tr -d ' ' || true)
CARGO_HITS=$(find crates -name Cargo.toml -exec grep -EHn "^(${DEAD_CARGO_NAMES})[[:space:]]*=" {} \; 2>/dev/null | wc -l | tr -d ' ' || true)
TOTAL_DEAD=$((LIVE_HITS + CARGO_HITS))
if [ "$TOTAL_DEAD" -eq 0 ]; then
  echo "✓ no live retired-identifier refs in src/ or crates/"
  PASS=$((PASS+1))
else
  echo "✗ $TOTAL_DEAD live retired-identifier refs in src/ or crates/ ($LIVE_HITS use/mod, $CARGO_HITS Cargo deps):"
  grep -rEn "^[[:space:]]*(use|mod|extern[[:space:]]+crate)[[:space:]]+(${DEAD_IDENTS})\b" src/ crates/ 2>/dev/null || true
  find crates -name Cargo.toml -exec grep -EHn "^(${DEAD_CARGO_NAMES})[[:space:]]*=" {} \; 2>/dev/null || true
  FAIL=$((FAIL+1))
fi

echo ""
echo "$PASS passed, $FAIL failed"
if [ "$FAIL" -gt 0 ]; then
  exit 1
fi
