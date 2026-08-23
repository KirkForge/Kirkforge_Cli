#!/bin/bash
# Merge all remaining WO 43 branches, auto-resolving doc conflicts via union.
set -uo pipefail

BRANCHES="43.28 43.29 43.30 43.31 43.32 43.33 43.34 43.35 43.36 43.37 43.38 43.39 43.1 43.3 43.4"

for b in $BRANCHES; do
  echo "=== Merging wo$b ==="
  git merge "origin/wo/wo$b" --no-edit 2>&1 | tail -10 || true
  conflicts=$(git diff --name-only --diff-filter=U 2>/dev/null || true)
  if [ -n "$conflicts" ]; then
    echo "--- CONFLICTS in: ---"
    echo "$conflicts"
    # Classify and auto-resolve known doc files via union
    doc_conflicts=""
    code_conflicts=""
    for f in $conflicts; do
      case "$f" in
        CHANGELOG.md|docs/workorders/README.md|state.md|README.md|lessons.md)
          doc_conflicts="$doc_conflicts $f"
          ;;
        docs/workorders/*.md)
          doc_conflicts="$doc_conflicts $f"
          ;;
        *)
          code_conflicts="$code_conflicts $f"
          ;;
      esac
    done
    if [ -n "$doc_conflicts" ]; then
      echo "--- Auto-resolving doc conflicts:$doc_conflicts ---"
      python3 merge_resolve.py $doc_conflicts
      # Stage resolved files
      for f in $doc_conflicts; do
        git add "$f"
      done
    fi
    if [ -n "$code_conflicts" ]; then
      echo "--- CODE CONFLICTS (need manual):$code_conflicts ---"
    fi
    # Check if any conflicts remain after auto-resolve + add
    remaining=$(git diff --name-only --diff-filter=U 2>/dev/null || true)
    if [ -n "$remaining" ]; then
      echo "--- UNRESOLVED after auto-resolve:$remaining ---"
      echo "STOP: manual resolution needed for wo$b"
      exit 1
    fi
    # Verify no conflict markers remain in resolved files
    marker_check=""
    for f in $doc_conflicts; do
      if grep -qE '^(<<<<<<<|=======|>>>>>>>)' "$f" 2>/dev/null; then
        marker_check="$marker_check $f"
      fi
    done
    if [ -n "$marker_check" ]; then
      echo "--- CONFLICT MARKERS REMAIN in:$marker_check ---"
      exit 1
    fi
    git add -A && git commit --no-edit 2>&1 | tail -3
    echo "--- resolved + committed ---"
  else
    echo "--- clean ---"
  fi
  echo ""
done

echo "=== ALL MERGES COMPLETE ==="
git log --oneline -20