#!/bin/sh
set -eu

# --- Detect OS ---
OS_RAW="$(uname -s)"
case "$OS_RAW" in
  Linux)  OS="linux" ;;
  Darwin) OS="darwin" ;;
  *)
    echo "Error: Unsupported OS: $OS_RAW" >&2
    exit 1
    ;;
esac

# --- Detect architecture ---
ARCH_RAW="$(uname -m)"
case "$ARCH_RAW" in
  x86_64)       ARCH="x86_64" ;;
  arm64|aarch64) ARCH="aarch64" ;;
  *)
    echo "Error: Unsupported architecture: $ARCH_RAW" >&2
    exit 1
    ;;
esac

echo "Detected OS: $OS, Arch: $ARCH"

# --- Map to target triple ---
case "$OS" in
  linux)  TARGET="${ARCH}-unknown-linux-gnu" ;;
  darwin) TARGET="${ARCH}-apple-darwin" ;;
esac

echo "Target triple: $TARGET"

# --- Determine version ---
if [ -n "${VERSION:-}" ]; then
  case "$VERSION" in
    v*) ;;
    *)
      echo "Error: VERSION must start with 'v' (got '$VERSION')" >&2
      exit 1
      ;;
  esac
else
  echo "Fetching latest release version..."
  VERSION="$(curl -fsSL https://api.github.com/repos/wb14123/llm-rs/releases/latest \
    | grep '"tag_name"' \
    | sed 's/.*"tag_name": *"\([^"]*\)".*/\1/')"
  if [ -z "$VERSION" ]; then
    echo "Error: Failed to determine latest version" >&2
    exit 1
  fi
fi

echo "Version: $VERSION"

# --- Create temp dir with cleanup trap ---
WORK_DIR="$(mktemp -d)"
trap 'rm -rf "$WORK_DIR"' EXIT

# --- Download ---
TARBALL="tcode-${VERSION}-${TARGET}.tar.gz"
URL="https://github.com/wb14123/llm-rs/releases/download/${VERSION}/${TARBALL}"

echo "Downloading ${TARBALL}..."
curl -fSL -o "${WORK_DIR}/${TARBALL}" "$URL"

# --- Extract ---
echo "Extracting..."
tar xzf "${WORK_DIR}/${TARBALL}" -C "$WORK_DIR"

# --- Determine shared library name ---
case "$OS" in
  linux)  SHLIB="libtree-sitter-tcode.so" ;;
  darwin) SHLIB="libtree-sitter-tcode.dylib" ;;
esac

# --- Install binaries ---
echo "Installing binaries to /usr/local/bin/..."
sudo install -m 755 "${WORK_DIR}/tcode" /usr/local/bin/
sudo install -m 755 "${WORK_DIR}/browser-server" /usr/local/bin/

# --- Install shared library ---
echo "Installing ${SHLIB} to /usr/local/lib/..."
sudo install -m 644 "${WORK_DIR}/${SHLIB}" /usr/local/lib/

# --- Done ---
echo ""
echo "Successfully installed tcode ${VERSION}:"
echo "  tcode          -> /usr/local/bin/tcode"
echo "  browser-server -> /usr/local/bin/browser-server"
echo "  ${SHLIB} -> /usr/local/lib/${SHLIB}"
