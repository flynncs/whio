FROM rust:1.97.1-bookworm AS compiler
RUN apt-get update && apt-get install --no-install-recommends --yes btrfs-progs ffmpeg && rm -rf /var/lib/apt/lists/*
RUN rustup component add clippy rustfmt
WORKDIR /workspace

FROM debian:bookworm-slim AS runtime
RUN apt-get update && apt-get install --no-install-recommends --yes ca-certificates curl && rm -rf /var/lib/apt/lists/*
ENV SSL_CERT_FILE=/etc/ssl/certs/ca-certificates.crt
