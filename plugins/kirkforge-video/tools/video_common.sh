#!/usr/bin/env bash
set -euo pipefail

# video_common.sh — shared helpers for KirkForge-Video plugin tools.
# Sourced by the tool scripts; not invoked directly.

# Read KIRKFORGE_TOOL_ARGS or fall back to first arg.
tool_args() {
    if [[ -n "${KIRKFORGE_TOOL_ARGS:-}" ]]; then
        printf '%s' "$KIRKFORGE_TOOL_ARGS"
    elif [[ $# -gt 0 ]]; then
        printf '%s' "$1"
    else
        printf '{}'
    fi
}

# Locate the kirkforge-video binary.
# 1. Same directory as this script (plugin/tools/)
# 2. plugin root relative: <plugin_root>/../../target/release/kirkforge-video
#    (plugin lives inside the repo, two levels below workspace root)
# 3. PATH
find_video_bin() {
    local script_dir
    script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
    # When run from the source repo, plugin/tools/ is two levels below the workspace root.
    # When installed to ~/.local/share/kirkforge/plugins/kirkforge-video/, the binary must be
    # on PATH or next to the script (copied by the user).
    local candidates=(
        "$script_dir/kirkforge-video"
        "$script_dir/../../../target/release/kirkforge-video"
        "$script_dir/../../../target/debug/kirkforge-video"
        "$(command -v kirkforge-video 2>/dev/null || true)"
    )
    for c in "${candidates[@]}"; do
        if [[ -n "$c" && -x "$c" ]]; then
            printf '%s' "$c"
            return 0
        fi
    done
    return 1
}

# Resolve a path relative to the current working directory.
resolve_path() {
    local p="$1"
    if [[ "$p" =~ ^/ ]]; then
        printf '%s' "$p"
    else
        printf '%s/%s' "$PWD" "$p"
    fi
}

# Print JSON error and exit non-zero.
die_json() {
    local msg="$1"
    printf '{"error":"%s"}\n' "$msg" >&2
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
        value="$(printf '%s' "$json" | python3 -c "import sys,json; d=json.load(sys.stdin); print(d.get('${key}', '${default}'))")"
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

# Extract a top-level boolean value as "true"/"false".
json_get_bool() {
    local json="$1" key="$2" default="${3:-false}"
    local raw
    raw="$(json_get_string "$json" "$key" "$default")"
    case "${raw,,}" in
        true|1|yes) printf 'true' ;;
        *)          printf 'false' ;;
    esac
}

# Extract a space-separated list from a top-level string array.
json_get_string_array() {
    local json="$1" key="$2"

    if command -v jq >/dev/null 2>&1; then
        printf '%s' "$json" | jq -r "[.${key}[]?] | join(\" \")"
        return 0
    fi

    if command -v python3 >/dev/null 2>&1; then
        printf '%s' "$json" | python3 -c "import sys,json; d=json.load(sys.stdin); print(' '.join(d.get('${key}', [])))"
        return 0
    fi

    # Fallback: extract quoted strings between [ and ].
    if [[ "$json" =~ \"${key}\":[[:space:]]*\[([^\]]*)\] ]]; then
        local inner="${BASH_REMATCH[1]}"
        # Remove quotes and commas, keep spaces between values.
        inner="${inner//\"/}"
        inner="${inner//,/ }"
        printf '%s' "$inner"
        return 0
    fi
}
