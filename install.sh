#!/bin/sh

set -e
set -x

cargo build --release
sudo cp target/release/tcode /usr/bin
sudo cp target/release/libtree-sitter-tcode.so /usr/lib 2>/dev/null ||
  sudo cp target/release/libtree-sitter-tcode.dylib /usr/lib 2>/dev/null || true
killall browser-server || echo ""
sudo cp target/release/browser-server /usr/bin

