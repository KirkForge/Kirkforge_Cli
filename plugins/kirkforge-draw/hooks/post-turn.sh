#!/usr/bin/env bash
# post-turn: nudge the model to render any new .td.json files in cwd.
# Reads the event JSON from stdin, writes a verdict JSON to stdout.
# Verdict shape: {"verdict":"allow|deny","message":"..."}
set -euo pipefail

# Read and discard the event (we don't need its contents).
cat >/dev/null

# Quiet when nothing to do.
shopt -s nullglob
hits=( ./*.td.json ./out/*.td.json )
shopt -u nullglob

if [[ ${#hits[@]} -eq 0 ]]; then
  printf '{"verdict":"allow"}\n'
  exit 0
fi

# One-line notice; the model decides whether to render.
names=$(printf '%s\n' "${hits[@]}" | sort -u | head -5 | paste -sd, -)
if command -v jq > /dev/null 2>&1; then
  jq -n --arg names "$names" '{"verdict":"allow","message":"Found new .td.json: \($names). Render with kfd --load <path> --fenced if useful."}'
elif command -v python3 > /dev/null 2>&1; then
  python3 -c 'import json,sys; print(json.dumps({"verdict":"allow","message":f"Found new .td.json: {sys.argv[1]}. Render with kfd --load <path> --fenced if useful."}))' "$names"
else
  # Minimal escaping fallback.
  names="${names//\\/\\\\}"
  names="${names//\"/\\\"}"
  printf '{"verdict":"allow","message":"Found new .td.json: %s. Render with kfd --load <path> --fenced if useful."}\n' "$names"
fi
