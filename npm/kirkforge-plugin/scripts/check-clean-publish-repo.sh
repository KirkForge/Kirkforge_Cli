#!/usr/bin/env bash
# check-clean-publish-repo.sh — Verify no runtime artifacts are in the publish repo
#
# Usage:
#   ./scripts/check-clean-publish-repo.sh
#
# Exits 0 if clean, exits 1 if problems found.
# Prints a report of any issues.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$REPO_ROOT"

ISSUES=0

echo "=== Checking publish repo cleanliness: $REPO_ROOT ==="
echo ""

# 1. Raw benchmark JSON reports
echo "Checking for raw benchmark JSON..."
FOUND_JSON=0
for f in bench/report-real-*.json; do
  if [[ -f "$f" ]]; then
    echo "  PROBLEM: $f (raw benchmark JSON — use bench/results/*.md summaries instead)"
    ((ISSUES++)) || true
    FOUND_JSON=1
  fi
done
if [[ $FOUND_JSON -eq 0 ]]; then
  echo "  OK: no raw benchmark JSON"
fi

# 2. Benchmark logs (use find to avoid glob double-matching)
echo "Checking for benchmark logs..."
FOUND_LOG=0
while IFS= read -r f; do
  if [[ -n "$f" ]]; then
    echo "  PROBLEM: $f (benchmark log — should be gitignored)"
    ((ISSUES++)) || true
    FOUND_LOG=1
  fi
done < <(find bench -maxdepth 1 -name '*.log' -type f 2>/dev/null | sort -u)
if [[ $FOUND_LOG -eq 0 ]]; then
  echo "  OK: no benchmark logs"
fi

# 3. .env files (other than .env.example)
echo "Checking for .env files..."
FOUND_ENV=0
for f in .env .env.local .env.production .env.staging; do
  if [[ -f "$f" ]]; then
    echo "  PROBLEM: $f (env file with potential secrets — use .env.example only)"
    ((ISSUES++)) || true
    FOUND_ENV=1
  fi
done
if [[ $FOUND_ENV -eq 0 ]]; then
  echo "  OK: no .env files"
fi

# 4. node_modules
echo "Checking for node_modules..."
if [[ -d "node_modules" ]]; then
  echo "  PROBLEM: node_modules/ exists (should be gitignored, run: rm -rf node_modules/)"
  ((ISSUES++)) || true
else
  echo "  OK: no node_modules"
fi

# 5. Build output (dist directories)
echo "Checking for dist directories..."
FOUND_DIST=0
while IFS= read -r d; do
  if [[ -n "$d" ]]; then
    echo "  PROBLEM: $d/ (build output — should be gitignored, run: npm run clean)"
    ((ISSUES++)) || true
    FOUND_DIST=1
  fi
done < <(find packages apps -maxdepth 2 -name 'dist' -type d 2>/dev/null | sort -u)
if [[ $FOUND_DIST -eq 0 ]]; then
  echo "  OK: no dist directories"
fi

# 6. .git directory
echo "Checking for .git directory..."
if [[ -d ".git" ]]; then
  echo "  PROBLEM: .git/ exists (repository history must not be in publish artifact)"
  ((ISSUES++)) || true
else
  echo "  OK: no .git directory"
fi

# 7. tsbuildinfo files
echo "Checking for tsbuildinfo files..."
FOUND_TSINFO=0
while IFS= read -r f; do
  if [[ -n "$f" ]]; then
    echo "  PROBLEM: $f (build cache — should be gitignored)"
    ((ISSUES++)) || true
    FOUND_TSINFO=1
  fi
done < <(find . -name '*.tsbuildinfo' -type f 2>/dev/null | sort -u)
if [[ $FOUND_TSINFO -eq 0 ]]; then
  echo "  OK: no tsbuildinfo files"
fi

# 8. Local /home/kirk paths in runtime scripts
echo "Checking for hardcoded local paths..."
FOUND_PATHS=0
for f in scripts/run-225-native-worker.sh scripts/sync-to-sandbox.sh; do
  if [[ -f "$f" ]]; then
    if grep -q '$HOME/' "$f" 2>/dev/null; then
      echo "  PROBLEM: $f contains hardcoded $HOME/ path"
      ((ISSUES++)) || true
      FOUND_PATHS=1
    fi
  fi
done
if [[ $FOUND_PATHS -eq 0 ]]; then
  echo "  OK: no hardcoded local paths in runtime scripts"
fi

# 9. PEM/key files
echo "Checking for secret key files..."
FOUND_KEYS=0
while IFS= read -r f; do
  if [[ -n "$f" ]]; then
    echo "  PROBLEM: $f (secret key file — must never be in repo)"
    ((ISSUES++)) || true
    FOUND_KEYS=1
  fi
done < <(find . -maxdepth 1 \( -name '*.pem' -o -name '*.key' -o -name 'id_rsa*' -o -name 'id_ed25519*' \) -type f 2>/dev/null | sort -u)
if [[ -d ".ssh" ]]; then
  echo "  PROBLEM: .ssh/ directory (must never be in repo)"
  ((ISSUES++)) || true
  FOUND_KEYS=1
fi
if [[ $FOUND_KEYS -eq 0 ]]; then
  echo "  OK: no secret key files"
fi

# 10. Check .gitignore coverage
echo "Checking .gitignore coverage..."
GITIGNORE_OK=true
if ! grep -q 'report-real-\*.json' .gitignore 2>/dev/null && ! grep -q 'bench/report-real' .gitignore 2>/dev/null; then
  echo "  PROBLEM: .gitignore missing bench report pattern"
  ((ISSUES++)) || true
  GITIGNORE_OK=false
fi
if ! grep -q '\.env$' .gitignore 2>/dev/null && ! grep -q '^\.env$' .gitignore 2>/dev/null; then
  echo "  PROBLEM: .gitignore missing .env"
  ((ISSUES++)) || true
  GITIGNORE_OK=false
fi
if ! grep -q 'node_modules' .gitignore 2>/dev/null; then
  echo "  PROBLEM: .gitignore missing node_modules"
  ((ISSUES++)) || true
  GITIGNORE_OK=false
fi
if ! grep -q '\.git' .gitignore 2>/dev/null; then
  echo "  PROBLEM: .gitignore missing .git"
  ((ISSUES++)) || true
  GITIGNORE_OK=false
fi
if ! grep -q 'tsbuildinfo' .gitignore 2>/dev/null; then
  echo "  PROBLEM: .gitignore missing tsbuildinfo"
  ((ISSUES++)) || true
  GITIGNORE_OK=false
fi
if $GITIGNORE_OK; then
  echo "  OK: .gitignore covers key patterns"
fi

echo ""
if [[ $ISSUES -eq 0 ]]; then
  echo "=== CLEAN: no runtime artifacts found ==="
  exit 0
else
  echo "=== DIRTY: $ISSUES issue(s) found ==="
  echo ""
  echo "Run './scripts/sync-to-sandbox.sh' to push changes to the sandbox."
  echo "Remove runtime artifacts from the publish repo."
  exit 1
fi
