#!/usr/bin/env bash
# draw_edit: open the kfd editor on a .td.json file.
set -euo pipefail

source "$(dirname "$0")/common.sh"

KFD="$(find_kfd)" || die "draw_edit: kfd binary not found (build the workspace or install kfd on PATH)"

ARGS="${KIRKFORGE_TOOL_ARGS_JSON:-"{}"}"
PATH_ARG="$(json_get_string "$ARGS" "path" "")"

if [[ -z "$PATH_ARG" ]]; then
  die "draw_edit: missing 'path' argument"
fi

if [[ ! -f "$PATH_ARG" ]]; then
  mkdir -p "$(dirname "$PATH_ARG")"
  printf '{"version":1,"objects":[]}\n' > "$PATH_ARG"
fi

# The KirkForge host runs plugin tools with a non-interactive (null) stdin so
# they cannot accidentally consume user input. A TUI editor needs a real
# terminal; fail cleanly instead of launching into a broken/captured screen.
if [[ ! -t 0 ]]; then
  die "draw_edit requires an interactive terminal; run: $KFD --load $PATH_ARG"
fi

exec "$KFD" --load "$PATH_ARG"
