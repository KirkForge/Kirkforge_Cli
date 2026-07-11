#!/usr/bin/env bash
# stratum_rules: emit the canonical ruleset for the active or requested mode.
# Arguments are passed via KIRKFORGE_TOOL_ARGS_JSON.

set -euo pipefail

source "$(dirname "$0")/common.sh"
STRATUM="$(find_stratum)" || die "stratum_rules: stratum binary not found (build the workspace or install stratum on PATH)"

ARGS="$(stratum_args)"

args=()

mode="$(json_get_string "$ARGS" "mode" "")"
json_out="$(json_get_bool "$ARGS" "json" "false")"

[ -n "$mode" ] && args+=("--mode" "$mode")
[ "$json_out" = "true" ] && args+=("--json")

exec "$STRATUM" "${args[@]}" rules
