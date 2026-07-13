#!/usr/bin/env bash
set -euo pipefail

# common.sh — shared helpers for the Stratum filesystem plugin.
# Sourced by the tool scripts; not invoked directly.

# Locate the stratum binary.
# 1. Same directory as this script (installed layout may copy the binary here)
# 2. Built executable in the workspace target directory
# 3. Installed executable on PATH
find_stratum() {
    local script_dir
    script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
    local target_dir="${CARGO_TARGET_DIR:-${script_dir}/../../../target}"
    local candidates=(
        "${script_dir}/stratum"
        "${script_dir}/stratum.exe"
        "${target_dir}/release/stratum"
        "${target_dir}/release/stratum.exe"
        "${target_dir}/debug/stratum"
        "${target_dir}/debug/stratum.exe"
        "$(command -v stratum 2>/dev/null || true)"
    )
    for c in "${candidates[@]}"; do
        if [[ -n "$c" && -x "$c" ]]; then
            printf '%s' "$c"
            return 0
        fi
    done
    return 1
}

# Print usage error and exit non-zero.
die() {
    printf '%s\n' "$1" >&2
    exit 1
}

# Read KIRKFORGE_TOOL_ARGS_JSON, normalising empty/unset to "{}".
stratum_args() {
    local args="${KIRKFORGE_TOOL_ARGS_JSON:-"{}"}"
    [[ -n "$args" ]] || args="{}"
    printf '%s' "$args"
}

# Extract a top-level scalar string value from a JSON object.
# Falls back to $default when the key is missing or not a scalar.
# Supports jq, python3, and a naive bash fallback for simple flat objects.
json_get_string() {
    local json="$1" key="$2" default="${3:-}"
    local value

    if command -v jq >/dev/null 2>&1; then
        value="$(printf '%s' "$json" | jq -r --arg key "$key" --arg default "$default" '.[$key] // $default // ""')"
        printf '%s' "$value"
        return 0
    fi

    if command -v python3 >/dev/null 2>&1; then
        value="$(printf '%s' "$json" | KEY="$key" DEFAULT="$default" python3 -c '
import sys, json, os
d = json.load(sys.stdin)
k = os.environ["KEY"]
default = os.environ["DEFAULT"]
v = d.get(k)
if v is None or v == "":
    print(default)
else:
    print(v)
')"
        printf '%s' "$value"
        return 0
    fi

    # Fallback removed: jq or python3 is required to safely extract JSON
    # string values. This avoids silent wrong answers for keys that appear
    # as substrings or values containing escaped quotes.
    die "json_get_string: jq or python3 is required to parse tool arguments"
}

# Extract a top-level integer value. Preserves an explicitly empty default
# so callers can detect a missing key.
json_get_integer() {
    local json="$1" key="$2" default
    if [[ $# -ge 3 ]]; then
        default="$3"
    else
        default="0"
    fi
    local value
    value="$(json_get_string "$json" "$key" "$default")"
    value="${value%.*}"
    if [[ "$value" =~ ^-?[0-9]+$ ]]; then
        printf '%s' "$value"
    else
        printf '%s' "$default"
    fi
}

# Extract a top-level boolean value.
# Accepts common truthy spellings (true, True, 1, yes, y, on) so model-provided
# string values are handled consistently with the Node SDK plugin wrappers.
json_get_bool() {
    local json="$1" key="$2" default="${3:-false}"
    local value
    value="$(json_get_string "$json" "$key" "$default")"
    case "${value,,}" in
        true|1|yes|y|on) printf 'true' ;;
        *) printf 'false' ;;
    esac
}

# Return 0 if the top-level JSON object has the given key.
json_has_key() {
    local json="$1" key="$2"

    if command -v jq >/dev/null 2>&1; then
        jq -e --arg key "$key" 'has($key)' >/dev/null 2>&1 <<<"$json"
        return
    fi

    if command -v python3 >/dev/null 2>&1; then
        KEY="$key" python3 -c '
import sys, json, os
d = json.load(sys.stdin)
sys.exit(0 if os.environ["KEY"] in d else 1)
' <<<"$json"
        return
    fi

    # Fallback removed: jq or python3 is required to safely detect keys.
    die "json_has_key: jq or python3 is required to parse tool arguments"
}
