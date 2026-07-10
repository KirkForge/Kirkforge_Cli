#!/usr/bin/env bash
set -euo pipefail

# video_render.sh — render an existing scene_plan.json to final.mp4.

source "$(dirname "$0")/video_common.sh"

VIDEO_BIN="$(find_video_bin)" || die_json "kirkforge-video binary not found"

args="$(tool_args "$@")"
project="$(json_get_string "$args" "project" "projects/default")"
profile="$(json_get_string "$args" "profile" "")"

project="$(resolve_path "$project")"

if [[ -n "$profile" ]]; then
    "$VIDEO_BIN" render --project "$project" --profile "$profile"
else
    "$VIDEO_BIN" render --project "$project"
fi
