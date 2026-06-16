# Multi-stage build for cortex-http REST API

# ── Stage 1: Build ───────────────────────────────────────────────────────────
FROM rust:1.85-bookworm AS builder

WORKDIR /src

# Install build deps for fastembed/ONNX
RUN apt-get update && apt-get install -y cmake pkg-config && rm -rf /var/lib/apt/lists/*

# Copy workspace
COPY Cargo.toml Cargo.lock ./
COPY cortex-core/ cortex-core/
COPY cortex-http/ cortex-http/
COPY cortex-mcp-server/ cortex-mcp-server/

# Stub out cortex-python and cortex-wasm so workspace resolves without their deps
RUN mkdir -p cortex-python/src && \
    printf '[package]\nname = "cortex-python"\nversion = "0.1.0"\nedition = "2021"\n\n[lib]\ncrate-type = ["cdylib"]\npath = "src/lib.rs"' > cortex-python/Cargo.toml && \
    echo '' > cortex-python/src/lib.rs && \
    mkdir -p cortex-wasm/src && \
    printf '[package]\nname = "cortex-wasm"\nversion = "2.0.0"\nedition = "2021"\n\n[lib]\ncrate-type = ["cdylib"]\npath = "src/lib.rs"' > cortex-wasm/Cargo.toml && \
    echo '' > cortex-wasm/src/lib.rs

# Build release binaries (HTTP server + MCP server)
RUN cargo build --release -p cortex-http -p cortex-mcp-server

# ── Stage 2: Runtime ─────────────────────────────────────────────────────────
FROM debian:bookworm-slim

RUN apt-get update && \
    apt-get install -y --no-install-recommends ca-certificates && \
    rm -rf /var/lib/apt/lists/*

COPY --from=builder /src/target/release/cortex-http /usr/local/bin/cortex-http
COPY --from=builder /src/target/release/cortex-mcp-server /usr/local/bin/cortex-mcp-server

# MCP Registry ownership verification
LABEL io.modelcontextprotocol.server.name="io.github.gambletan/cortex"

# Data volume for SQLite persistence
VOLUME /data
ENV CORTEX_DB_PATH=/data/memory.db
ENV CORTEX_HOST=0.0.0.0
ENV CORTEX_PORT=3315

EXPOSE 3315

# This image's identity is the MCP server (see the LABEL above), so MCP stdio is the
# default entrypoint — `docker run -i image` starts the JSON-RPC server on stdin/stdout
# and responds to `initialize`/`tools/list` introspection out of the box (what MCP
# clients and the Glama registry expect). The DB path comes from CORTEX_DB_PATH.
#
# HTTP REST mode (alternate): docker run -p 3315:3315 --entrypoint cortex-http image
ENTRYPOINT ["cortex-mcp-server"]
