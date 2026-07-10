#!/usr/bin/env bash
# session-start hook: emit the stratum ruleset for the active mode so the model
# knows the compression contract at the start of the session.
#
# Receives env vars: KF_EVENT, KF_SESSION_ID.

set -euo pipefail

source "$(dirname "$0")/../tools/common.sh"

STRATUM="$(find_stratum)" || {
  echo "[stratum hook: session-start] stratum binary not found (build the workspace or install stratum on PATH)" >&2
  exit 0
}

"$STRATUM" rules
