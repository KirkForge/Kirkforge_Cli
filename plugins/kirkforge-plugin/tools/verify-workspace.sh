#!/usr/bin/env bash
set -euo pipefail

# Run the kirkforge CLI `verify-workspace` command.
# Expects arguments in KIRKFORGE_TOOL_ARGS_JSON as:
#   { workspace: string, file?: string | string[], language?: string, description?: string, taskId?: string }
# `file` may be a single path or an array of paths.

source "$(dirname "$0")/common.sh"

CLI_JS="$(find_cli)" || die "KirkForge CLI not found. Ensure the bundled npm/kirkforge-plugin tree is installed next to the plugins directory or set KIRKFORGE_CLI_JS."
require_node

WORKSPACE=$(node_json_arg "workspace")
LANGUAGE=$(node_json_arg "language")
DESCRIPTION=$(node_json_arg "description")
TASK_ID=$(node_json_arg "taskId")

if [ -z "$WORKSPACE" ]; then
  echo "Error: workspace is required"
  echo "Usage: KIRKFORGE_TOOL_ARGS_JSON={\"workspace\":\"/path/to/project\"}"
  exit 1
fi

ARGS=(--workspace "$WORKSPACE")

# Read file paths (string or array) one per line so spaces in paths are preserved.
mapfile -t file_paths < <(node_json_file_arg)
for f in "${file_paths[@]}"; do
  [ -n "$f" ] || continue
  ARGS+=(--file "$f")
done

if [ -n "$LANGUAGE" ]; then
  ARGS+=(--language "$LANGUAGE")
fi
if [ -n "$DESCRIPTION" ]; then
  ARGS+=(--description "$DESCRIPTION")
fi
if [ -n "$TASK_ID" ]; then
  ARGS+=(--task-id "$TASK_ID")
fi

exec node "$CLI_JS" verify-workspace "${ARGS[@]}"
