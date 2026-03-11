# Multi-stage build for cortex-http REST API

# ── Stage 1: Build ───────────────────────────────────────────────────────────
FROM rust:1.82-bookworm AS builder

WORKDIR /src

# Install build deps for fastembed/ONNX
RUN apt-get update && apt-get install -y cmake pkg-config && rm -rf /var/lib/apt/lists/*

# Copy workspace
COPY Cargo.toml Cargo.lock ./
COPY cortex-core/ cortex-core/
COPY cortex-http/ cortex-http/
COPY cortex-mcp-server/ cortex-mcp-server/

# Stub out cortex-python so workspace resolves without Python deps
RUN mkdir -p cortex-python/src && \
    printf '[package]\nname = "cortex-python"\nversion = "0.1.0"\nedition = "2021"\n\n[lib]\ncrate-type = ["cdylib"]\npath = "src/lib.rs"' > cortex-python/Cargo.toml && \
    echo '' > cortex-python/src/lib.rs

# Build release binary
RUN cargo build --release -p cortex-http

# ── Stage 2: Runtime ─────────────────────────────────────────────────────────
FROM debian:bookworm-slim

RUN apt-get update && \
    apt-get install -y --no-install-recommends ca-certificates && \
    rm -rf /var/lib/apt/lists/*

COPY --from=builder /src/target/release/cortex-http /usr/local/bin/cortex-http

# Data volume for SQLite persistence
VOLUME /data
ENV CORTEX_DB_PATH=/data/memory.db
ENV CORTEX_HOST=0.0.0.0
ENV CORTEX_PORT=3315

EXPOSE 3315

ENTRYPOINT ["cortex-http"]
