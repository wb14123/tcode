#!/bin/sh
# Run the tcode.lua test suites through headless nvim, without cargo.
# Usage: ./scripts/test-tcode-lua.sh
# Set TCODE_NVIM to override the nvim binary.
set -eu

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
REPO_ROOT=$(dirname -- "$SCRIPT_DIR")
NVIM_BIN=${TCODE_NVIM:-nvim}

if ! command -v "$NVIM_BIN" >/dev/null 2>&1; then
  echo "error: nvim not found on PATH (set TCODE_NVIM to the nvim binary)" >&2
  exit 1
fi

exec "$NVIM_BIN" --headless -l \
  "$REPO_ROOT/tcode/lua/tests/runner.lua" \
  "$REPO_ROOT/tcode/lua/tcode.lua" \
  "$REPO_ROOT"/tcode/lua/tests/*_tests.lua
