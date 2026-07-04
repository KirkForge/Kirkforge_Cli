#!/bin/sh
# Install the latest kirkforge release binary to ~/.local/bin.
# Usage: curl -fsSL https://raw.githubusercontent.com/KirkForge/Kirkforge_Cli/main/scripts/install.sh | sh

set -eu

REPO="KirkForge/Kirkforge_Cli"
PREFIX="${PREFIX:-$HOME/.local}"
BIN_DIR="$PREFIX/bin"

# Detect target triple.
os=$(uname -s | tr '[:upper:]' '[:lower:]')
arch=$(uname -m)
case "$arch" in
    x86_64) target="x86_64-unknown-linux-gnu" ;;
    aarch64|arm64)
        if [ "$os" = "darwin" ]; then
            target="aarch64-apple-darwin"
        else
            target="aarch64-unknown-linux-gnu"
        fi
        ;;
    *)
        echo "Unsupported architecture: $arch" >&2
        exit 1
        ;;
esac

if [ "$os" = "darwin" ] && [ "$arch" = "x86_64" ]; then
    target="x86_64-apple-darwin"
fi

# Fetch latest release tag.
tag=$(curl -fsSL "https://api.github.com/repos/$REPO/releases/latest" | sed -n 's/.*"tag_name": "\([^"]*\)".*/\1/p')
if [ -z "$tag" ]; then
    echo "Failed to determine latest release tag" >&2
    exit 1
fi

archive="kirkforge-$target.tar.gz"
url="https://github.com/$REPO/releases/download/$tag/$archive"

tmpdir=$(mktemp -d)
trap 'rm -rf "$tmpdir"' EXIT

echo "Downloading kirkforge $tag for $target..."
curl -fsSL "$url" -o "$tmpdir/$archive"
tar -xzf "$tmpdir/$archive" -C "$tmpdir"

mkdir -p "$BIN_DIR"
cp "$tmpdir/kirkforge" "$BIN_DIR/kirkforge"
chmod +x "$BIN_DIR/kirkforge"

echo "Installed kirkforge to $BIN_DIR/kirkforge"

if ! echo "$PATH" | tr ':' '\n' | grep -qx "$BIN_DIR"; then
    echo "Warning: $BIN_DIR is not on your PATH. Add it to your shell profile:"
    echo "  export PATH=\"$BIN_DIR:\$PATH\""
fi
