#!/usr/bin/env bash
set -euo pipefail

# Run the kirkforge CLI `health` command.
# Accepts no arguments; KIRKFORGE_TOOL_ARGS_JSON may be empty or {}.

source "$(dirname "$0")/common.sh"

CLI_JS="$(find_cli)" || die "KirkForge CLI not found. Ensure the bundled npm/kirkforge-plugin tree is installed next to the plugins directory."
require_node

exec node "$CLI_JS" health
