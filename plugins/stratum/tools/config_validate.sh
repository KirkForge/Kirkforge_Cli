#!/usr/bin/env bash
# stratum_config_validate: validate the effective stratum configuration.
# Arguments are passed via KIRKFORGE_TOOL_ARGS_JSON.

set -euo pipefail

source "$(dirname "$0")/common.sh"
STRATUM="$(find_stratum)" || die "stratum_config_validate: stratum binary not found (build the workspace or install stratum on PATH)"

: "${KIRKFORGE_TOOL_ARGS_JSON:={}}"

args=("--validate")

if command -v jq >/dev/null 2>&1; then
  json_out=$(jq -r '.json // false' <<<"$KIRKFORGE_TOOL_ARGS_JSON")
else
  json_out=$(echo "$KIRKFORGE_TOOL_ARGS_JSON" | grep -o '"json"[[:space:]]*:[[:space:]]*true' >/dev/null 2>&1 && echo true || echo false)
fi

[ "$json_out" = "true" ] && args+=("--json")

exec "$STRATUM" config "${args[@]}"
