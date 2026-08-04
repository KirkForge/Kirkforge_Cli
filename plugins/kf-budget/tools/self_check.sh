#!/usr/bin/env bash
set -euo pipefail

# self_check.sh — run plugin3 self-check diagnostics.

source "$(dirname "$0")/plugin3_common.sh"

PLUGIN3_BIN="$(find_plugin3_bin)" || die_json "plugin3 binary not found"

"$PLUGIN3_BIN" self-check
