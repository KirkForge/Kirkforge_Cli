#!/usr/bin/env bash
set -euo pipefail

# plugin3_hook_common.sh — shared helpers for KirkForge-Plugin3 plugin hooks.
# Sourced by the hook scripts; not invoked directly.

# Locate the plugin3 binary.
find_plugin3_bin() {
    local script_dir
    script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
    local candidates=(
        "$script_dir/plugin3"
        "$script_dir/../../../target/release/plugin3"
        "$script_dir/../../../target/debug/plugin3"
        "$(command -v plugin3 2>/dev/null || true)"
    )
    for c in "${candidates[@]}"; do
        if [[ -n "$c" && -x "$c" ]]; then
            printf '%s' "$c"
            return 0
        fi
    done
    return 1
}

# Build a JSON payload from hook env vars and write it to stdout.
hook_payload() {
    # Prefer Python for robust JSON construction, fall back to manual quoting.
    if command -v python3 > /dev/null 2>&1; then
        python3 - <<'PY'
import json, os
args_raw = os.environ.get("KF_TOOL_ARGS_JSON")
try:
    args = json.loads(args_raw) if args_raw else None
except Exception:
    args = None
print(json.dumps({
    "event": os.environ.get("KF_EVENT", ""),
    "tool": os.environ.get("KF_TOOL_NAME", ""),
    "args": args,
    "session_id": os.environ.get("KF_SESSION_ID", ""),
}))
PY
        return 0
    fi

    # Manual fallback.
    local event="${KF_EVENT:-}"
    local tool="${KF_TOOL_NAME:-}"
    local session="${KF_SESSION_ID:-}"
    local args="${KF_TOOL_ARGS_JSON:-null}"

    event="$(printf '%s' "$event" | sed 's/\\/\\\\/g; s/"/\\"/g')"
    tool="$(printf '%s' "$tool" | sed 's/\\/\\\\/g; s/"/\\"/g')"
    session="$(printf '%s' "$session" | sed 's/\\/\\\\/g; s/"/\\"/g')"

    printf '{"event":"%s","tool":"%s","args":%s,"session_id":"%s"}\n' \
        "$event" "$tool" "$args" "$session"
}
