#!/usr/bin/env bash
set -euo pipefail

# video_decision_log.sh — print recent decision log entries.

source "$(dirname "$0")/video_common.sh"

VIDEO_BIN="$(find_video_bin)" || die_json "kirkforge-video binary not found"

args="$(tool_args "$@")"
project="$(json_get_string "$args" "project" "projects/default")"
project="$(resolve_path "$project")"

since_s="$(json_get_string "$args" "since_s" "")"
category="$(json_get_string "$args" "category" "")"

opts=(--project "$project")
[[ -n "$since_s" ]] && opts+=(--since-s "$since_s")
[[ -n "$category" ]] && opts+=(--category "$category")

"$VIDEO_BIN" decision-log "${opts[@]}"
