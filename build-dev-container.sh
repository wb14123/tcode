#!/usr/bin/env bash
set -Eeuo pipefail

ROOT="$(dirname -- "$(readlink -f -- "${BASH_SOURCE[0]}")")"
readonly ROOT

podman --remote=false build \
    --file "$ROOT/Dockerfile.dev" \
    --tag localhost/tcode-dev:dev \
    "$ROOT"

printf 'Next, run on the host: %q\n' "$ROOT/run-dev-container.sh"
