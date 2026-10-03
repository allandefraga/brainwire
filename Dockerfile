FROM rust:1.98.1-bookworm AS builder

RUN apt-get update && apt-get install -y --no-install-recommends \
    clang libclang-dev cmake pkg-config && \
    rm -rf /var/lib/apt/lists/*

WORKDIR /src
COPY Cargo.toml Cargo.lock rust-toolchain.toml ./
COPY crates ./crates
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target,sharing=locked \
    cargo build --release --locked -p brainwire-server && \
    cp target/release/brainwire /src/brainwire

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates libstdc++6 && \
    rm -rf /var/lib/apt/lists/* && \
    groupadd --gid 10001 brainwire && \
    useradd --uid 10001 --gid brainwire --home-dir /nonexistent --no-create-home brainwire && \
    mkdir /data && chown brainwire:brainwire /data

COPY --from=builder /src/brainwire /usr/local/bin/brainwire
COPY crates/core/tests/fixtures/graph_mapping.json /opt/brainwire/example-schema.json

ENV BRAINWIRE_SERVER_DATA_DIR=/data \
    BRAINWIRE_SERVER_LISTEN=0.0.0.0:7687
EXPOSE 7687
USER brainwire
HEALTHCHECK --interval=10s --timeout=5s --start-period=180s --retries=3 \
    CMD ["/usr/local/bin/brainwire", "probe", "--mode", "ready"]
ENTRYPOINT ["/usr/local/bin/brainwire"]
CMD ["serve"]
