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

case "$os" in
    mingw*|msys*|cygwin*)
        echo "Windows native install is not supported by this shell script." >&2
        echo "Download the release .zip for x86_64-pc-windows-msvc and extract it manually." >&2
        exit 1
        ;;
esac

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

# Verify the archive against the release checksum file before extracting.
# The release workflow publishes SHA256SUMS.txt alongside every archive;
# refusing to install when it is missing or the hash mismatches guards
# against a tampered or truncated download.
sums_url="https://github.com/$REPO/releases/download/$tag/SHA256SUMS.txt"
curl -fsSL "$sums_url" -o "$tmpdir/SHA256SUMS.txt"

# shasum is native on macOS; sha256sum is the Linux norm. Pick whichever exists.
if command -v shasum >/dev/null 2>&1; then
    actual=$(shasum -a 256 "$tmpdir/$archive" | awk '{print $1}')
elif command -v sha256sum >/dev/null 2>&1; then
    actual=$(sha256sum "$tmpdir/$archive" | awk '{print $1}')
else
    echo "Need 'shasum' or 'sha256sum' to verify the download; neither found." >&2
    exit 1
fi

# sha256sum lines look like "<hash>  <file>" (text) or "<hash> *<file>" (binary).
expected=$(awk -v f="$archive" '{ gsub(/^\*/, "", $2); if ($2 == f) print $1 }' "$tmpdir/SHA256SUMS.txt")
if [ -z "$expected" ]; then
    echo "No checksum entry for $archive in SHA256SUMS.txt — refusing to install." >&2
    exit 1
fi
if [ "$actual" != "$expected" ]; then
    echo "Checksum mismatch for $archive — refusing to install." >&2
    echo "  expected: $expected" >&2
    echo "  actual:   $actual" >&2
    exit 1
fi
echo "Verified checksum for $archive."

tar -xzf "$tmpdir/$archive" -C "$tmpdir"

mkdir -p "$BIN_DIR"
for bin in kirkforge kfd plugin3 stratum kirkforge-video; do
    cp "$tmpdir/$bin" "$BIN_DIR/$bin"
    chmod +x "$BIN_DIR/$bin"
done

DATA_DIR="${DATA_DIR:-$PREFIX/share/kirkforge}"
PLUGIN_DIR="$DATA_DIR/plugins"
mkdir -p "$PLUGIN_DIR"
if [ -d "$tmpdir/plugins" ]; then
    for plugin in "$tmpdir"/plugins/*/; do
        [ -d "$plugin" ] || continue
        name="$(basename "$plugin")"
        rm -rf "$PLUGIN_DIR/$name"
        cp -R "$plugin" "$PLUGIN_DIR/$name"
    done
    echo "Installed bundled plugins to $PLUGIN_DIR"
fi

if [ -d "$tmpdir/npm" ]; then
    NPM_DIR="$DATA_DIR/npm"
    rm -rf "$NPM_DIR"
    cp -R -P "$tmpdir/npm" "$NPM_DIR"
    echo "Installed bundled Node SDK to $NPM_DIR"

    if ! command -v node >/dev/null 2>&1; then
        echo "Warning: node was not found on PATH. The kirkforge-plugin Node SDK tools require Node.js (>=20)." >&2
        echo "Install Node.js and ensure 'node' is on PATH before using those tools." >&2
    else
        node_version=$(node --version 2>/dev/null | sed 's/^v//')
        case "$node_version" in
            [0-9]*.[0-9]*.[0-9]*) ;;
            *) node_version="" ;;
        esac
        if [ -n "$node_version" ]; then
            major=$(echo "$node_version" | cut -d. -f1)
            if [ "$major" -lt 20 ] 2>/dev/null; then
                echo "Warning: Node.js $node_version is installed, but the bundled Node SDK requires Node >=20." >&2
            fi
        fi
    fi
fi

echo "Installed binaries to $BIN_DIR: kirkforge kfd plugin3 stratum kirkforge-video"

if ! echo "$PATH" | tr ':' '\n' | grep -qx "$BIN_DIR"; then
    echo "Warning: $BIN_DIR is not on your PATH. Add it to your shell profile:"
    echo "  export PATH=\"$BIN_DIR:\$PATH\""
fi
