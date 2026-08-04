#!/usr/bin/env bash
set -euo pipefail

# pre-compact.sh — forward KirkForge's pre-compact event to plugin3.
#
# Dual-mode:
# - Under Claude Code the canonical PreCompact payload arrives on stdin;
#   pass it straight through to plugin3.
# - Under KirkForge hooks receive compact metadata in env vars, not the full
#   history turns shape plugin3 expects, so emit the canonical empty response.

if [ -z "${KF_EVENT:-}" ]; then
    # Claude Code mode: stdin carries the payload.
    source "$(dirname "$0")/plugin3_hook_common.sh"
    PLUGIN3_BIN="$(find_plugin3_bin)" || exit 0
    exec "$PLUGIN3_BIN" hook pre-compact
fi

# KirkForge mode: history turns are not provided to hooks; proceed with host compaction.
printf '{"hint":null,"summary":""}\n'
