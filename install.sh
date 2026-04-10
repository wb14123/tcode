#!/bin/sh
set -eu

usage() {
  cat <<'EOF'
Usage: install.sh [--user] [-h|--help]

Download and install the latest tcode release.

Options:
  --user        Install to $HOME/.local/{bin,lib} (no sudo).
                Without this flag, installs to /usr/local/{bin,lib}
                and uses sudo when not running as root.
  -h, --help    Show this help message and exit.

Environment variables:
  VERSION       Install a specific release tag (e.g. VERSION=v0.2.0)
                instead of the latest release.
EOF
}

# --- Parse arguments ---
USER_INSTALL=0
while [ $# -gt 0 ]; do
  case "$1" in
    --user)
      USER_INSTALL=1
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "Error: unknown argument: $1" >&2
      echo "" >&2
      usage >&2
      exit 1
      ;;
  esac
done

# --- Resolve prefix and sudo policy ---
if [ "$USER_INSTALL" = 1 ]; then
  if [ -z "${HOME:-}" ]; then
    echo "Error: --user requires \$HOME to be set" >&2
    exit 1
  fi
  PREFIX="$HOME/.local"
  SUDO=""
else
  PREFIX="/usr/local"
  if [ "$(id -u)" = 0 ]; then
    SUDO=""
  elif command -v sudo >/dev/null 2>&1; then
    SUDO="sudo"
  else
    cat >&2 <<'EOF'
Error: system install to /usr/local requires sudo, which was not found.
Rerun with --user to install into $HOME/.local instead:
    curl -sSL https://raw.githubusercontent.com/wb14123/tcode/refs/heads/master/install.sh | sh -s -- --user
EOF
    exit 1
  fi
fi

BIN_DIR="$PREFIX/bin"
LIB_DIR="$PREFIX/lib"

run() {
  if [ -n "$SUDO" ]; then
    sudo "$@"
  else
    "$@"
  fi
}

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
  VERSION="$(curl -fsSL https://api.github.com/repos/wb14123/tcode/releases/latest \
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
URL="https://github.com/wb14123/tcode/releases/download/${VERSION}/${TARBALL}"

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

# --- Create install dirs ---
run mkdir -p "$BIN_DIR" "$LIB_DIR"

# --- Install binaries ---
echo "Installing binaries to $BIN_DIR/..."
run install -m 755 "${WORK_DIR}/tcode" "$BIN_DIR/"
run install -m 755 "${WORK_DIR}/browser-server" "$BIN_DIR/"

# --- Install shared library ---
echo "Installing ${SHLIB} to $LIB_DIR/..."
run install -m 644 "${WORK_DIR}/${SHLIB}" "$LIB_DIR/"

# --- Done ---
echo ""
echo "Successfully installed tcode ${VERSION}:"
echo "  tcode          -> $BIN_DIR/tcode"
echo "  browser-server -> $BIN_DIR/browser-server"
echo "  ${SHLIB} -> $LIB_DIR/${SHLIB}"

# --- PATH hint (user mode only) ---
# Single-quoted strings below contain literal `$HOME`/`$PATH` text that we
# want to print verbatim as copy-paste hints. Silence SC2016 for this block.
# shellcheck disable=SC2016
if [ "$USER_INSTALL" = 1 ]; then
  case ":${PATH:-}:" in
    *":$BIN_DIR:"*) ;;
    *)
      echo ""
      printf 'Warning: %s is not on your $PATH.\n' "$BIN_DIR"
      shell_name="$(basename "${SHELL:-}")"
      case "$shell_name" in
        bash)
          echo 'Add this line to ~/.bashrc:'
          echo '    export PATH="$HOME/.local/bin:$PATH"'
          ;;
        zsh)
          echo 'Add this line to ~/.zshrc:'
          echo '    export PATH="$HOME/.local/bin:$PATH"'
          ;;
        fish)
          echo 'Run once:'
          echo '    fish_add_path $HOME/.local/bin'
          ;;
        *)
          echo 'Add this line to your shell rc file:'
          echo '    export PATH="$HOME/.local/bin:$PATH"'
          ;;
      esac
      ;;
  esac
fi
