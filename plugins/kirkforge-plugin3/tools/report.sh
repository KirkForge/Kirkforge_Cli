#!/usr/bin/env bash
set -euo pipefail

# report.sh — print a spending report.

source "$(dirname "$0")/plugin3_common.sh"

PLUGIN3_BIN="$(find_plugin3_bin)" || die_json "plugin3 binary not found"

"$PLUGIN3_BIN" report
