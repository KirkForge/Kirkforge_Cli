#!/usr/bin/env bash
set -euo pipefail

# budget_status.sh — show the current token budget status.

source "$(dirname "$0")/plugin3_common.sh"

PLUGIN3_BIN="$(find_plugin3_bin)" || die_json "plugin3 binary not found"

"$PLUGIN3_BIN" budget status
