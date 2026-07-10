#!/usr/bin/env bash
set -euo pipefail

# pre-compact.sh — forward KirkForge's pre-compact event to plugin3 pre-compact.

source "$(dirname "$0")/plugin3_hook_common.sh"

PLUGIN3_BIN="$(find_plugin3_bin)" || exit 0  # Hooks are best-effort.

hook_payload | "$PLUGIN3_BIN" hook pre-compact
