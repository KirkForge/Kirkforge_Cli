#!/usr/bin/env bash
set -euo pipefail

# budget_compact.sh — compact the budget store.

source "$(dirname "$0")/plugin3_common.sh"

PLUGIN3_BIN="$(find_plugin3_bin)" || die_json "plugin3 binary not found"

"$PLUGIN3_BIN" budget compact
