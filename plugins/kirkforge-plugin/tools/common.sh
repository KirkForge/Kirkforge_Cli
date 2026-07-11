#!/usr/bin/env bash
set -euo pipefail

# common.sh — shared helpers for the KirkForge-Plugin filesystem plugin.
# Sourced by the tool scripts; not invoked directly.

# Locate the KirkForge CLI entry point (the bundled Node SDK).
#
# Release and installed layout:
#   <data-dir>/plugins/kirkforge-plugin/tools/this-script
#   -> <data-dir>/npm/kirkforge-plugin/apps/cli/dist/index.js
#
# Merged repo / source layout:
#   <repo-root>/plugins/kirkforge-plugin/tools/this-script
#   -> <repo-root>/npm/kirkforge-plugin/apps/cli/dist/index.js
#
# There is no PATH fallback: callers execute the result with
# `node <path>`, so returning the Rust `kirkforge` binary (or any
# non-JS executable) would cause Node to fail parsing an ELF file.
find_cli() {
    local script_dir
    script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
    local candidate="${script_dir}/../../../npm/kirkforge-plugin/apps/cli/dist/index.js"
    if [ -f "$candidate" ]; then
        printf '%s' "$candidate"
        return 0
    fi
    return 1
}

# Verify that Node.js is available; tools in this plugin execute the CLI via node.
require_node() {
    command -v node >/dev/null 2>&1 || die "node is required but not installed"
}

# Print usage error and exit non-zero.
die() {
    printf '%s\n' "$1" >&2
    exit 1
}
