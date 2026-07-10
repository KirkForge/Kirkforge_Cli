#!/usr/bin/env bash
set -euo pipefail

# video_demos.sh — list demos, pipelines, and profiles.

source "$(dirname "$0")/video_common.sh"

VIDEO_BIN="$(find_video_bin)" || die_json "kirkforge-video binary not found"

args="$(tool_args "$@")"
cmd="$(json_get_string "$args" "command" "demos")"
case "$cmd" in
    demos)       "$VIDEO_BIN" demos ;;
    pipelines)   "$VIDEO_BIN" pipelines list ;;
    profiles)    "$VIDEO_BIN" profiles list ;;
    tools)       "$VIDEO_BIN" tools ;;
    *)           die_json "unknown list command: $cmd (use demos|pipelines|profiles|tools)" ;;
esac
