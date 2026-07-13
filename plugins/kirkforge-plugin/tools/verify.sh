#!/usr/bin/env bash
set -euo pipefail

# Run the kirkforge CLI `verify` command.
# Expects arguments in KIRKFORGE_TOOL_ARGS_JSON as { task?: string, json?: boolean }.

source "$(dirname "$0")/common.sh"

CLI_JS="$(find_cli)" || die "KirkForge CLI not found. Ensure the bundled npm/kirkforge-plugin tree is installed next to the plugins directory or set KIRKFORGE_CLI_JS."
require_node

TASK=$(node_json_arg "task")
JSON_FLAG=$(node_json_arg "json" "false")

ARGS=()
if [ -n "$TASK" ]; then
  ARGS+=(--task "$TASK")
fi
if node_is_truthy "$JSON_FLAG"; then
  ARGS+=(--json)
fi

exec node "$CLI_JS" verify "${ARGS[@]}"
