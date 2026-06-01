# How to reproduce the LoCoMo 73.7% benchmark

> Category: **Q&A**

A few people have asked how the **73.7% on LoCoMo** number is produced and how to reproduce it. Here's the full recipe — the harness and data ship in the repo, so you can verify it yourself.

**What LoCoMo is:** a long-conversation memory benchmark ([ACL 2024](https://snap-research.github.io/locomo/)) — multi-session dialogues where the system must answer questions that depend on remembering earlier turns.

**Reproduce it:**
```bash
git clone https://github.com/gambletan/cortex
cd cortex

# Benchmark data lives here:
ls bench/data/locomo10.json

# Run the harness
python3 bench/locomo_bench.py
```

**Notes on methodology:**
- Setup: Claude Sonnet 4 as QA + judge, `nomic-embed-text` embeddings via Ollama, top-30 retrieval.
- Cortex scores **73.7%**, ahead of Mem0 on the same set.
- Results land in `bench/results/`.

If your run differs, post your machine + exact command here and we'll dig in. PRs that improve the harness or add comparison baselines are very welcome.
