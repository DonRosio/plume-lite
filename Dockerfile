# syntax=docker/dockerfile:1
# API server image (the desktop client is distributed as a native binary).
#   docker build -t plume-lite-server .
#   docker compose up -d --build

FROM rust:1.99-slim-trixie AS build
WORKDIR /src
COPY . .
# Cache mounts keep the registry and build artefacts between builds.
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --release --locked -p plume-lite-server \
 && cp target/release/plume-lite-server /usr/local/bin/plume-lite-server

FROM debian:trixie-slim
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates \
 && rm -rf /var/lib/apt/lists/* \
 && useradd --system --uid 10001 --no-create-home plume \
 && mkdir /data && chown plume /data
COPY --from=build /usr/local/bin/plume-lite-server /usr/local/bin/plume-lite-server
# The Windows client handed out at /download/ (put there by .github/workflows/release.yml;
# empty in a local build). Owned by root: the server can serve it, not change it.
COPY downloads/ /app/downloads/
# Readable by the server's user whatever the permissions they were built with.
RUN rm -f /app/downloads/.gitkeep && chmod -R a+rX /app/downloads
USER plume
# The database lives in /data: mount a volume there (docker-compose.yml does).
VOLUME /data
ENV BIND_ADDR=0.0.0.0:8080 \
    DATABASE_PATH=/data/plume-lite.db \
    DOWNLOADS_DIR=/app/downloads \
    RUST_LOG=plume_lite_server=info,tower_http=info
EXPOSE 8080
# Probes: GET /health (liveness) and GET /health/ready (database reachable).
ENTRYPOINT ["/usr/local/bin/plume-lite-server"]
