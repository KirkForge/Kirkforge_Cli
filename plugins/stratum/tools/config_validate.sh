#!/usr/bin/env bash
# stratum_config_validate: validate the effective stratum configuration.
# Arguments are passed via KIRKFORGE_TOOL_ARGS_JSON.

set -euo pipefail

source "$(dirname "$0")/common.sh"
STRATUM="$(find_stratum)" || die "stratum_config_validate: stratum binary not found (build the workspace or install stratum on PATH)"

ARGS="$(stratum_args)"

args=("--validate")

json_out="$(json_get_bool "$ARGS" "json" "false")"

[ "$json_out" = "true" ] && args+=("--json")

exec "$STRATUM" config "${args[@]}"
