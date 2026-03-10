FROM rust:1.82-slim AS builder

WORKDIR /build
COPY . .

RUN apt-get update && apt-get install -y pkg-config libssl-dev && rm -rf /var/lib/apt/lists/*
RUN cargo build --release -p cortex-http -p cortex-mcp-server

FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y ca-certificates && rm -rf /var/lib/apt/lists/*

COPY --from=builder /build/target/release/cortex-http /usr/local/bin/cortex-http
COPY --from=builder /build/target/release/cortex-mcp-server /usr/local/bin/cortex-mcp-server

ENV CORTEX_HOST=0.0.0.0
ENV CORTEX_PORT=3315
ENV CORTEX_DB_PATH=/data/memory.db

VOLUME ["/data"]
EXPOSE 3315

CMD ["cortex-http", "--host", "0.0.0.0", "--port", "3315", "--db", "/data/memory.db"]
