#!/usr/bin/env bash
# post-turn: nudge the model to render any new .td.json files in cwd.
# Reads the event JSON from stdin, writes a verdict JSON to stdout.
# Verdict shape: {"verdict":"allow|deny","message":"..."}
set -euo pipefail

# Under Claude Code the event JSON arrives on stdin; consume it so the
# host pipe does not block. Under KirkForge hooks receive only env vars,
# so leave stdin alone (the host already provides a null stdin).
if [ -z "${KF_EVENT:-}" ]; then
    cat >/dev/null
fi

# Quiet when nothing to do.
shopt -s nullglob
hits=( ./*.td.json ./out/*.td.json )
shopt -u nullglob

if [[ ${#hits[@]} -eq 0 ]]; then
  printf '{"verdict":"allow"}\n'
  exit 0
fi

# One-line notice; the model decides whether to render.
sorted=$(printf '%s\n' "${hits[@]}" | sort -u)
count=$(printf '%s\n' "$sorted" | wc -l)
names=$(printf '%s\n' "$sorted" | head -5 | paste -sd, -)
if [[ "$count" -gt 5 ]]; then
  names="${names}, ..."
fi
if command -v jq > /dev/null 2>&1; then
  jq -n --arg names "$names" '{"verdict":"allow","message":"Found new .td.json: \($names). Render with kfd --load <path> --render --fenced if useful."}'
elif command -v python3 > /dev/null 2>&1; then
  python3 -c 'import json,sys; print(json.dumps({"verdict":"allow","message":f"Found new .td.json: {sys.argv[1]}. Render with kfd --load <path> --render --fenced if useful."}))' "$names"
else
  die "post-turn: jq or python3 is required to encode the hook verdict"
fi
