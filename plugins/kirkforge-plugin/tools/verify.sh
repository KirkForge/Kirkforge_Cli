#!/usr/bin/env bash
set -euo pipefail

# Run the kirkforge CLI `verify` command.
# Expects arguments in KIRKFORGE_TOOL_ARGS_JSON as { task?: string, json?: boolean }.

source "$(dirname "$0")/common.sh"

CLI_JS="$(find_cli)" || die "KirkForge CLI not found. Ensure the bundled npm/kirkforge-plugin tree is installed next to the plugins directory."
require_node

if [ -z "${KIRKFORGE_TOOL_ARGS_JSON:-}" ]; then
  echo "Usage: provide KIRKFORGE_TOOL_ARGS_JSON such as {\"task\":\"verify self\",\"json\":true}"
  exit 1
fi

TASK=$(node -e 'const a=JSON.parse(process.env.KIRKFORGE_TOOL_ARGS_JSON||"{}"); console.log(a.task||"")')
JSON_FLAG=$(node -e 'const a=JSON.parse(process.env.KIRKFORGE_TOOL_ARGS_JSON||"{}"); console.log(a.json?"--json":"")')

ARGS=()
if [ -n "$TASK" ]; then
  ARGS+=(--task "$TASK")
fi
if [ -n "$JSON_FLAG" ]; then
  ARGS+=("$JSON_FLAG")
fi

exec node "$CLI_JS" verify "${ARGS[@]}"
