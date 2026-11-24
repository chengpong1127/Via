FROM rust:1.98.1-slim-trixie AS builder

RUN apt-get update \
    && apt-get install --yes --no-install-recommends clang lld \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /build/server

COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY config ./config

RUN cargo build --release --locked --bin via-server --bin via-tdx-gateway --bin via-data

FROM debian:trixie-slim AS runtime

RUN apt-get update \
    && apt-get install --yes --no-install-recommends ca-certificates jq \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --uid 10001 --create-home via

WORKDIR /app/server

COPY --from=builder /build/server/target/release/via-server /usr/local/bin/via-server
COPY --from=builder /build/server/target/release/via-tdx-gateway /usr/local/bin/via-tdx-gateway
COPY --from=builder /build/server/target/release/via-data /usr/local/bin/via-data

COPY deploy/aws/artifacts/nightly-build.sh /usr/local/bin/nightly-build.sh
COPY config/via.toml /etc/via/via.toml
ENV VIA_CONFIG=/etc/via/via.toml

USER via
ENTRYPOINT ["/usr/local/bin/via-server"]
