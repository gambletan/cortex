# r/LocalLLaMA post

**Title:**
```
Cortex: a local-first, encrypted long-term memory engine for agents (Rust, 3.8MB, MCP) — beats Mem0 on LoCoMo
```

**Flair:** Resources / Tutorial (or "New Model"→ use "Resources")

**Body:**

---

If you're running local models and want them to actually *remember* across sessions without shipping everything to a cloud memory API, I built something for exactly this.

**Cortex** is a memory engine that stays on your machine:

- 100% local — SQLite + in-memory vector index, sub-ms ops, no network unless you opt into sync
- Optional cross-device sync through **your own** iCloud/GDrive/Dropbox, AES-256-GCM encrypted (provider only sees ciphertext)
- 4 memory tiers, Bayesian self-correcting beliefs, cross-channel people graph, contradiction detection, automatic consolidation
- **MCP server with 29 tools** → drop straight into Claude Desktop, or anything that speaks MCP. Also REST + a Python SDK (`pip install cortex-ai-memory`)
- Pure Rust, 3.8 MB, zero runtime deps. ~156µs ingest / ~568µs search
- **73.7% on LoCoMo** (long-conversation memory benchmark) — ahead of Mem0, repro harness included

Zero-install browser demo (124 KB WASM): https://gambletan.github.io/cortex/
Repo (MIT): https://github.com/gambletan/cortex

Genuinely curious how this community is handling agent memory right now — flat files, RAG over a vector DB, cloud APIs? What breaks for you at scale? The inference layer is strongest in EN/CN today and I'd love pointers on what retrieval signals matter most for your setups.
