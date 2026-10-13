#!/usr/bin/env bash
set -euo pipefail

if [[ $(podman --remote=false info --format '{{.Host.Security.Rootless}}') != true ]]; then
    printf 'Error: local rootless Podman is required; rootful execution is unsafe.\n' >&2
    exit 1
fi

podman --remote=false rm --ignore tcode-dev

podman --remote=false run --rm --pull=never --userns=host --user=0:0 \
    --network=none --image-volume=ignore --http-proxy=false --env-host=false \
    -v tcode-dev-deps:/var/cache/tcode:rw,nocopy \
    -v tcode-dev-builds:/var/build/tcode:rw,nocopy \
    localhost/tcode-dev:dev mkdir -p /var/cache/tcode/node_modules \
    /var/build/tcode/rust-target /var/build/tcode/dist /var/build/tcode/generated

podman --remote=false run --rm -it --name tcode-dev \
    --userns=host --user=0:0 --workdir /home/wangbin/hobbi_source/tcode \
    --env HOME=/home/wangbin --env CARGO_TARGET_DIR=/home/wangbin/hobbi_source/tcode/target \
    --init --pull=never --image-volume=ignore --http-proxy=false \
    --env-host=false --network=bridge \
    -v /home/wangbin/hobbi_source/tcode:/home/wangbin/hobbi_source/tcode:rw \
    -v /home/wangbin/.gitconfig:/home/wangbin/.gitconfig:ro \
    -v /home/wangbin/.global_ignore:/home/wangbin/.global_ignore:ro \
    -v tcode-dev-deps:/var/cache/tcode:rw,nocopy \
    -v tcode-dev-builds:/var/build/tcode:rw,nocopy \
    --mount type=volume,src=tcode-dev-builds,dst=/home/wangbin/hobbi_source/tcode/target,subpath=rust-target \
    --mount type=volume,src=tcode-dev-deps,dst=/home/wangbin/hobbi_source/tcode/tcode-web/frontend/node_modules,subpath=node_modules \
    --mount type=volume,src=tcode-dev-builds,dst=/home/wangbin/hobbi_source/tcode/tcode-web/frontend/dist,subpath=dist \
    --mount type=volume,src=tcode-dev-builds,dst=/home/wangbin/hobbi_source/tcode/tree-sitter-tcode/src/generated,subpath=generated \
    localhost/tcode-dev:dev bash -ec '
    cargo fetch --locked
    npm --prefix /home/wangbin/hobbi_source/tcode/tcode-web/frontend ci --include=dev
    (cd -- /home/wangbin/hobbi_source/tcode/tree-sitter-tcode; tree-sitter generate -o src/generated grammar.js)

    printf "%s\n" \
        "Runtime: local rootless Podman" \
        "Container: tcode-dev" \
        "Source (rw): /home/wangbin/hobbi_source/tcode" \
        "HOME (ephemeral): /home/wangbin" \
        "Host Git configuration (ro): /home/wangbin/.gitconfig" \
        "Host Git ignore file (ro): /home/wangbin/.global_ignore" \
        "Named volumes (whole mounts rw,nocopy):" \
        "  tcode-dev-deps -> /var/cache/tcode (Cargo/npm downloads)" \
        "  tcode-dev-deps/node_modules -> /home/wangbin/hobbi_source/tcode/tcode-web/frontend/node_modules" \
        "  tcode-dev-builds -> /var/build/tcode" \
        "  tcode-dev-builds/rust-target -> /home/wangbin/hobbi_source/tcode/target" \
        "  tcode-dev-builds/dist -> /home/wangbin/hobbi_source/tcode/tcode-web/frontend/dist" \
        "  tcode-dev-builds/generated -> /home/wangbin/hobbi_source/tcode/tree-sitter-tcode/src/generated" \
        "" \
        "Run tcode from another host terminal:" \
        "cd -- /home/wangbin/hobbi_source/tcode" \
        "tcode -c tcode-dev --container-runtime podman" \
        "" \
        "To use this container by default, run tcode project-config-path on the host" \
        "from the project root. Edit config.toml in the printed directory," \
        "creating the directory/file if needed and preserving existing settings." \
        "Add or update these top-level keys before any [section]:" \
        "container = \"tcode-dev\"" \
        "container_runtime = \"podman\"" \
        "Then start a fresh tcode session from the same host directory without flags."
    exec bash -i
'
