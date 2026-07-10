#!/usr/bin/env bash
set -euo pipefail

# post-tool-bash.sh — forward KirkForge's post-tool-bash event to plugin3 post-tool-use.

source "$(dirname "$0")/plugin3_hook_common.sh"

PLUGIN3_BIN="$(find_plugin3_bin)" || exit 0  # Hooks are best-effort.

hook_payload | "$PLUGIN3_BIN" hook post-tool-use
