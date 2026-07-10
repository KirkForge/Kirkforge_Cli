#!/usr/bin/env bash
set -euo pipefail

# common.sh — shared helpers for the KirkForge-Draw filesystem plugin.
# Sourced by the tool scripts; not invoked directly.

# Locate the kfd binary.
# 1. Built executable in the workspace target directory
# 2. Installed executable on PATH
find_kfd() {
    local script_dir
    script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
    local candidates=(
        "${script_dir}/../../../target/release/kfd"
        "${script_dir}/../../../target/debug/kfd"
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
