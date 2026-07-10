#!/usr/bin/env bash
set -euo pipefail

# Run the kirkforge CLI `doctor` command.
# Expects arguments in KIRKFORGE_TOOL_ARGS_JSON as { pretty?: boolean }.
# Default output is JSON; pass pretty=true for human-readable output.

source "$(dirname "$0")/common.sh"

CLI_JS="$(find_cli)" || die "KirkForge CLI not found. Ensure apps/cli/dist/index.js exists or kirkforge is on PATH."

: "${KIRKFORGE_TOOL_ARGS_JSON:=${KIRKFORGE_TOOL_ARGS:-}}"
if [ -z "$KIRKFORGE_TOOL_ARGS_JSON" ]; then
  KIRKFORGE_TOOL_ARGS_JSON="{}"
fi

PRETTY_FLAG=$(node -e 'const a=JSON.parse(process.env.KIRKFORGE_TOOL_ARGS_JSON||"{}"); console.log(a.pretty?"--pretty":"")')

ARGS=()
if [ -n "$PRETTY_FLAG" ]; then
  ARGS+=("$PRETTY_FLAG")
fi

exec node "$CLI_JS" doctor "${ARGS[@]}"
