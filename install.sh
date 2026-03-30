#/bin/sh

set -e
set -x

cargo build --release
sudo cp target/release/tcode /usr/bin
killall browser-server || echo ""
sudo cp target/release/browser-server /usr/bin
