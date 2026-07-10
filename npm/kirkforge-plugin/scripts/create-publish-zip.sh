#!/usr/bin/env bash
# create-publish-zip.sh — Produce a clean, buildable source artifact
#
# Usage:
#   ./scripts/create-publish-zip.sh [output.zip]
#
# 1. Creates zip from git-tracked source (respects .gitignore)
# 2. Extracts to temp dir, runs: npm ci → npm run build → npm test
# 3. Only succeeds if all three pass in the extracted zip
#
# NOTE: If using the SQLite backend (better-sqlite3), native build tools
# (python3, make, g++) and network access are required for npm ci.
# Default is FileAdapter (zero native deps).

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$REPO_ROOT"

OUTPUT="${1:-$REPO_ROOT/kirkforge-plugin-publish.zip}"
TMPDIR="$(mktemp -d)"
trap 'rm -rf "$TMPDIR"' EXIT

echo "=== Creating publish zip: $OUTPUT ==="

echo "--- Pre-check: repo cleanliness ---"
if ! bash "$REPO_ROOT/scripts/check-clean-publish-repo.sh"; then
  echo "ERROR: Publish repo is dirty. Run './scripts/sync-to-sandbox.sh' to clean."
  exit 1
fi
echo "Cleanliness pre-check: OK"

echo "--- Creating temp zip from git-tracked source files ---"
TMPZIP="$TMPDIR/kirkforge-plugin-publish-tmp.zip"
git ls-files -z | xargs -0 zip -q "$TMPZIP"
FILE_COUNT=$(unzip -l "$TMPZIP" | tail -1 | awk '{print $2}')
echo "Temp zip: $(du -h "$TMPZIP" | cut -f1), ${FILE_COUNT} files"

echo ""
echo "--- Extracting to temp dir ---"
mkdir -p "$TMPDIR/extract"
unzip -qq "$TMPZIP" -d "$TMPDIR/extract"
cd "$TMPDIR/extract"

# Verify key exclusions
FAIL=0
for check in ".git" "node_modules" "dist" ".tsbuildinfo"; do
  found=$(find . -name "$check" -not -path './.gitignore' -not -path './.github/*' 2>/dev/null | head -1 || true)
  if [ -n "$found" ]; then echo "  FAIL: '$check' found: $found"; FAIL=1; fi
done
[ $FAIL -eq 1 ] && echo "ERROR: Forbidden paths in zip." && exit 1
echo "Cleanliness: OK"
# Separate path-based checks for bench/ (find -name does not match path separators)
for check in "./bench/*.log"; do
  found=$(find . -path "$check" -o -path "${check}/*" 2>/dev/null | head -1 || true)
  if [ -n "$found" ]; then echo "  FAIL: bench artifact found: $found"; FAIL=1; fi
done

echo ""
echo "--- npm ci ---"
npm ci --silent 2>&1 | tail -3

echo ""
echo "--- npm run build ---"
npm run build 2>&1 | tail -5

echo ""
echo "--- npm test ---"
npm test 2>&1 | tail -10

echo ""
echo "--- Post-build clean check ---"
npm run clean 2>&1
FAIL=0
for check in "dist" ".tsbuildinfo"; do
  found=$(find . -name "*$check*" 2>/dev/null | head -1 || true)
  if [ -n "$found" ]; then echo "  FAIL: '$check' found after clean: $found"; FAIL=1; fi
done
[ $FAIL -eq 1 ] && echo "ERROR: Build artifacts remain after clean." && exit 1
echo "Clean lifecycle: OK (zip → ci → build → test → clean)"

echo ""
echo "=== PUBLISH ZIP VERIFIED ==="
mv "$TMPZIP" "$OUTPUT"
echo "Output: $OUTPUT"
