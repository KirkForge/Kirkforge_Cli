#!/usr/bin/env bash
# draw_edit: open the kfd editor on a .td.json file.
set -euo pipefail

source "$(dirname "$0")/common.sh"

KFD="$(find_kfd)" || die "draw_edit: kfd binary not found (build the workspace or install kfd on PATH)"

ARGS="${KIRKFORGE_TOOL_ARGS_JSON:-${KIRKFORGE_TOOL_ARGS:-}}"
PATH_ARG="$(printf '%s' "$ARGS" | sed -n 's/.*"path"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p')"

if [[ -z "$PATH_ARG" ]]; then
  die "draw_edit: missing 'path' argument"
fi

if [[ ! -f "$PATH_ARG" ]]; then
  mkdir -p "$(dirname "$PATH_ARG")"
  printf '{"version":1,"objects":[]}\n' > "$PATH_ARG"
fi

exec "$KFD" --load "$PATH_ARG"
