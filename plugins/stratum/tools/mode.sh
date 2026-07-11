#!/usr/bin/env bash
# stratum_mode: show the active mode, or set it for this invocation.
# Arguments are passed via KIRKFORGE_TOOL_ARGS_JSON.

set -euo pipefail

source "$(dirname "$0")/common.sh"
STRATUM="$(find_stratum)" || die "stratum_mode: stratum binary not found (build the workspace or install stratum on PATH)"

ARGS="$(stratum_args)"

args=()

value="$(json_get_string "$ARGS" "value" "")"
json_out="$(json_get_bool "$ARGS" "json" "false")"

[ "$json_out" = "true" ] && args+=("--json")

exec "$STRATUM" "${args[@]}" mode ${value:+"$value"}
