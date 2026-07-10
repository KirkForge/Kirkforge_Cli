#!/usr/bin/env bash
set -euo pipefail

# Run the kirkforge CLI `verify-workspace` command.
# Expects arguments in KIRKFORGE_TOOL_ARGS_JSON as:
#   { workspace: string, file?: string, language?: string, description?: string, taskId?: string }
# `file` may be a single path or space-separated paths.

source "$(dirname "$0")/common.sh"

CLI_JS="$(find_cli)" || die "KirkForge CLI not found. Ensure apps/cli/dist/index.js exists or kirkforge is on PATH."

if [ -z "${KIRKFORGE_TOOL_ARGS_JSON:-}" ]; then
  echo "Usage: provide KIRKFORGE_TOOL_ARGS_JSON such as {\"workspace\":\"/path/to/project\"}"
  exit 1
fi

WORKSPACE=$(node -e 'const a=JSON.parse(process.env.KIRKFORGE_TOOL_ARGS_JSON||"{}"); console.log(a.workspace||"")')
FILE=$(node -e 'const a=JSON.parse(process.env.KIRKFORGE_TOOL_ARGS_JSON||"{}"); console.log(a.file||"")')
LANGUAGE=$(node -e 'const a=JSON.parse(process.env.KIRKFORGE_TOOL_ARGS_JSON||"{}"); console.log(a.language||"")')
DESCRIPTION=$(node -e 'const a=JSON.parse(process.env.KIRKFORGE_TOOL_ARGS_JSON||"{}"); console.log(a.description||"")')
TASK_ID=$(node -e 'const a=JSON.parse(process.env.KIRKFORGE_TOOL_ARGS_JSON||"{}"); console.log(a.taskId||"")')

if [ -z "$WORKSPACE" ]; then
  echo "Error: workspace is required"
  echo "Usage: KIRKFORGE_TOOL_ARGS_JSON={\"workspace\":\"/path/to/project\"}"
  exit 1
fi

ARGS=(--workspace "$WORKSPACE")

if [ -n "$FILE" ]; then
  for f in $FILE; do
    ARGS+=(--file "$f")
  done
fi
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
