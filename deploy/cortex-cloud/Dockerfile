# Cortex Cloud for Muse. Build from the repository root:
#   docker build -f deploy/cortex-cloud/Dockerfile -t cortex-cloud .

FROM rust:1.94-bookworm AS builder
WORKDIR /src
RUN apt-get update && apt-get install -y cmake pkg-config && rm -rf /var/lib/apt/lists/*
COPY Cargo.toml Cargo.lock ./
COPY cortex-core/ cortex-core/
COPY cortex-http/ cortex-http/
COPY cortex-mcp-server/ cortex-mcp-server/
COPY cortex-cloud/ cortex-cloud/
# Stub the members this image doesn't build (they need Python / wasm toolchains).
RUN mkdir -p cortex-python/src cortex-wasm/src && \
    printf '[package]\nname = "cortex-python"\nversion = "0.1.0"\nedition = "2021"\n\n[lib]\ncrate-type = ["cdylib"]\npath = "src/lib.rs"' > cortex-python/Cargo.toml && \
    echo '' > cortex-python/src/lib.rs && \
    printf '[package]\nname = "cortex-wasm"\nversion = "2.0.0"\nedition = "2021"\n\n[lib]\ncrate-type = ["cdylib"]\npath = "src/lib.rs"' > cortex-wasm/Cargo.toml && \
    echo '' > cortex-wasm/src/lib.rs
RUN cargo build --release -p cortex-cloud

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && rm -rf /var/lib/apt/lists/*
COPY --from=builder /src/target/release/cortex-cloud /usr/local/bin/cortex-cloud
# Tenant data (never backed up: devices re-push) and the model cache live in /data; the
# master key is mounted separately at /secrets (keep it out of any backup of /data).
ENV FASTEMBED_CACHE_DIR=/data/models
VOLUME /data
ENTRYPOINT ["cortex-cloud", "--data-dir", "/data", "--master-key", "/secrets/master.key"]
