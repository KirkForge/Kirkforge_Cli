#!/usr/bin/env bash
set -euo pipefail

# video_risk.sh — score slideshow risk for a scene plan.

source "$(dirname "$0")/video_common.sh"

VIDEO_BIN="$(find_video_bin)" || die_json "kirkforge-video binary not found"

args="$(tool_args "$@")"
project="$(json_get_string "$args" "project" "")"
duration_s="$(json_get_string "$args" "duration_s" "30")"

if [[ -n "$project" ]]; then
    project="$(resolve_path "$project")"
    "$VIDEO_BIN" risk --project "$project"
else
    kinds="$(json_get_string_array "$args" "kinds")"
    if [[ -z "$kinds" ]]; then
        die_json "provide project or kinds array"
    fi
    "$VIDEO_BIN" risk $kinds --duration-s "$duration_s"
fi
