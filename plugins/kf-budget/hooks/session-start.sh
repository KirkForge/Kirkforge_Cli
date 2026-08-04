#!/usr/bin/env bash
set -euo pipefail

# session-start.sh — map KirkForge's session-start event to plugin3.
#
# Dual-mode:
# - Under Claude Code the canonical UserPromptSubmit payload arrives on stdin;
#   pass it straight through to plugin3.
# - Under KirkForge hooks receive only env vars and the user prompt is not
#   available, so emit the canonical Allow response directly.

if [ -z "${KF_EVENT:-}" ]; then
    # Claude Code mode: stdin carries the payload.
    source "$(dirname "$0")/plugin3_hook_common.sh"
    PLUGIN3_BIN="$(find_plugin3_bin)" || exit 0
    exec "$PLUGIN3_BIN" hook user-prompt-submit
fi

# KirkForge mode: no prompt available; default to allow so the session starts.
printf '{"kind":"allow"}\n'
