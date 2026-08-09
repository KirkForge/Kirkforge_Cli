#!/usr/bin/env bash
set -euo pipefail

# Run the kf-code CLI `doctor` command.
# Expects arguments in KIRKFORGE_TOOL_ARGS_JSON as { pretty?: boolean }.
# Default output is JSON; pass pretty=true for human-readable output.

source "$(dirname "$0")/common.sh"

CLI_JS="$(find_cli)" || die "Kf-Code CLI not found. Ensure the bundled npm/kf-plugin tree is installed next to the plugins directory or set KF_CODE_CLI_JS."
require_node

PRETTY_FLAG=$(node_json_arg "pretty" "false")

ARGS=()
if node_is_truthy "$PRETTY_FLAG"; then
  ARGS+=(--pretty)
fi

exec node "$CLI_JS" doctor "${ARGS[@]}"
