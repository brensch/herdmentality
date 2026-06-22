# Multi-stage, multi-target build for the whole Herdcore stack.
#
# Targets (selected via `build.target` in docker-compose.yml):
#   * server — the axum WebSocket game server
#   * bot    — the bot service that plays CPU seats
#   * web    — Caddy serving the prebuilt Yew/WASM frontend + /ws proxy
#
# The native binaries (server, bot) share a single cargo-chef dependency cache,
# so they only pay for the workspace's dependency compile once.

ARG RUST_VERSION=1.92
ARG TRUNK_VERSION=0.21.14

# ---------------------------------------------------------------------------
# Shared base: Rust toolchain + protoc (prost-build shells out to it) + chef.
# ---------------------------------------------------------------------------
FROM rust:${RUST_VERSION}-bookworm AS chef
RUN apt-get update \
    && apt-get install -y --no-install-recommends protobuf-compiler \
    && rm -rf /var/lib/apt/lists/*
RUN cargo install cargo-chef --locked
WORKDIR /app

# ---------------------------------------------------------------------------
# Plan: capture the dependency graph for both native binaries and the wasm lib.
# ---------------------------------------------------------------------------
FROM chef AS planner
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

# ---------------------------------------------------------------------------
# Native builder: server + bot. Dependencies are cooked first for caching.
# ---------------------------------------------------------------------------
FROM chef AS native-builder
COPY --from=planner /app/recipe.json recipe.json
RUN cargo chef cook --release --recipe-path recipe.json \
    -p herdcore-server -p herdcore-bot
COPY . .
RUN cargo build --release -p herdcore-server -p herdcore-bot

# ---------------------------------------------------------------------------
# Web builder: compile the Yew app to WASM and bundle it with Trunk.
# The app connects same-origin at runtime (see crates/web/src/api.rs), so no
# server URL is baked in — the same bundle works at localhost and at the public
# domain. Set HERDCORE_WS_URL only to force a specific server.
# ---------------------------------------------------------------------------
FROM chef AS web-builder
ARG TRUNK_VERSION
RUN rustup target add wasm32-unknown-unknown \
    && curl -fsSL "https://github.com/trunk-rs/trunk/releases/download/v${TRUNK_VERSION}/trunk-x86_64-unknown-linux-gnu.tar.gz" \
       | tar -xzf - -C /usr/local/bin
COPY . .
# Only expose HERDCORE_WS_URL to the build when actually set: an empty value
# would make option_env!() see Some("") and bake a broken URL.
ARG HERDCORE_WS_URL
RUN if [ -n "$HERDCORE_WS_URL" ]; then export HERDCORE_WS_URL; else unset HERDCORE_WS_URL; fi; \
    trunk build --release

# ---------------------------------------------------------------------------
# server runtime
# ---------------------------------------------------------------------------
FROM debian:bookworm-slim AS server
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --uid 10001 herdcore \
    && mkdir -p /data && chown herdcore /data
COPY --from=native-builder /app/target/release/herdcore-server /usr/local/bin/herdcore-server
USER herdcore
ENV HERDCORE_LISTEN=0.0.0.0:55051 \
    HERDCORE_DB=/data/herdcore.db
EXPOSE 55051
VOLUME ["/data"]
ENTRYPOINT ["herdcore-server"]

# ---------------------------------------------------------------------------
# bot runtime
# ---------------------------------------------------------------------------
FROM debian:bookworm-slim AS bot
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --uid 10001 herdcore
COPY --from=native-builder /app/target/release/herdcore-bot /usr/local/bin/herdcore-bot
USER herdcore
ENTRYPOINT ["herdcore-bot"]

# ---------------------------------------------------------------------------
# web runtime: Caddy serves the static bundle and proxies /ws to the server.
# ---------------------------------------------------------------------------
FROM caddy:2-alpine AS web
COPY docker/Caddyfile /etc/caddy/Caddyfile
COPY --from=web-builder /app/dist /srv
EXPOSE 80
