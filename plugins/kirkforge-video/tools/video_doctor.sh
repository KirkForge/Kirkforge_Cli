#!/usr/bin/env bash
set -euo pipefail

# video_doctor.sh — probe ffmpeg or validate a project directory.

source "$(dirname "$0")/video_common.sh"

VIDEO_BIN="$(find_video_bin)" || die_json "kirkforge-video binary not found"

args="$(tool_args "$@")"
check="$(json_get_string "$args" "check" "ffmpeg")"

json_flag=""
if [[ "$(json_get_bool "$args" "json" "false")" == "true" ]]; then
    json_flag="--json"
fi

case "$check" in
    ffmpeg)
        ffmpeg_path="$(json_get_string "$args" "ffmpeg_path" "ffmpeg")"
        if [[ -n "$json_flag" ]]; then
            "$VIDEO_BIN" doctor ffmpeg --ffmpeg-path "$ffmpeg_path" --json
        else
            "$VIDEO_BIN" doctor ffmpeg --ffmpeg-path "$ffmpeg_path"
        fi
        ;;
    project)
        project="$(json_get_string "$args" "project" "projects/default")"
        project="$(resolve_path "$project")"
        if [[ -n "$json_flag" ]]; then
            "$VIDEO_BIN" doctor project --project "$project" --json
        else
            "$VIDEO_BIN" doctor project --project "$project"
        fi
        ;;
    *)
        die_json "unknown doctor check: $check (use ffmpeg|project)"
        ;;
esac
