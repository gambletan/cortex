# r/rust post

**Title:**
```
Cortex: a 3.8MB pure-Rust memory engine for AI agents — HNSW, SQLite, CRDT sync, WASM build
```

**Body:**

---

Sharing a project that turned into a fun systems-Rust playground: **Cortex**, a local-first long-term memory engine for AI agents.

The Rust-interesting bits:

- **One core crate, many targets.** `cortex-core` compiles to a native 3.8 MB binary, an `axum` HTTP server, an MCP (Model Context Protocol) server, a `pyo3` Python extension, *and* a 124 KB WASM bundle that runs the full engine in a browser tab.
- **Vector search.** Incremental HNSW index, int8 quantization (75% storage cut), with FTS5 + materialized column indexes in SQLite for the lexical/structured side.
- **CRDT-ish sync without a server.** Append-only oplogs per device in a shared cloud folder, Hybrid Logical Clocks for ordering, LWW per entity, beliefs merge as add-only observation sets. No two devices ever touch the same file, so the cloud provider never sees a conflict.
- **Crypto.** AES-256-GCM with Argon2id KDF, `zeroize` for wiping key material.
- **Perf.** ~156µs ingest (with on-write fact extraction), ~568µs search; `rayon` for parallel decay, `Arc`-shared embeddings, generation-based cache invalidation. 489+ tests.

Zero runtime deps in the final binary. MIT licensed.

Demo (WASM, no install): https://gambletan.github.io/cortex/
Repo: https://github.com/gambletan/cortex

Happy to talk through any of the design decisions — the single-core-many-frontends setup and the serverless CRDT sync were the two trickiest parts to get right. Critiques on the HNSW/quantization choices very welcome.
