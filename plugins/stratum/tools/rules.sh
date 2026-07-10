#!/usr/bin/env bash
# stratum_rules: emit the canonical ruleset for the active or requested mode.
# Arguments are passed via KIRKFORGE_TOOL_ARGS_JSON.

set -euo pipefail

source "$(dirname "$0")/common.sh"
STRATUM="$(find_stratum)" || die "stratum_rules: stratum binary not found (build the workspace or install stratum on PATH)"

KIRKFORGE_TOOL_ARGS_JSON="${KIRKFORGE_TOOL_ARGS_JSON:-${KIRKFORGE_TOOL_ARGS:-}}"

if [ -z "${KIRKFORGE_TOOL_ARGS_JSON:-}" ]; then
  echo "Usage: KIRKFORGE_TOOL_ARGS_JSON='{...}' $0"
  echo "Emit the stratum ruleset for the active or requested mode."
  echo "JSON keys: mode, json"
  exit 1
fi

args=()

if command -v jq >/dev/null 2>&1; then
  mode=$(jq -r '.mode // empty' <<<"$KIRKFORGE_TOOL_ARGS_JSON")
  json_out=$(jq -r '.json // false' <<<"$KIRKFORGE_TOOL_ARGS_JSON")
else
  mode=$(echo "$KIRKFORGE_TOOL_ARGS_JSON" | grep -o '"mode"[[:space:]]*:[[:space:]]*"[^"]*"' | sed 's/.*"\([^"]*\)"/\1/' || true)
  json_out=$(echo "$KIRKFORGE_TOOL_ARGS_JSON" | grep -o '"json"[[:space:]]*:[[:space:]]*true' >/dev/null 2>&1 && echo true || echo false)
fi

[ -n "$mode" ] && args+=("--mode" "$mode")
[ "$json_out" = "true" ] && args+=("--json")

exec "$STRATUM" "${args[@]}" rules
