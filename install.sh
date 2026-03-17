#!/bin/bash
set -e

INSTALL_DIR="/usr/local/bin"
BINARY_NAME="devc"

# Check if running from repo with pre-built binary
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
RELEASE_BIN="$SCRIPT_DIR/target/release/$BINARY_NAME"

if [ ! -f "$RELEASE_BIN" ]; then
    echo "Building release binary..."
    if ! command -v cargo &> /dev/null; then
        echo "Error: cargo not found. Install Rust or use a pre-built binary."
        exit 1
    fi
    cd "$SCRIPT_DIR"
    cargo build --release
fi

echo "Installing $BINARY_NAME to $INSTALL_DIR..."
sudo cp "$RELEASE_BIN" "$INSTALL_DIR/$BINARY_NAME"
sudo chmod +x "$INSTALL_DIR/$BINARY_NAME"

echo "Installed $(devc --version 2>/dev/null || echo "$BINARY_NAME") to $INSTALL_DIR/$BINARY_NAME"
echo ""
echo "Usage: place a devc.toml in your project root, then run: devc"
