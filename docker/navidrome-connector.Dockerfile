# syntax=docker/dockerfile:1.7

FROM rust:1.97.1-bookworm AS rust-base

WORKDIR /workspace

COPY rust-toolchain.toml ./

FROM rust-base AS dev

RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    cargo install cargo-watch --locked

WORKDIR /workspace/apps/navidrome-connector

CMD ["cargo", "watch", "--poll", "--why", "-w", ".", "-x", "run"]

FROM rust-base AS builder

COPY apps/navidrome-connector/Cargo.toml apps/navidrome-connector/Cargo.lock ./apps/navidrome-connector/
COPY apps/navidrome-connector/src ./apps/navidrome-connector/src

RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    cargo build --release --locked --manifest-path apps/navidrome-connector/Cargo.toml

FROM debian:bookworm-slim AS runtime

COPY --from=builder /etc/ssl/certs/ca-certificates.crt /etc/ssl/certs/ca-certificates.crt
COPY --from=builder /workspace/apps/navidrome-connector/target/release/navidrome-connector /usr/local/bin/navidrome-connector

ENV SSL_CERT_FILE=/etc/ssl/certs/ca-certificates.crt \
    WHIO_ND_CONNECTOR_BIND_ADDRESS=0.0.0.0:3001

EXPOSE 3001

USER 65532:65532

ENTRYPOINT ["/usr/local/bin/navidrome-connector"]
