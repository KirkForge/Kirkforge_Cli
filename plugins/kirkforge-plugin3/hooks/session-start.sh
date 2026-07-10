#!/usr/bin/env bash
set -euo pipefail

# session-start.sh — map KirkForge's session-start event to plugin3 user-prompt-submit.

source "$(dirname "$0")/plugin3_hook_common.sh"

PLUGIN3_BIN="$(find_plugin3_bin)" || exit 0  # Hooks are best-effort.

hook_payload | "$PLUGIN3_BIN" hook user-prompt-submit
