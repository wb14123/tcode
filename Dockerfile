# The tree-sitter CLI needs Trixie's newer glibc. Only static assets and C
# sources cross into the Rust builder; builder and runtime stay on Bookworm.
FROM node:24-trixie-slim AS assets

WORKDIR /build/tcode-web/frontend
COPY tcode-web/frontend/package.json tcode-web/frontend/package-lock.json ./
RUN npm ci
COPY tcode-web/frontend/ ./
RUN npm run build

# Generate the C parser where tcode/build.rs expects it. Pin the CLI rather
# than depending on whichever version npm happens to publish most recently.
RUN npm install --global tree-sitter-cli@0.26.9
WORKDIR /build/tree-sitter-tcode
COPY tree-sitter-tcode/grammar.js ./
RUN tree-sitter generate -o src/generated grammar.js

# Locked AWS SDK crates require Rust >= 1.94.1.
FROM rust:1.95.0-slim-bookworm AS builder

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        build-essential \
        ca-certificates \
        cmake \
        pkg-config \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /build
COPY Cargo.toml Cargo.lock ./
COPY auth/ ./auth/
COPY browser-server/ ./browser-server/
COPY llm-rs/ ./llm-rs/
COPY llm-rs-macros/ ./llm-rs-macros/
COPY lsp-client/ ./lsp-client/
COPY tcode/ ./tcode/
COPY tcode-encoding/ ./tcode-encoding/
COPY tcode-runtime/ ./tcode-runtime/
COPY tcode-web/Cargo.toml ./tcode-web/Cargo.toml
COPY tcode-web/src/ ./tcode-web/src/
COPY tools/ ./tools/
COPY tree-sitter-tcode/src/scanner.c ./tree-sitter-tcode/src/scanner.c
COPY tree-sitter-tcode/queries/ ./tree-sitter-tcode/queries/
COPY --from=assets /build/tree-sitter-tcode/ ./tree-sitter-tcode/
COPY --from=assets /build/tcode-web/frontend/dist/ ./tcode-web/frontend/dist/

# Deliberately use the debug profile, not release. Omit debug symbols and
# incremental caches to reduce build storage and the installed binary sizes.
ENV CARGO_PROFILE_DEV_DEBUG=0 \
    CARGO_INCREMENTAL=0

# .git is excluded from the context. build.rs only overrides GIT_HASH when
# git produces a hash, so supply it directly to rustc through Cargo's env.
ARG GIT_HASH=unknown
RUN GIT_HASH="${GIT_HASH}" cargo build --locked \
    --package tcode --bin tcode \
    --package browser-server --bin browser-server \
    --features tcode/bundled-frontend

FROM debian:bookworm-slim AS runtime

# Use distro Chromium and its sandbox unchanged; do not disable sandboxing.
RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        ca-certificates \
        chromium \
        chromium-sandbox \
        fonts-liberation \
        fonts-noto-color-emoji \
    && rm -rf /var/lib/apt/lists/*

# Docker bind mounts can match the host IDs at build time. Rootless Podman
# can keep these defaults and use :U on an exclusive copy of the state directory.
ARG TCODE_UID=1000
ARG TCODE_GID=1000
RUN test "${TCODE_UID}" -gt 0 \
    && test "${TCODE_GID}" -gt 0 \
    && groupadd --gid "${TCODE_GID}" tcode \
    && useradd --uid "${TCODE_UID}" --gid tcode \
        --create-home --home-dir /home/tcode --shell /bin/bash tcode \
    && install -d -m 0700 -o tcode -g tcode \
        /home/tcode/.tcode /home/tcode/.tcode/chrome

# browser-server must be a sibling of tcode. Include the generated parser
# library in the same installation layout as install-from-source.sh.
COPY --from=builder /build/target/debug/tcode /build/target/debug/browser-server /usr/local/bin/
COPY --from=builder /build/target/debug/libtree-sitter-tcode.so /usr/local/lib/
COPY scripts/docker-entrypoint.sh /usr/local/bin/docker-entrypoint.sh
RUN chmod 0755 /usr/local/bin/docker-entrypoint.sh

ENV HOME=/home/tcode \
    CHROME=/usr/bin/chromium
WORKDIR /home/tcode
USER tcode

# Mount writable state and a separate, writable COPY of any browser profile
# at /home/tcode/.tcode and /home/tcode/.tcode/chrome, respectively.
EXPOSE 8080
STOPSIGNAL SIGINT
ENTRYPOINT ["/usr/local/bin/docker-entrypoint.sh"]
CMD ["remote", "--host", "0.0.0.0", "--port", "8080"]
