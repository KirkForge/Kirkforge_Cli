#!/usr/bin/env bash
# stratum_run: run the stratum pipeline on stdin.
# Arguments are passed via KIRKFORGE_TOOL_ARGS_JSON.

set -euo pipefail

source "$(dirname "$0")/common.sh"
STRATUM="$(find_stratum)" || die "stratum_run: stratum binary not found (build the workspace or install stratum on PATH)"

if [ -z "${KIRKFORGE_TOOL_ARGS_JSON:-}" ]; then
  echo "Usage: KIRKFORGE_TOOL_ARGS_JSON='{...}' $0"
  echo "Run the stratum pipeline on stdin."
  exit 1
fi

args=()

if command -v jq >/dev/null 2>&1; then
  mode=$(jq -r '.mode // empty' <<<"$KIRKFORGE_TOOL_ARGS_JSON")
  token_budget=$(jq -r '.token_budget // empty' <<<"$KIRKFORGE_TOOL_ARGS_JSON")
  json_out=$(jq -r '.json // false' <<<"$KIRKFORGE_TOOL_ARGS_JSON")
  dry_run=$(jq -r '.dry_run // false' <<<"$KIRKFORGE_TOOL_ARGS_JSON")
  max_input_size=$(jq -r '.max_input_size // empty' <<<"$KIRKFORGE_TOOL_ARGS_JSON")
else
  mode=$(echo "$KIRKFORGE_TOOL_ARGS_JSON" | grep -o '"mode"[[:space:]]*:[[:space:]]*"[^"]*"' | sed 's/.*"\([^"]*\)"/\1/' || true)
  token_budget=$(echo "$KIRKFORGE_TOOL_ARGS_JSON" | grep -o '"token_budget"[[:space:]]*:[[:space:]]*[0-9]*' | sed 's/.*://' || true)
  json_out=$(echo "$KIRKFORGE_TOOL_ARGS_JSON" | grep -o '"json"[[:space:]]*:[[:space:]]*true' >/dev/null 2>&1 && echo true || echo false)
  dry_run=$(echo "$KIRKFORGE_TOOL_ARGS_JSON" | grep -o '"dry_run"[[:space:]]*:[[:space:]]*true' >/dev/null 2>&1 && echo true || echo false)
  max_input_size=$(echo "$KIRKFORGE_TOOL_ARGS_JSON" | grep -o '"max_input_size"[[:space:]]*:[[:space:]]*[0-9]*' | sed 's/.*://' || true)
fi

[ -n "$mode" ] && args+=("--mode" "$mode")
[ -n "$token_budget" ] && args+=("--token-budget" "$token_budget")
[ "$json_out" = "true" ] && args+=("--json")
[ "$dry_run" = "true" ] && args+=("--dry-run")
[ -n "$max_input_size" ] && args+=("--max-input-size" "$max_input_size")

exec "$STRATUM" "${args[@]}" run
