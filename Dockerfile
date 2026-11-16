# syntax=docker/dockerfile:1.7
# Headless iloader: iloader-server + web UI in one small image.
# See README-SERVER.md ("Docker").

# --- server binary -----------------------------------------------------------
FROM rust:1-bookworm AS server
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY crates crates
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --release --locked -p iloader-server \
 && cp target/release/iloader-server /iloader-server

# --- web UI ------------------------------------------------------------------
FROM oven/bun:1.4.4 AS web
WORKDIR /app
COPY package.json bun.lock ./
RUN bun install --frozen-lockfile
COPY index.html tsconfig.json tsconfig.node.json vite.config.ts ./
COPY public public
COPY src src
RUN bun run build

# --- runtime -----------------------------------------------------------------
FROM debian:bookworm-slim
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates \
 && rm -rf /var/lib/apt/lists/* \
 && useradd --system --uid 10001 --user-group --home-dir /data --shell /usr/sbin/nologin iloader \
 && install -d -o iloader -g iloader -m 0700 /data /cache
COPY --from=server /iloader-server /usr/local/bin/iloader-server
COPY --from=web /app/dist /app/dist

ENV ILOADER_BIND=0.0.0.0:8080 \
    ILOADER_DATA_DIR=/data \
    ILOADER_TEMP_DIR=/cache \
    ILOADER_STATIC_DIR=/app/dist \
    ILOADER_IN_CONTAINER=1

USER iloader
VOLUME ["/data", "/cache"]
EXPOSE 8080
CMD ["iloader-server"]
