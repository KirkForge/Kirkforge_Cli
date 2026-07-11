#!/usr/bin/env bash
set -euo pipefail

# common.sh — shared helpers for the KirkForge-Draw filesystem plugin.
# Sourced by the tool scripts; not invoked directly.

# Locate the kfd binary.
# 1. Same directory as this script (installed layout may copy the binary here)
# 2. Built executable in the workspace target directory
# 3. Installed executable on PATH
find_kfd() {
    local script_dir
    script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
    local candidates=(
        "${script_dir}/kfd"
        "${script_dir}/kfd.exe"
        "${script_dir}/../../../target/release/kfd"
        "${script_dir}/../../../target/release/kfd.exe"
        "${script_dir}/../../../target/debug/kfd"
        "${script_dir}/../../../target/debug/kfd.exe"
        "$(command -v kfd 2>/dev/null || true)"
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

# Extract a top-level string value from a JSON object. Supports jq, python3,
# or a pure-bash fallback for flat string values.
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
    printf '%s' "$default"
}
