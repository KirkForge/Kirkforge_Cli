#!/usr/bin/env bash
# pre-tool-bash hook: validate the stratum configuration before any bash tool
# is invoked so configuration drift is surfaced early.
#
# Receives env vars: KF_EVENT, KF_TOOL_NAME, KF_TOOL_ARGS_JSON, KF_SESSION_ID.

set -euo pipefail

source "$(dirname "$0")/../tools/common.sh"

STRATUM="$(find_stratum)" || {
  echo "[stratum hook: pre-tool-bash] stratum binary not found (build the workspace or install stratum on PATH)" >&2
  exit 0
}

"$STRATUM" config --validate
