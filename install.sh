#/bin/sh

set -e
set -x

cargo build --release
sudo cp target/release/tcode /usr/bin
killall browser-server || echo ""
sudo cp target/release/browser-server /usr/bin

# Copy default shortcuts config if not already present
mkdir -p ~/.tcode
if [ ! -f ~/.tcode/shortcuts.lua ]; then
  cp tcode/config/shortcuts.lua ~/.tcode/shortcuts.lua
  echo "Copied default shortcuts.lua to ~/.tcode/"
fi
