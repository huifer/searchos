# syntax=docker/dockerfile:1
# Multi-stage build for SearchOS.
#
#   docker build --target gateway . -t ghcr.io/huifer/searchos-gateway:0.1.1
#   docker build --target console . -t ghcr.io/huifer/searchos-console:0.1.1
#   docker compose up --build
#
# Targets:
#   gateway  Debian slim + the release `searchos` binary (Rust, glibc)
#   console  Node 22 + the built Next.js console
#
# The gateway listens on 0.0.0.0:8787 (compose maps it), stores everything
# under SEARCHOS_DATA_DIR=/data (mount a volume there), handles SIGTERM with a
# graceful drain (see docs/api/http.md#graceful-shutdown), and exposes
# GET /health and GET /metrics (Prometheus text format).
#
# MCP is stdio-only (ADR-031): there is deliberately no MCP container.

# --------------------------------------------------------------------------- #
# Rust build
# --------------------------------------------------------------------------- #
# The lockfile requires rustc >= 1.94 (sqlx 0.9, rusqlite 0.41, uuid 1.29,
# tantivy 0.27 all declare a newer rust-version). Keep this in step with the
# repo's `rust-toolchain.toml` (currently stable) and with `cargo build --locked`
# in CI.
ARG RUST_VERSION=1.97
FROM rust:${RUST_VERSION}-slim AS rust-build

# protoc: LanceDB codegen (see docs — `brew install protobuf` locally).
# libprotobuf-dev: the well-known google/protobuf/*.proto includes. It is only
# a Recommends of protobuf-compiler, and `--no-install-recommends` would drop
# it, so LanceDB's build script would fail to resolve
# `import "google/protobuf/empty.proto"`. Installed explicitly.
# build-essential: the bundled SQLite C sources and C-based crates.
RUN apt-get update \
 && apt-get install -y --no-install-recommends \
      build-essential \
      protobuf-compiler \
      libprotobuf-dev \
      pkg-config \
      ca-certificates \
 && rm -rf /var/lib/apt/lists/*

# Debian's protoc does not put /usr/include on its default include path, so the
# well-known google/protobuf/*.proto types fail to resolve for LanceDB's build
# script. `prost-build` reads PROTOC_INCLUDE and adds it to protoc's include
# paths, which is the supported way to point at the well-known types.
ENV PROTOC_INCLUDE=/usr/include

WORKDIR /build

# Manifests first so the dependency layer caches independently of source.
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
COPY tests ./tests

# Optional crates.io mirror, for regions where the default registry is slow or
# unreachable. Pass a full sparse URL, e.g.
#   --build-arg CARGO_REGISTRY=sparse+https://mirrors.ustc.edu.cn/crates.io-index/
# Empty (the default) keeps the upstream registry, so the Dockerfile stays
# portable and reproducible outside a mirror region.
ARG CARGO_REGISTRY=""
RUN if [ -n "${CARGO_REGISTRY}" ]; then \
      CARGO_HOME_DIR="${CARGO_HOME:-/usr/local/cargo}"; \
      mkdir -p "${CARGO_HOME_DIR}"; \
      { \
        echo '[source.crates-io]'; \
        echo 'replace-with = "mirror"'; \
        echo '[source.mirror]'; \
        echo "registry = \"${CARGO_REGISTRY}\""; \
      } > "${CARGO_HOME_DIR}/config.toml"; \
    fi

RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/build/target \
    cargo build --release --locked --bin searchos \
 && cp target/release/searchos /usr/local/bin/searchos

# --------------------------------------------------------------------------- #
# Console build
# --------------------------------------------------------------------------- #
FROM node:22-alpine AS console-build

WORKDIR /app
ENV NEXT_TELEMETRY_DISABLED=1

COPY web/package.json web/package-lock.json ./
RUN npm ci

COPY web ./
RUN npm run build \
 && npm prune --omit=dev

# --------------------------------------------------------------------------- #
# Gateway runtime
# --------------------------------------------------------------------------- #
FROM debian:bookworm-slim AS gateway

# ca-certificates: outbound HTTPS to providers.
# curl:            the container healthcheck.
# tini:            PID 1 that forwards SIGTERM and reaps zombies, so the
#                  gateway's graceful shutdown actually runs.
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates curl tini \
 && rm -rf /var/lib/apt/lists/*

COPY --from=rust-build /usr/local/bin/searchos /usr/local/bin/searchos

# All state (SQLite, Tantivy, LanceDB, objects, extensions) lives under one
# directory so a single volume is enough.
ENV SEARCHOS_DATA_DIR=/data \
    RUST_LOG=info
VOLUME ["/data"]

EXPOSE 8787
STOPSIGNAL SIGTERM
HEALTHCHECK --interval=30s --timeout=3s --start-period=10s --retries=3 \
    CMD ["CMD-SHELL", "curl -fsS http://127.0.0.1:8787/health || exit 1"]

ENTRYPOINT ["/usr/bin/tini", "--"]
CMD ["searchos", "gateway", "start", "--host", "0.0.0.0", "--port", "8787"]

# --------------------------------------------------------------------------- #
# Console runtime
# --------------------------------------------------------------------------- #
FROM node:22-alpine AS console

WORKDIR /app
ENV NODE_ENV=production \
    NEXT_TELEMETRY_DISABLED=1 \
    # The console talks to the gateway through its server-side proxy, so the
    # browser never needs CORS and the token never reaches the client.
    SEARCHOS_GATEWAY_URL=http://gateway:8787

COPY --from=console-build --chown=node:node /app /app
USER node

EXPOSE 3100
STOPSIGNAL SIGTERM
HEALTHCHECK --interval=30s --timeout=3s --start-period=10s --retries=3 \
    CMD ["CMD-SHELL", "node -e \"fetch('http://127.0.0.1:3100/').then(r=>process.exit(r.ok?0:1)).catch(()=>process.exit(1))\""]

CMD ["npm", "run", "start"]
