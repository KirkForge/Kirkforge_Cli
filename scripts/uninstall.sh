#!/bin/sh
# Uninstall kf-code. Removes the binary; optionally removes config.
# Usage: sh scripts/uninstall.sh
set -eu

PREFIX="${PREFIX:-$HOME/.local}"
BIN_PATH="$PREFIX/bin/kf-code"
CONFIG_DIR="${XDG_CONFIG_HOME:-$HOME/.config}/kf-code"
DATA_DIR="${XDG_DATA_HOME:-$HOME/.local/share}/kf-code"

removed=0
if [ -f "$BIN_PATH" ]; then
    rm -f "$BIN_PATH"
    echo "Removed $BIN_PATH"
    removed=1
else
    echo "No binary at $BIN_PATH (already removed?)."
fi

# Root-installed copy lives in /usr/local/bin.
if [ "$(id -u)" -eq 0 ] && [ -f /usr/local/bin/kf-code ]; then
    rm -f /usr/local/bin/kf-code
    echo "Removed /usr/local/bin/kf-code"
    removed=1
fi

if [ "$removed" -eq 1 ]; then
    echo "kf-code uninstalled."
else
    echo "kf-code was not installed."
fi

printf 'Also remove config (%s) and data (%s)? [y/N] ' "$CONFIG_DIR" "$DATA_DIR"
read -r ans
case "$ans" in
    y|Y|yes|YES)
        rm -rf "$CONFIG_DIR" "$DATA_DIR"
        echo "Removed $CONFIG_DIR and $DATA_DIR"
        ;;
    *)
        echo "Left config and data in place."
        ;;
esac