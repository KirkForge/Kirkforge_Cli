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

# Print usage error and exit non-zero.
die() {
    printf '%s\n' "$1" >&2
    exit 1
}
