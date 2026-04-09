#!/bin/sh

set -eu

echo "Building release binaries..."
cargo build --release

killall browser-server 2>/dev/null || true

echo "Installing binaries to /usr/local/bin..."
sudo install -m 755 target/release/tcode target/release/browser-server /usr/local/bin/

echo "Installing shared library to /usr/local/lib..."
if [ "$(uname -s)" = "Darwin" ]; then
  sudo install -m 644 target/release/libtree-sitter-tcode.dylib /usr/local/lib/
else
  sudo install -m 644 target/release/libtree-sitter-tcode.so /usr/local/lib/
fi

echo "Done."
