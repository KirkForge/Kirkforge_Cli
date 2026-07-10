#!/usr/bin/env bash
set -euo pipefail

# video_pipeline.sh — run a full video pipeline.

source "$(dirname "$0")/video_common.sh"

VIDEO_BIN="$(find_video_bin)" || die_json "kirkforge-video binary not found"

args="$(tool_args "$@")"
kind="$(json_get_string "$args" "kind" "animated_explainer")"
project="$(json_get_string "$args" "project" "projects/default")"
brief="$(json_get_string "$args" "brief" "")"

project="$(resolve_path "$project")"

if [[ -n "$brief" ]]; then
    brief="$(resolve_path "$brief")"
    "$VIDEO_BIN" from-brief "$brief" --project "$project" --kind "$kind"
else
    "$VIDEO_BIN" pipeline "$kind" --project "$project"
fi
