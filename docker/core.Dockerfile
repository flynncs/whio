# syntax=docker/dockerfile:1.7

FROM rust:1.97.1-bookworm AS rust-base

WORKDIR /workspace

COPY rust-toolchain.toml ./

FROM rust-base AS dev

RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    cargo install cargo-watch --locked

WORKDIR /workspace/apps/core

CMD ["cargo", "watch", "--poll", "--why", "-w", ".", "-w", "../resolver-client-generated", "-x", "run"]

FROM rust-base AS builder

COPY apps/core/Cargo.toml apps/core/Cargo.lock ./apps/core/
COPY apps/core/src ./apps/core/src
COPY apps/resolver-client-generated/Cargo.toml ./apps/resolver-client-generated/
COPY apps/resolver-client-generated/src ./apps/resolver-client-generated/src

RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    cargo build --release --locked --manifest-path apps/core/Cargo.toml --bins

FROM debian:bookworm-slim AS runtime

COPY --from=builder /etc/ssl/certs/ca-certificates.crt /etc/ssl/certs/ca-certificates.crt
COPY --from=builder /workspace/apps/core/target/release/whio-api /usr/local/bin/whio-api
COPY --from=builder /workspace/apps/core/target/release/whio-cli /usr/local/bin/whio-cli

ENV SSL_CERT_FILE=/etc/ssl/certs/ca-certificates.crt \
    WHIO_BIND_ADDRESS=0.0.0.0:3000

EXPOSE 3000

USER 65532:65532

ENTRYPOINT ["/usr/local/bin/whio-api"]
