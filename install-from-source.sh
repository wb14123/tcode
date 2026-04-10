#!/bin/sh
set -eu

usage() {
  cat <<'EOF'
Usage: install-from-source.sh [--user] [-h|--help]

Build tcode from source and install it.

Options:
  --user        Install to $HOME/.local/{bin,lib} (no sudo).
                Without this flag, installs to /usr/local/{bin,lib}
                and uses sudo when not running as root.
  -h, --help    Show this help message and exit.
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
    ./install-from-source.sh --user
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

echo "Building release binaries..."
cargo build --release

killall browser-server 2>/dev/null || true

run mkdir -p "$BIN_DIR" "$LIB_DIR"

echo "Installing binaries to $BIN_DIR..."
run install -m 755 target/release/tcode target/release/browser-server "$BIN_DIR/"

echo "Installing shared library to $LIB_DIR..."
if [ "$(uname -s)" = "Darwin" ]; then
  run install -m 644 target/release/libtree-sitter-tcode.dylib "$LIB_DIR/"
else
  run install -m 644 target/release/libtree-sitter-tcode.so "$LIB_DIR/"
fi

echo "Done."
echo ""
echo "Installed to:"
echo "  tcode          -> $BIN_DIR/tcode"
echo "  browser-server -> $BIN_DIR/browser-server"
if [ "$(uname -s)" = "Darwin" ]; then
  echo "  libtree-sitter-tcode.dylib -> $LIB_DIR/libtree-sitter-tcode.dylib"
else
  echo "  libtree-sitter-tcode.so    -> $LIB_DIR/libtree-sitter-tcode.so"
fi

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
