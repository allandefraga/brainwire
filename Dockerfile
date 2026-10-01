FROM rust:1.98.1-bookworm AS builder

RUN apt-get update && apt-get install -y --no-install-recommends \
    clang libclang-dev cmake pkg-config && \
    rm -rf /var/lib/apt/lists/*

WORKDIR /src
COPY Cargo.toml Cargo.lock rust-toolchain.toml ./
COPY crates ./crates
RUN cargo build --release --locked -p brainwire-server

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates libstdc++6 && \
    rm -rf /var/lib/apt/lists/* && \
    groupadd --gid 10001 brainwire && \
    useradd --uid 10001 --gid brainwire --home-dir /nonexistent --no-create-home brainwire && \
    mkdir /data && chown brainwire:brainwire /data

COPY --from=builder /src/target/release/brainwire /usr/local/bin/brainwire
COPY crates/core/tests/fixtures/graph_mapping.json /opt/brainwire/example-schema.json

ENV BRAINWIRE_SERVER_DATA_DIR=/data \
    BRAINWIRE_SERVER_LISTEN=0.0.0.0:7687
EXPOSE 7687
USER brainwire
ENTRYPOINT ["/usr/local/bin/brainwire"]
CMD ["serve"]
