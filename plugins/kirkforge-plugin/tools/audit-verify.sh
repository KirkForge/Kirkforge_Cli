#!/usr/bin/env bash
set -euo pipefail

# Run the kirkforge CLI `audit-verify` command.
# Expects arguments in KIRKFORGE_TOOL_ARGS_JSON as { file: string, json?: boolean }.

source "$(dirname "$0")/common.sh"

CLI_JS="$(find_cli)" || die "KirkForge CLI not found. Ensure apps/cli/dist/index.js exists or kirkforge is on PATH."

if [ -z "${KIRKFORGE_TOOL_ARGS_JSON:-${KIRKFORGE_TOOL_ARGS:-}}" ]; then
  echo "Usage: provide KIRKFORGE_TOOL_ARGS_JSON such as {\"file\":\"/path/to/audit.jsonl\"}"
  exit 1
fi

FILE=$(node -e 'const a=JSON.parse(process.env.KIRKFORGE_TOOL_ARGS_JSON||"{}"); console.log(a.file||"")')
JSON_FLAG=$(node -e 'const a=JSON.parse(process.env.KIRKFORGE_TOOL_ARGS_JSON||"{}"); console.log(a.json?"--json":"")')

if [ -z "$FILE" ]; then
  echo "Error: file is required"
  echo "Usage: KIRKFORGE_TOOL_ARGS_JSON={\"file\":\"/path/to/audit.jsonl\"}"
  exit 1
fi

ARGS=(--file "$FILE")
if [ -n "$JSON_FLAG" ]; then
  ARGS+=("$JSON_FLAG")
fi

exec node "$CLI_JS" audit-verify "${ARGS[@]}"
