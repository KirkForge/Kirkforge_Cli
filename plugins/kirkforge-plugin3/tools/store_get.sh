#!/usr/bin/env bash
set -euo pipefail

# store_get.sh — retrieve a stored marker by key.

source "$(dirname "$0")/plugin3_common.sh"

PLUGIN3_BIN="$(find_plugin3_bin)" || die_json "plugin3 binary not found"

args="$(tool_args "$@")"
marker="$(json_get_string "$args" "marker" "")"

if [[ -z "$marker" ]]; then
    die_json "missing required argument: marker"
fi

"$PLUGIN3_BIN" store get "$marker"
