#!/bin/sh
set -eu

cd "$(dirname "$0")"

usage() {
  printf '%s\n' \
    'Usage: ./build-docker.sh [docker|podman] [build arguments...]' \
    'Without an engine, auto-detect Docker first, then Podman.' \
    'Example: ./build-docker.sh docker --build-arg TCODE_UID=1001 --build-arg TCODE_GID=1001'
}

ENGINE=""
case "${1:-}" in
  docker|podman)
    ENGINE="$1"
    shift
    ;;
  -h|--help)
    usage
    exit 0
    ;;
  ""|-*)
    ;;
  *)
    printf 'Error: unsupported container engine: %s\n' "$1" >&2
    usage >&2
    exit 2
    ;;
esac

if [ -z "$ENGINE" ]; then
  if command -v docker >/dev/null 2>&1; then
    ENGINE="docker"
  elif command -v podman >/dev/null 2>&1; then
    ENGINE="podman"
  else
    printf '%s\n' 'Error: neither Docker nor Podman is installed.' >&2
    exit 1
  fi
elif ! command -v "$ENGINE" >/dev/null 2>&1; then
  printf 'Error: %s is not installed.\n' "$ENGINE" >&2
  exit 1
fi

IMAGE="tcode-remote"
STATUS="$(git status --porcelain)"

if [ -z "$STATUS" ]; then
  TAG="$(git rev-parse --short HEAD)"
else
  TAG="$(date -u +%Y-%m-%d-%s)"
fi

IMAGE_TAG="${IMAGE}:${TAG}"

printf 'Building %s with %s\n' "$IMAGE_TAG" "$ENGINE"
"$ENGINE" build "$@" --build-arg "GIT_HASH=${TAG}" -t "${IMAGE_TAG}" -t "${IMAGE}:latest" .
printf 'Built %s and %s:latest\n' "$IMAGE_TAG" "$IMAGE"
