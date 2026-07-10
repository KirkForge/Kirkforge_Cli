#!/usr/bin/env bash
# stratum_mode: show the active mode, or set it for this invocation.
# Arguments are passed via KIRKFORGE_TOOL_ARGS_JSON.

set -euo pipefail

source "$(dirname "$0")/common.sh"
STRATUM="$(find_stratum)" || die "stratum_mode: stratum binary not found (build the workspace or install stratum on PATH)"

KIRKFORGE_TOOL_ARGS_JSON="${KIRKFORGE_TOOL_ARGS_JSON:-${KIRKFORGE_TOOL_ARGS:-}}"

if [ -z "${KIRKFORGE_TOOL_ARGS_JSON:-}" ]; then
  echo "Usage: KIRKFORGE_TOOL_ARGS_JSON='{...}' $0"
  echo "Show or set the active stratum mode."
  echo "JSON keys: value, json"
  exit 1
fi

args=()
value=""

if command -v jq >/dev/null 2>&1; then
  value=$(jq -r '.value // empty' <<<"$KIRKFORGE_TOOL_ARGS_JSON")
  json_out=$(jq -r '.json // false' <<<"$KIRKFORGE_TOOL_ARGS_JSON")
else
  value=$(echo "$KIRKFORGE_TOOL_ARGS_JSON" | grep -o '"value"[[:space:]]*:[[:space:]]*"[^"]*"' | sed 's/.*"\([^"]*\)"/\1/' || true)
  json_out=$(echo "$KIRKFORGE_TOOL_ARGS_JSON" | grep -o '"json"[[:space:]]*:[[:space:]]*true' >/dev/null 2>&1 && echo true || echo false)
fi

[ "$json_out" = "true" ] && args+=("--json")

exec "$STRATUM" "${args[@]}" mode $value
