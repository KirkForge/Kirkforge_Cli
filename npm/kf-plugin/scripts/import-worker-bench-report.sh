#!/usr/bin/env bash
# import-worker-bench-report.sh — Copy worker benchmark markdown reports into publish repo
#
# Usage:
#   ./scripts/import-worker-bench-report.sh <path-to-worker-md-report>
#   ./scripts/import-worker-bench-report.sh --force <path-to-worker-md-report>
#
# Copies only .md files into bench/results/.
# Refuses .json, .log, .tmp, .env, or anything outside markdown.
# Refuses overwrite unless --force is passed.
# Runs scripts/check-clean-publish-repo.sh after copy.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
RESULTS_DIR="$REPO_ROOT/bench/results"
FORCE=false

if [[ "${1:-}" == "--force" ]]; then
  FORCE=true
  shift
fi

if [[ $# -lt 1 ]]; then
  echo "Usage: $0 [--force] <path-to-worker-md-report>" >&2
  echo "" >&2
  echo "Copies markdown benchmark reports from a worker into bench/results/." >&2
  echo "Only .md files are accepted. No raw JSON, logs, or env files." >&2
  exit 1
fi

SOURCE="$1"

if [[ ! -f "$SOURCE" ]]; then
  echo "ERROR: File not found: $SOURCE" >&2
  exit 1
fi

# Only accept markdown files
if [[ "$SOURCE" != *.md ]]; then
  echo "ERROR: Only .md files are accepted. Got: $SOURCE" >&2
  echo "Raw JSON, logs, .env, and .tmp files must not enter the publish repo." >&2
  exit 1
fi

FILENAME="$(basename "$SOURCE")"
DEST="$RESULTS_DIR/$FILENAME"

# Check for forbidden content patterns
FORBIDDEN=false
if grep -qE '\.(json|log|tmp|env)' "$SOURCE" 2>/dev/null; then
  # Having references to .json/.log files in a markdown report is fine —
  # the check is about not importing those actual file types.
  :
fi

# Refuse overwrite unless --force
if [[ -f "$DEST" && "$FORCE" != "true" ]]; then
  echo "ERROR: $DEST already exists. Use --force to overwrite." >&2
  exit 1
fi

mkdir -p "$RESULTS_DIR"
cp "$SOURCE" "$DEST"
echo "Copied: $SOURCE -> $DEST"

# Verify publish repo cleanliness
echo ""
bash "$REPO_ROOT/scripts/check-clean-publish-repo.sh"
