# Show HN post

**Title (≤80 chars, no "Show HN:" emoji, plain):**

```
Show HN: Cortex – local-first encrypted memory for AI agents, in pure Rust
```

**URL field:** https://github.com/gambletan/cortex

**First comment (post immediately after submitting — this is what people read):**

---

Hi HN, I built Cortex because every "memory" layer for AI agents I tried either shipped my personal data to someone's cloud, charged a subscription, or was just a flat text file behind keyword search.

Cortex is a memory engine that runs 100% on your device:

- **Local-first.** SQLite on your disk, in-memory vector index, sub-millisecond ops. Nothing leaves your machine unless you turn on sync.
- **Sync is through YOUR cloud.** iCloud / Google Drive / Dropbox folder — changelog-based, Hybrid Logical Clocks for ordering, AES-256-GCM (Argon2id) so the provider only ever sees ciphertext. No server of mine in the loop.
- **It's structured, not a blob.** 4 memory tiers (working/episodic/semantic/procedural), Bayesian beliefs that self-correct with evidence, a people-graph that resolves identities across channels, contradiction detection, automatic episodic→semantic consolidation.
- **Fast + tiny.** Pure Rust, 3.8 MB binary, zero runtime deps. ~156µs ingest (including on-write fact extraction), ~568µs search. 124 KB WASM build runs the whole thing in a browser tab.
- **Plugs into LLMs via MCP** (29 tools) or a plain REST API. Python SDK too (`pip install cortex-ai-memory`).

On LoCoMo (a long-conversation memory benchmark, ACL 2024) it scores 73.7%, ahead of Mem0 — the harness and data are in the repo so you can reproduce it.

Try it with zero install in the browser: https://gambletan.github.io/cortex/

It's MIT licensed. Honest about limitations: NLP inference rules are currently strongest in English + Chinese, mobile bindings aren't done yet, and the "beats cloud" benchmarks are single-machine `cargo bench` (methodology noted in the README). Happy to answer anything — especially interested in how people are wiring agent memory today and what's missing.
