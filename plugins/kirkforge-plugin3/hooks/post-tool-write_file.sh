#!/usr/bin/env bash
set -euo pipefail

# post-tool-write_file.sh — forward KirkForge's post-tool-write_file event to plugin3.
#
# Dual-mode:
# - Under Claude Code the canonical PostToolUse payload arrives on stdin;
#   pass it straight through to plugin3.
# - Under KirkForge hooks receive only env vars and the tool result content
#   is not available, so emit the canonical no-op response directly.

if [ -z "${KF_EVENT:-}" ]; then
    # Claude Code mode: stdin carries the payload.
    source "$(dirname "$0")/plugin3_hook_common.sh"
    PLUGIN3_BIN="$(find_plugin3_bin)" || exit 0
    exec "$PLUGIN3_BIN" hook post-tool-use
fi

# KirkForge mode: no tool result content available; respond with a clean no-op.
printf '{"content":"","note":null}\n'
