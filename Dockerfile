# syntax=docker/dockerfile:1.7
# Vigil hub image: web console + hub + bundled agents (Linux static musl, Windows x64).
# Build:  docker build -t vigil-hub .
# Behind a registry mirror: docker build --build-arg REGISTRY=mirror.gcr.io/library -t vigil-hub .
ARG REGISTRY=docker.io/library

# ---------------------------------------------------------------- web console
FROM ${REGISTRY}/node:22-bookworm-slim AS web
WORKDIR /src/web
COPY web/package.json web/package-lock.json ./
RUN npm ci --no-audit --no-fund
COPY web/ ./
RUN npm run build

# ---------------------------------------------------------------- rust binaries
FROM ${REGISTRY}/rust:1-bookworm AS build
RUN apt-get update \
 && apt-get install -y --no-install-recommends musl-tools gcc-mingw-w64-x86-64 \
 && rm -rf /var/lib/apt/lists/* \
 && rustup target add x86_64-unknown-linux-musl x86_64-pc-windows-gnu
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
COPY deploy ./deploy
COPY --from=web /src/web/dist ./web/dist
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --release --locked -p vigil-hub \
 && cargo build --release --locked -p vigil-agent --target x86_64-unknown-linux-musl \
 && cargo build --release --locked -p vigil-agent --target x86_64-pc-windows-gnu \
 && mkdir -p /out/agents \
 && cp target/release/vigil-hub /out/vigil-hub \
 && cp target/x86_64-unknown-linux-musl/release/vigil-agent /out/agents/vigil-agent-linux-amd64 \
 && cp target/x86_64-pc-windows-gnu/release/vigil-agent.exe /out/agents/vigil-agent-windows-amd64.exe

# ---------------------------------------------------------------- runtime
FROM ${REGISTRY}/debian:bookworm-slim
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates tzdata \
 && rm -rf /var/lib/apt/lists/*
COPY --from=build /out/vigil-hub /usr/local/bin/vigil-hub
COPY --from=build /out/agents /opt/vigil/agents
ENV VIGIL_DATA_DIR=/data \
    VIGIL_LISTEN=0.0.0.0:8443 \
    VIGIL_AGENTS_DIR=/opt/vigil/agents
VOLUME ["/data"]
EXPOSE 8443
STOPSIGNAL SIGTERM
ENTRYPOINT ["/usr/local/bin/vigil-hub"]
