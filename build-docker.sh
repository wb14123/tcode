#!/bin/sh
set -eu

cd "$(dirname "$0")"

IMAGE="tcode-remote"

if [ -z "$(git status --porcelain)" ]; then
  TAG="$(git rev-parse --short HEAD)"
else
  TAG="$(date -u +%Y-%m-%d-%s)"
fi

IMAGE_TAG="${IMAGE}:${TAG}"

echo "Building ${IMAGE_TAG}"
docker build --build-arg "GIT_HASH=${TAG}" -t "${IMAGE_TAG}" .
echo "Built ${IMAGE_TAG}"
