# syntax=docker/dockerfile:1.7
# MonitorLab hub image: web console + hub + bundled agents (Linux static musl, Windows x64).
# Build:  docker build -t monitorlab-hub .
# Behind a registry mirror: docker build --build-arg REGISTRY=mirror.gcr.io/library -t monitorlab-hub .
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
# Short commit SHA shown in the UI footer ("build abc1234"); set by CI.
ARG GIT_SHA=""
ENV MONITORLAB_BUILD=${GIT_SHA}
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
    cargo build --release --locked -p monitorlab-hub \
 && cargo build --release --locked -p monitorlab-agent --target x86_64-unknown-linux-musl \
 && cargo build --release --locked -p monitorlab-agent --target x86_64-pc-windows-gnu \
 && mkdir -p /out/agents \
 && cp target/release/monitorlab-hub /out/monitorlab-hub \
 && cp target/x86_64-unknown-linux-musl/release/monitorlab-agent /out/agents/monitorlab-agent-linux-amd64 \
 && cp target/x86_64-pc-windows-gnu/release/monitorlab-agent.exe /out/agents/monitorlab-agent-windows-amd64.exe

# ---------------------------------------------------------------- runtime
FROM ${REGISTRY}/debian:bookworm-slim
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates tzdata \
 && rm -rf /var/lib/apt/lists/*
COPY --from=build /out/monitorlab-hub /usr/local/bin/monitorlab-hub
COPY --from=build /out/agents /opt/monitorlab/agents
# macOS agents are built by CI on a Mac runner and dropped into extra-agents/ (empty for local builds).
COPY extra-agents/ /opt/monitorlab/agents/
ENV MONITORLAB_DATA_DIR=/data \
    MONITORLAB_LISTEN=0.0.0.0:8443 \
    MONITORLAB_AGENTS_DIR=/opt/monitorlab/agents
VOLUME ["/data"]
EXPOSE 8443
HEALTHCHECK --interval=30s --timeout=8s --start-period=30s --start-interval=2s --retries=3 \
  CMD ["/usr/local/bin/monitorlab-hub", "healthcheck"]
STOPSIGNAL SIGTERM
ENTRYPOINT ["/usr/local/bin/monitorlab-hub"]
