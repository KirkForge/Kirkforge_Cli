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

echo ""
echo "$PASS passed, $FAIL failed"
if [ "$FAIL" -gt 0 ]; then
  exit 1
fi
