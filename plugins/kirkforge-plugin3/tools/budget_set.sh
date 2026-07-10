#!/usr/bin/env bash
set -euo pipefail

# budget_set.sh — set the token budget ceiling.

source "$(dirname "$0")/plugin3_common.sh"

PLUGIN3_BIN="$(find_plugin3_bin)" || die_json "plugin3 binary not found"

args="$(tool_args "$@")"
ceiling="$(json_get_integer "$args" "ceiling" "")"

if [[ -z "$ceiling" ]]; then
    die_json "missing required argument: ceiling"
fi

"$PLUGIN3_BIN" budget set "$ceiling"
