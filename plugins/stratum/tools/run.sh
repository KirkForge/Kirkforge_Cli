#!/usr/bin/env bash
# stratum_run: run the stratum pipeline on stdin.
# Arguments are passed via KIRKFORGE_TOOL_ARGS_JSON.

set -euo pipefail

source "$(dirname "$0")/common.sh"
STRATUM="$(find_stratum)" || die "stratum_run: stratum binary not found (build the workspace or install stratum on PATH)"

ARGS="$(stratum_args)"

args=()

mode="$(json_get_string "$ARGS" "mode" "")"
token_budget="$(json_get_integer "$ARGS" "token_budget" "")"
json_out="$(json_get_bool "$ARGS" "json" "false")"
dry_run="$(json_get_bool "$ARGS" "dry_run" "false")"
max_input_size="$(json_get_integer "$ARGS" "max_input_size" "")"

[ -n "$mode" ] && args+=("--mode" "$mode")
[ -n "$token_budget" ] && args+=("--token-budget" "$token_budget")
[ "$json_out" = "true" ] && args+=("--json")
[ "$dry_run" = "true" ] && args+=("--dry-run")
[ -n "$max_input_size" ] && args+=("--max-input-size" "$max_input_size")

if ! json_has_key "$ARGS" "input"; then
  die "stratum_run: missing 'input' field; pass the text to compress as input, or use stratum_apply for files"
fi

input="$(json_get_string "$ARGS" "input" "")"
printf '%s' "$input" | "$STRATUM" "${args[@]}" run
