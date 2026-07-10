#!/usr/bin/env bash
# stratum_apply: apply the stratum pipeline to a file or stdin.
# Arguments are passed via KIRKFORGE_TOOL_ARGS_JSON.

set -euo pipefail

source "$(dirname "$0")/common.sh"
STRATUM="$(find_stratum)" || die "stratum_apply: stratum binary not found (build the workspace or install stratum on PATH)"

KIRKFORGE_TOOL_ARGS_JSON="${KIRKFORGE_TOOL_ARGS_JSON:-${KIRKFORGE_TOOL_ARGS:-}}"

if [ -z "${KIRKFORGE_TOOL_ARGS_JSON:-}" ]; then
  echo "Usage: KIRKFORGE_TOOL_ARGS_JSON='{...}' $0"
  echo "Apply the stratum pipeline to a file or stdin."
  echo "JSON keys: file, content_type, mode, token_budget, json, dry_run"
  exit 1
fi

args=()
file=""
content_type=""

if command -v jq >/dev/null 2>&1; then
  file=$(jq -r '.file // empty' <<<"$KIRKFORGE_TOOL_ARGS_JSON")
  content_type=$(jq -r '.content_type // empty' <<<"$KIRKFORGE_TOOL_ARGS_JSON")
  mode=$(jq -r '.mode // empty' <<<"$KIRKFORGE_TOOL_ARGS_JSON")
  token_budget=$(jq -r '.token_budget // empty' <<<"$KIRKFORGE_TOOL_ARGS_JSON")
  json_out=$(jq -r '.json // false' <<<"$KIRKFORGE_TOOL_ARGS_JSON")
  dry_run=$(jq -r '.dry_run // false' <<<"$KIRKFORGE_TOOL_ARGS_JSON")
else
  file=$(echo "$KIRKFORGE_TOOL_ARGS_JSON" | grep -o '"file"[[:space:]]*:[[:space:]]*"[^"]*"' | sed 's/.*"\([^"]*\)"/\1/' || true)
  content_type=$(echo "$KIRKFORGE_TOOL_ARGS_JSON" | grep -o '"content_type"[[:space:]]*:[[:space:]]*"[^"]*"' | sed 's/.*"\([^"]*\)"/\1/' || true)
  mode=$(echo "$KIRKFORGE_TOOL_ARGS_JSON" | grep -o '"mode"[[:space:]]*:[[:space:]]*"[^"]*"' | sed 's/.*"\([^"]*\)"/\1/' || true)
  token_budget=$(echo "$KIRKFORGE_TOOL_ARGS_JSON" | grep -o '"token_budget"[[:space:]]*:[[:space:]]*[0-9]*' | sed 's/.*://' || true)
  json_out=$(echo "$KIRKFORGE_TOOL_ARGS_JSON" | grep -o '"json"[[:space:]]*:[[:space:]]*true' >/dev/null 2>&1 && echo true || echo false)
  dry_run=$(echo "$KIRKFORGE_TOOL_ARGS_JSON" | grep -o '"dry_run"[[:space:]]*:[[:space:]]*true' >/dev/null 2>&1 && echo true || echo false)
fi

[ -n "$content_type" ] && args+=("--content-type" "$content_type")
[ -n "$mode" ] && args+=("--mode" "$mode")
[ -n "$token_budget" ] && args+=("--token-budget" "$token_budget")
[ "$json_out" = "true" ] && args+=("--json")
[ "$dry_run" = "true" ] && args+=("--dry-run")

if [ -n "$file" ]; then
  if [ ! -r "$file" ]; then
    echo "Error: cannot read file: $file" >&2
    exit 2
  fi
  exec "$STRATUM" "${args[@]}" apply "$file"
else
  exec "$STRATUM" "${args[@]}" apply
fi
