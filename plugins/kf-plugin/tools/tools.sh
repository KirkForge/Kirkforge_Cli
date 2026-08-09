#!/usr/bin/env bash
set -euo pipefail

# Run the kf-code CLI `tools` command.
# Accepts no arguments; KIRKFORGE_TOOL_ARGS_JSON may be empty or {}.

source "$(dirname "$0")/common.sh"

CLI_JS="$(find_cli)" || die "Kf-Code CLI not found. Ensure the bundled npm/kf-plugin tree is installed next to the plugins directory or set KF_CODE_CLI_JS."
require_node

exec node "$CLI_JS" tools
