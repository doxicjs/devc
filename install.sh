#!/bin/bash
set -e

REPO="doxicjs/devc"
BINARY_NAME="devc"
INSTALL_DIR="/usr/local/bin"

echo "Installing $BINARY_NAME..."

# Detect OS and architecture
OS=$(uname -s)
ARCH=$(uname -m)

case "$OS" in
  Darwin)
    case "$ARCH" in
      arm64|aarch64) ASSET="devc-darwin-arm64.tar.gz" ;;
      x86_64)        ASSET="devc-darwin-x86_64.tar.gz" ;;
      *)
        echo "Error: unsupported architecture $ARCH on macOS"
        exit 1
        ;;
    esac
    ;;
  Linux)
    case "$ARCH" in
      aarch64) ASSET="devc-linux-arm64.tar.gz" ;;
      x86_64)  ASSET="devc-linux-x86_64.tar.gz" ;;
      *)
        echo "Error: unsupported architecture $ARCH on Linux"
        exit 1
        ;;
    esac
    ;;
  *)
    echo "Error: unsupported OS $OS"
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
  rm -f "$INSTALL_DIR/$BINARY_NAME"
  cp "$TMP_DIR/$BIN_NAME" "$INSTALL_DIR/$BINARY_NAME"
  chmod +x "$INSTALL_DIR/$BINARY_NAME"
else
  sudo rm -f "$INSTALL_DIR/$BINARY_NAME"
  sudo cp "$TMP_DIR/$BIN_NAME" "$INSTALL_DIR/$BINARY_NAME"
  sudo chmod +x "$INSTALL_DIR/$BINARY_NAME"
fi

# macOS: remove quarantine attribute
if [ "$(uname -s)" = "Darwin" ]; then
  if [ -w "$INSTALL_DIR" ]; then
    xattr -d com.apple.quarantine "$INSTALL_DIR/$BINARY_NAME" 2>/dev/null || true
  else
    sudo xattr -d com.apple.quarantine "$INSTALL_DIR/$BINARY_NAME" 2>/dev/null || true
  fi
fi

echo ""
echo "  devc installed to $INSTALL_DIR/$BINARY_NAME"
echo ""
echo "  Usage: place a devc.toml in your project root, then run:"
echo "    devc"
echo ""
