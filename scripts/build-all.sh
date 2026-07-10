#!/usr/bin/env bash
# Build every component of the unified KirkForge workspace.
#
# Usage:
#   scripts/build-all.sh           # debug build
#   scripts/build-all.sh --release # release build
#   scripts/build-all.sh --node    # build Node SDK only
#   scripts/build-all.sh --rust    # build Rust workspace only
#
# Produces:
#   - target/<profile>/kirkforge
#   - target/<profile>/kfd
#   - target/<profile>/kirkforge-video
#   - target/<profile>/stratum
#   - target/<profile>/plugin3
#   - npm/kirkforge-plugin/apps/cli/dist/index.js

set -euo pipefail

PROFILE="debug"
BUILD_RUST=true
BUILD_NODE=true

while [[ $# -gt 0 ]]; do
    case "$1" in
        --release)
            PROFILE="release"
            shift
            ;;
        --rust)
            BUILD_RUST=true
            BUILD_NODE=false
            shift
            ;;
        --node)
            BUILD_RUST=false
            BUILD_NODE=true
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

ROOT="$(cd "$(dirname "$0")/.." && pwd)"

if [ "$BUILD_RUST" = true ]; then
    if [ "$PROFILE" = "release" ]; then
        echo "==> Building Rust workspace (release)"
        cargo build --workspace --release --locked
    else
        echo "==> Building Rust workspace (debug)"
        cargo build --workspace --locked
    fi
fi

if [ "$BUILD_NODE" = true ]; then
    echo "==> Building Node SDK"
    cd "$ROOT/npm/kirkforge-plugin"
    if [ ! -d node_modules ]; then
        npm ci
    fi
    npm run build
fi

echo "==> Done"
