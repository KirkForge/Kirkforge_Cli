#!/usr/bin/env bash
set -euo pipefail

# video_validate.sh — validate a scene_plan.json without rendering.

source "$(dirname "$0")/video_common.sh"

VIDEO_BIN="$(find_video_bin)" || die_json "kirkforge-video binary not found"

args="$(tool_args "$@")"
path="$(json_get_string "$args" "path" "projects/default")"
path="$(resolve_path "$path")"

"$VIDEO_BIN" validate "$path"
