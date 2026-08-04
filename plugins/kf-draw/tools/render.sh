#!/usr/bin/env bash
# draw_render: render a .td.json to fenced markdown, no TUI.
# KirkForge invokes tools with the JSON args in $KIRKFORGE_TOOL_ARGS_JSON.
set -euo pipefail

source "$(dirname "$0")/common.sh"

KFD="$(find_kfd)" || die "draw_render: kfd binary not found (build the workspace or install kfd on PATH)"

ARGS="${KIRKFORGE_TOOL_ARGS_JSON:-"{}"}"
PATH_ARG="$(json_get_string "$ARGS" "path" "")"

if [[ -z "$PATH_ARG" ]]; then
  die "draw_render: missing 'path' argument"
fi

if [[ ! -f "$PATH_ARG" ]]; then
  die "draw_render: file not found: $PATH_ARG"
fi

exec "$KFD" --load "$PATH_ARG" --render --fenced
