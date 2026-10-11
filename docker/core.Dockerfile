# syntax=docker/dockerfile:1.7

FROM rust:1.97.1-bookworm AS rust-base

WORKDIR /workspace

COPY rust-toolchain.toml ./

FROM rust-base AS dev

RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    cargo install cargo-watch --locked

WORKDIR /workspace/apps/core

CMD ["cargo", "watch", "--poll", "--why", "-w", ".", "-w", "../resolver-client-generated", "-w", "../subsonic-types-generated", "-x", "run"]

FROM rust-base AS deps

COPY apps/core/Cargo.toml apps/core/Cargo.lock ./apps/core/
COPY apps/resolver-client-generated/Cargo.toml ./apps/resolver-client-generated/
COPY apps/subsonic-types-generated/Cargo.toml ./apps/subsonic-types-generated/

RUN mkdir -p apps/core/src/bin apps/resolver-client-generated/src apps/subsonic-types-generated/src \
    && printf 'fn main() {}\n' > apps/core/src/main.rs \
    && printf 'fn main() {}\n' > apps/core/src/bin/whio-cli.rs \
    && printf 'fn main() {}\n' > apps/core/src/bin/whio-worker.rs \
    && touch apps/core/src/lib.rs apps/resolver-client-generated/src/lib.rs apps/subsonic-types-generated/src/lib.rs

RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    cargo build --release --locked --manifest-path apps/core/Cargo.toml --bins

FROM deps AS builder

COPY apps/core/src ./apps/core/src
COPY apps/core/migrations ./apps/core/migrations
COPY apps/resolver-client-generated/src ./apps/resolver-client-generated/src
COPY apps/subsonic-types-generated/src ./apps/subsonic-types-generated/src

RUN find apps/core/src apps/resolver-client-generated/src apps/subsonic-types-generated/src -type f -exec touch {} +

RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    cargo build --release --locked --manifest-path apps/core/Cargo.toml --bins

FROM debian:bookworm-slim AS runtime

COPY --from=builder /etc/ssl/certs/ca-certificates.crt /etc/ssl/certs/ca-certificates.crt
COPY --from=builder /workspace/apps/core/target/release/whio-api /usr/local/bin/whio-api
COPY --from=builder /workspace/apps/core/target/release/whio-cli /usr/local/bin/whio-cli
COPY --from=builder /workspace/apps/core/target/release/whio-worker /usr/local/bin/whio-worker

ENV SSL_CERT_FILE=/etc/ssl/certs/ca-certificates.crt \
    WHIO_BIND_ADDRESS=0.0.0.0:3000

EXPOSE 3000

USER 65532:65532

ENTRYPOINT ["/usr/local/bin/whio-api"]

FROM python:3.13-slim-bookworm AS importer

RUN apt-get update \
    && apt-get install --no-install-recommends -y ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && pip install --no-cache-dir beets==2.14.1

COPY --from=builder /workspace/apps/core/target/release/whio-worker /usr/local/bin/whio-worker
COPY docker/importer/beets.yaml /etc/whio/beets.yaml

ENV HOME=/tmp \
    SSL_CERT_FILE=/etc/ssl/certs/ca-certificates.crt

USER 65532:65532

ENTRYPOINT ["/usr/local/bin/whio-worker"]
