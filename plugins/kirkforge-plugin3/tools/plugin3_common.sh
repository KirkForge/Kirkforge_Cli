#!/usr/bin/env bash
set -euo pipefail

# plugin3_common.sh — shared helpers for KirkForge-Plugin3 plugin tools.
# Sourced by the tool scripts; not invoked directly.

# Read KIRKFORGE_TOOL_ARGS_JSON or fall back to the first positional arg.
# The host always sets KIRKFORGE_TOOL_ARGS_JSON to a valid JSON object.
tool_args() {
    if [[ -n "${KIRKFORGE_TOOL_ARGS_JSON:-}" ]]; then
        printf '%s' "$KIRKFORGE_TOOL_ARGS_JSON"
    elif [[ $# -gt 0 ]]; then
        printf '%s' "$1"
    else
        printf '{}'
    fi
}

# Locate the plugin3 binary.
# 1. Same directory as this script (plugin/tools/)
# 2. plugin root relative: <plugin_root>/../target/release/plugin3
#    (plugin lives inside the repo, one level below workspace root)
# 3. PATH
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

# Print JSON error and exit non-zero.
die_json() {
    local msg="$1"
    if command -v jq > /dev/null 2>&1; then
        jq -n --arg msg "$msg" '{"error":$msg}' >&2
    elif command -v python3 > /dev/null 2>&1; then
        python3 -c 'import json,sys; print(json.dumps({"error":sys.argv[1]}))' "$msg" >&2
    else
        # Minimal escaping for systems without jq/python3.
        msg="${msg//\\/\\\\}"
        msg="${msg//\"/\\\"}"
        msg="${msg//$'\n'/\\n}"
        printf '{"error":"%s"}\n' "$msg" >&2
    fi
    exit 1
}

# Extract a top-level string value from a JSON object. Supports jq, python3,
# or a pure-bash fallback for flat string/number/boolean values.
json_get_string() {
    local json="$1" key="$2" default="${3:-}"
    local value

    if command -v jq > /dev/null 2>&1; then
        value="$(printf '%s' "$json" | jq -r --arg key "$key" --arg default "$default" '.[$key] // $default')"
        printf '%s' "$value"
        return 0
    fi

    if command -v python3 > /dev/null 2>&1; then
        value="$(printf '%s' "$json" | KEY="$key" DEFAULT="$default" python3 -c 'import sys,json,os; d=json.load(sys.stdin); k=os.environ["KEY"]; v=d.get(k, os.environ["DEFAULT"]); print(v if v is not None else os.environ["DEFAULT"])')"
        printf '%s' "$value"
        return 0
    fi

    # Pure-bash fallback: naive, works for flat values without escaped quotes.
    if [[ "$json" =~ \"${key}\":[[:space:]]*\"([^\"]+)\" ]]; then
        printf '%s' "${BASH_REMATCH[1]}"
        return 0
    fi
    if [[ "$json" =~ \"${key}\":[[:space:]]*(true|false|[0-9]+\.?[0-9]*) ]]; then
        printf '%s' "${BASH_REMATCH[1]}"
        return 0
    fi
    printf '%s' "$default"
}

# Extract a top-level integer value from a JSON object.
json_get_integer() {
    local json="$1" key="$2" default="${3:-0}"
    local value
    value="$(json_get_string "$json" "$key" "$default")"
    value="${value%.*}"
    if [[ "$value" =~ ^-?[0-9]+$ ]]; then
        printf '%s' "$value"
    else
        printf '%s' "$default"
    fi
}
