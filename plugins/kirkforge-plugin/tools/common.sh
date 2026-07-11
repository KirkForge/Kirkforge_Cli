#!/usr/bin/env bash
set -euo pipefail

# common.sh — shared helpers for the KirkForge-Plugin filesystem plugin.
# Sourced by the tool scripts; not invoked directly.


# Locate the KirkForge CLI entry point (the bundled Node SDK).
#
# Resolution order:
#   1. $KIRKFORGE_CLI_JS override (useful for custom installs and CI).
#   2. Same-directory / source-layout candidate:
#      <plugin-root>/../npm/kirkforge-plugin/apps/cli/dist/index.js
#   3. PATH-installed `kirkforge` command (global npm bin or other package
#      manager symlink), which is typically a JS script with a shebang.
#   4. Global npm install:
#      $(npm root -g)/@kirkforge/cli/dist/index.js
#
# Callers execute the result with `node <path>`; the resolved file must be a
# JavaScript file (or a shebang JS wrapper). A native ELF binary would cause
# Node to fail parsing it.
find_cli() {
    if [[ -n "${KIRKFORGE_CLI_JS:-}" && -f "$KIRKFORGE_CLI_JS" ]]; then
        printf '%s' "$KIRKFORGE_CLI_JS"
        return 0
    fi

    local script_dir
    script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
    local candidate="${script_dir}/../../../npm/kirkforge-plugin/apps/cli/dist/index.js"
    if [ -f "$candidate" ]; then
        printf '%s' "$candidate"
        return 0
    fi

    candidate="$(command -v kirkforge 2>/dev/null || true)"
    if [ -n "$candidate" ] && [ -f "$candidate" ]; then
        printf '%s' "$candidate"
        return 0
    fi

    if command -v npm >/dev/null 2>&1; then
        candidate="$(npm root -g 2>/dev/null)/@kirkforge/cli/dist/index.js"
        if [ -f "$candidate" ]; then
            printf '%s' "$candidate"
            return 0
        fi
    fi

    return 1
}

# Extract a scalar value from KIRKFORGE_TOOL_ARGS_JSON, defaulting on missing,
# null, or empty. Invalid JSON is reported as a tool error and the script exits.
node_json_arg() {
    local key="$1" default="${2:-}"
    node -e '
        const [key, defaultValue] = process.argv.slice(1);
        const raw = process.env.KIRKFORGE_TOOL_ARGS_JSON || "{}";
        try {
            const a = JSON.parse(raw);
            const v = a[key];
            if (v === undefined || v === null || v === "") {
                console.log(defaultValue);
            } else {
                console.log(String(v));
            }
        } catch (e) {
            console.error(JSON.stringify({ error: "invalid KIRKFORGE_TOOL_ARGS_JSON" }));
            process.exit(1);
        }
    ' "$key" "$default" || exit 1
}

# Extract the `file` argument, which may be a single path (string) or a list of
# paths (array). Emits one path per line so callers can read it safely.
node_json_file_arg() {
    node -e '
        const raw = process.env.KIRKFORGE_TOOL_ARGS_JSON || "{}";
        try {
            const a = JSON.parse(raw);
            const v = a.file;
            if (v === undefined || v === null) {
                // no files
            } else if (Array.isArray(v)) {
                v.forEach(x => console.log(String(x)));
            } else {
                console.log(String(v));
            }
        } catch (e) {
            console.error(JSON.stringify({ error: "invalid KIRKFORGE_TOOL_ARGS_JSON" }));
            process.exit(1);
        }
    ' || exit 1
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
