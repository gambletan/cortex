# cortex-memory

**Persistent memory engine for AI agents.** Local-first, sub-millisecond, zero cloud.

Native Python binding for [Cortex](https://github.com/gambletan/cortex) — a Rust memory engine with 4-tier memory, Bayesian beliefs, people graph, and HNSW vector search.

## Install

```bash
pip install cortex-ai-memory
```

## Quick Start

```python
from cortex_python import PyCortex

# Open or create a memory database
cx = PyCortex("memory.db")

# Ingest memories
cx.ingest("Met Alice at the Q3 planning meeting", "slack", user_id="alice_123")
cx.ingest("User prefers dark mode", "cli")

# Retrieve relevant memories
results = cx.retrieve("What do I know about Alice?", limit=5)
for memory_id, score, text in results:
    print(f"[{score:.2f}] {text}")

# Generate LLM-ready context (token-budgeted)
context = cx.get_context(2000, channel="slack")

# Structured knowledge
cx.add_fact("Alice", "works_at", "Acme Corp", 0.95, "slack")
cx.add_preference("timezone", "Asia/Shanghai", 0.9)

# Bayesian beliefs
cx.observe_belief("user_likes_python", True, 0.8)
beliefs = cx.get_beliefs(0.5)

# People graph
cx.add_person("Alice", "slack", "alice_123")

# Consolidation (run periodically)
scanned, promoted, swept, patterns = cx.run_consolidation()
```

## With Embeddings

For semantic search, pass embeddings from any provider:

```python
import numpy as np

# Use any embedding model (OpenAI, ollama, sentence-transformers, etc.)
def embed(text):
    # your embedding function here
    ...

cx.ingest("I live in Shanghai", "cli", embedding=embed("I live in Shanghai"))
results = cx.retrieve("where do I live?", 5, embedding=embed("where do I live?"))
```

## Features

- **4-tier memory**: Working, Episodic, Semantic, Procedural
- **HNSW vector search**: Sub-millisecond at 100K+ memories
- **Bayesian beliefs**: Self-correcting with evidence
- **People graph**: Cross-channel identity resolution
- **Conversation compression**: Automatic session summarization
- **Contradiction detection**: Catches conflicting facts
- **Chinese + English**: Native bilingual NLP
- **Zero cloud**: 100% local, your data stays on your device
- **3.8MB binary**: Pure Rust, zero runtime dependencies

## License

MIT
