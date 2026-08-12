#!/usr/bin/env bash
# Build the Rust workspace of KirkForge-Cli.
#
# Usage:
#   scripts/build-all.sh           # debug build
#   scripts/build-all.sh --release # release build
#
# Produces:
#   - target/<profile>/kf-code
#
# WO 29.9: the Node SDK build path (--node/--test) was removed when the
# npm/kf-plugin TS tree was deleted. The Rust binary is now the sole
# build artifact.

set -euo pipefail

PROFILE="debug"

while [[ $# -gt 0 ]]; do
    case "$1" in
        --release)
            PROFILE="release"
            shift
            ;;
        --rust)
            shift
            ;;
        --node|--test)
            echo "scripts/build-all.sh: '$1' no longer applies (Node SDK deleted, WO 29.9)" >&2
            shift
            ;;
        --help|-h)
            sed -n '2,17p' "$0"
            exit 0
            ;;
        *)
            echo "unknown argument: $1" >&2
            exit 1
            ;;
    esac
done

if [ "$PROFILE" = "release" ]; then
    echo "==> Building Rust workspace (release)"
    cargo build --workspace --release --locked
else
    echo "==> Building Rust workspace (debug)"
    cargo build --workspace --locked
fi

echo "==> Done"
