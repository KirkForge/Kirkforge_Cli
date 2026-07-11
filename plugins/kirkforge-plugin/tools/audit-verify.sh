#!/usr/bin/env bash
set -euo pipefail

# Run the kirkforge CLI `audit-verify` command.
# Expects arguments in KIRKFORGE_TOOL_ARGS_JSON as { file: string, json?: boolean }.

source "$(dirname "$0")/common.sh"

CLI_JS="$(find_cli)" || die "KirkForge CLI not found. Ensure the bundled npm/kirkforge-plugin tree is installed next to the plugins directory or set KIRKFORGE_CLI_JS."
require_node

FILE=$(node_json_arg "file")
JSON_FLAG=$(node_json_arg "json" "false")

if [ -z "$FILE" ]; then
  echo "Error: file is required"
  echo "Usage: KIRKFORGE_TOOL_ARGS_JSON={\"file\":\"/path/to/audit.jsonl\"}"
  exit 1
fi

ARGS=(--file "$FILE")
if [ "$JSON_FLAG" = "true" ]; then
  ARGS+=(--json)
fi

exec node "$CLI_JS" audit-verify "${ARGS[@]}"
