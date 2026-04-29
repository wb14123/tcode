# syntax=docker/dockerfile:1

FROM rust:1-bookworm AS builder

COPY --from=node:20-bookworm /usr/local/ /usr/local/

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        build-essential \
        ca-certificates \
        cmake \
        git \
        pkg-config \
        python3 \
    && rm -rf /var/lib/apt/lists/*

RUN cargo install tree-sitter-cli --version 0.25.10 --locked

WORKDIR /app

ARG GIT_HASH=unknown
ENV GIT_HASH=${GIT_HASH}

COPY tcode-web/frontend/package.json tcode-web/frontend/package-lock.json ./tcode-web/frontend/
RUN cd tcode-web/frontend && npm ci

COPY . .

RUN cd tcode-web/frontend && npm run build
RUN cargo build --locked --release -p tcode -p browser-server --features tcode/bundled-frontend

FROM debian:bookworm-slim AS runtime

ARG TCODE_UID=1000
ARG TCODE_GID=1000

ENV HOME=/home/tcode \
    CHROME=/usr/bin/chromium

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        ca-certificates \
        chromium \
        fonts-liberation \
        fonts-noto-color-emoji \
        tini \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd --gid "${TCODE_GID}" tcode \
    && useradd \
        --uid "${TCODE_UID}" \
        --gid "${TCODE_GID}" \
        --create-home \
        --home-dir /home/tcode \
        --shell /usr/sbin/nologin \
        tcode \
    && mkdir -p /home/tcode/.tcode \
    && chown -R tcode:tcode /home/tcode

COPY --from=builder /app/target/release/tcode /usr/local/bin/tcode
COPY --from=builder /app/target/release/browser-server /usr/local/bin/browser-server
COPY --from=builder /app/target/release/libtree-sitter-tcode.so /usr/local/lib/libtree-sitter-tcode.so

RUN chmod 0755 /usr/local/bin/tcode /usr/local/bin/browser-server \
    && ldconfig

USER tcode
WORKDIR /home/tcode

EXPOSE 8080

ENTRYPOINT ["/usr/bin/tini", "--", "tcode", "remote", "--web-only"]
CMD ["--host", "0.0.0.0", "--port", "8080"]
