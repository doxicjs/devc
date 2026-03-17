#!/bin/bash
set -e

REPO="doxicjs/devc"
BINARY_NAME="devc"
INSTALL_DIR="/usr/local/bin"

echo "Installing $BINARY_NAME..."

# Detect architecture
ARCH=$(uname -m)
case "$ARCH" in
  arm64|aarch64) ASSET="devc-darwin-arm64.tar.gz" ;;
  x86_64)        ASSET="devc-darwin-x86_64.tar.gz" ;;
  *)
    echo "Error: unsupported architecture $ARCH"
    exit 1
    ;;
esac

# Get latest release download URL
DOWNLOAD_URL=$(curl -fsSL "https://api.github.com/repos/$REPO/releases/latest" \
  | grep "browser_download_url.*$ASSET" \
  | cut -d '"' -f 4)

if [ -z "$DOWNLOAD_URL" ]; then
  echo "Error: could not find release asset $ASSET"
  echo "Check https://github.com/$REPO/releases for available downloads."
  exit 1
fi

# Download and extract to temp dir
TMP_DIR=$(mktemp -d)
trap 'rm -rf "$TMP_DIR"' EXIT

echo "Downloading $ASSET..."
curl -fsSL "$DOWNLOAD_URL" -o "$TMP_DIR/$ASSET"
tar xzf "$TMP_DIR/$ASSET" -C "$TMP_DIR"

# Find the binary (name without extension)
BIN_NAME="${ASSET%.tar.gz}"

# Install
echo "Installing to $INSTALL_DIR/$BINARY_NAME..."
if [ -w "$INSTALL_DIR" ]; then
  cp "$TMP_DIR/$BIN_NAME" "$INSTALL_DIR/$BINARY_NAME"
  chmod +x "$INSTALL_DIR/$BINARY_NAME"
else
  sudo cp "$TMP_DIR/$BIN_NAME" "$INSTALL_DIR/$BINARY_NAME"
  sudo chmod +x "$INSTALL_DIR/$BINARY_NAME"
fi

echo ""
echo "  devc installed to $INSTALL_DIR/$BINARY_NAME"
echo ""
echo "  Usage: place a devc.toml in your project root, then run:"
echo "    devc"
echo ""
