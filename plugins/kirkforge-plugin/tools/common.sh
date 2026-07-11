#!/usr/bin/env bash
set -euo pipefail

# common.sh — shared helpers for the KirkForge-Plugin filesystem plugin.
# Sourced by the tool scripts; not invoked directly.

# Locate the KirkForge CLI entry point.
# 1. Merged repo layout: <plugin_root>/../../npm/kirkforge-plugin/apps/cli/dist/index.js
# 2. Installed layout: <plugin_root>/apps/cli/dist/index.js
# 3. Source layout: <plugin_root>/../../apps/cli/dist/index.js
#    (plugin/ lives inside the repo, two levels below workspace root)
# 4. Built executable on PATH via `kirkforge` or `kirkforge-plugin`
find_cli() {
    local script_dir
    script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
    local candidates=(
        # Merged repo layout: plugin lives under plugins/kirkforge-plugin/tools,
        # repo root is three levels above, then npm/kirkforge-plugin/apps/cli/dist.
        "${script_dir}/../../../npm/kirkforge-plugin/apps/cli/dist/index.js"
        # Installed layout: plugin_root/apps/cli/dist/index.js
        "${script_dir}/../apps/cli/dist/index.js"
        # Legacy standalone layout: plugin/ sits inside the repo, two levels below workspace root.
        "${script_dir}/../../apps/cli/dist/index.js"
    )
    for c in "${candidates[@]}"; do
        if [ -f "$c" ]; then
            printf '%s' "$c"
            return 0
        fi
    done
    # Intentionally no PATH fallback: the callers execute the result with
    # `node <path>`, so returning the Rust `kirkforge` binary (or any
    # non-JS executable) would cause Node to fail parsing an ELF file.
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
