# Cortex Memory Tiers

Cortex organizes memory into four tiers. This guide describes what each tier is for, how data flows between them, and how to work with each one through the shipped surfaces (Rust core + the MCP server). The Python SDK is in progress; where it lands it will mirror the core API.

> **TL;DR**
> - **Working** = current session scratch pad, lives in RAM
> - **Episodic** = raw experiences with timestamps, persisted
> - **Semantic** = distilled facts / preferences / relationships, persisted
> - **Procedural** = learned routines and user-specific workflows, persisted
> - The **Consolidation Engine** promotes Episodic → Semantic, decays stale entries, and surfaces Procedural patterns.

---

## Tier overview

| Tier | Lifetime | Stored as | Typical size | Default promotion / decay |
|---|---|---|---|---|
| Working | Single session | RAM (in-memory ring) | KB | Dropped on session end |
| Episodic | Days to weeks | SQLite + vector index | Hundreds to thousands of rows | Promoted to Semantic by Consolidation; stale rows decay |
| Semantic | Indefinite | SQLite + facts/preferences + vector index | Bounded by distilled facts | Updated by Bayesian belief update; never auto-decays |
| Procedural | Indefinite | SQLite | Bounded by distinct routines | Surfaced by Consolidation when a pattern recurs |

## Working memory

### What it is

The scratch pad for the current session — the LLM's "RAM". Holds the recent conversation turns and the current working set, with optional summarization once a token budget is exceeded.

### How to use it

In the **MCP server**, working memory is implicit: you don't write to it directly. You assemble the context the model sees with `memory_context`, which token-budgets the most relevant Working/Episodic/Semantic content:

```text
memory_context({ "max_tokens": 2000 })
```

### When it disappears

At session end. Anything that should survive a session must live in Episodic or Semantic — which is the default for everything you `memory_ingest`.

## Episodic memory

### What it is

Raw experiences with timestamps and source metadata. Each ingest is an episodic memory row, indexed for temporal, lexical (FTS5/BM25), and vector search.

### How to write to it

```text
# MCP server (30 tools)
memory_ingest({
  "text": "Met with Sarah from Stripe about payment integration.",
  "channel": "telegram"
})
```

```rust
// Rust core
cortex.ingest(
    "Met with Sarah from Stripe about payment integration.",
    "telegram",   // channel
    None,         // user_id (optional — triggers identity resolution)
    None,         // salience hint (optional)
    None,         // precomputed embedding (optional; auto-embedded if omitted)
)?;
```

The ingest path also runs **proactive inference**: it extracts facts (e.g. `Sarah → works_at → Stripe`), updates the people graph, and stores the episode. See [integrations.md](integrations.md) for framework-specific setup.

### How to query it

Retrieval fuses vector similarity + BM25 + recency + frecency, and detects temporal intent from the query text itself:

```text
memory_search({ "query": "what did Sarah say about payments", "limit": 5 })
memory_search({ "query": "the first time we talked to Stripe" })   # temporal intent: earliest
memory_search({ "query": "what happened recently" })               # temporal intent: recent
```

```rust
let results = cortex.retrieve("what did Sarah say about payments", 5, None, None, None)?;
```

## Semantic memory

### What it is

Distilled facts, preferences, and relationships, stored as **subject → predicate → object** triples with a confidence score. Facts are updated by Bayesian-style inference — a contradicting observation lowers the old fact's confidence (and supersedes it) rather than deleting it.

### How to write to it

Most facts are **auto-extracted** during episodic ingest (the `Met with Sarah from Stripe` example above yields `Sarah → works_at → Stripe`). You can also assert one directly:

```text
fact_add({ "subject": "User", "predicate": "preferred_language", "object": "Rust", "confidence": 0.92 })
preference_set({ "key": "editor", "value": "neovim", "confidence": 0.9 })
```

```rust
cortex.add_fact("User", "preferred_language", "Rust", 0.92, "cli", None)?;
```

### How to query it

```text
fact_query({ "entity": "Sarah" })          # all facts about an entity
preference_query({ "key": "editor" })
```

```rust
let facts = cortex.query_facts("Sarah")?;   // → Sarah works_at Stripe (confidence …)
```

Because Semantic is confidence-scored, `memory_context` accepts a `min_confidence` floor (default `0.3`) so low-confidence or superseded facts stay queryable but are kept out of the context the LLM sees:

```text
memory_context({ "max_tokens": 2000, "min_confidence": 0.5 })
```

## Procedural memory

### What it is

Learned routines and user-specific workflows. Cortex's consolidation observes recurring patterns and surfaces them over time. Procedural memory is currently **managed automatically** — it is populated by consolidation, not by a manual "assert procedure" tool — so there's no public write API to document yet; it's an internal tier that the engine maintains.

## Consolidation: how the tiers move data

The **Consolidation Engine** runs automatically (by default every N ingests) and can be triggered manually. It performs:

| Operation | Direction | Trigger |
|---|---|---|
| **Promotion** | Episodic → Semantic | A pattern recurs across episodes |
| **Decay** | Episodic → faded | An episode is old and not re-referenced (importance-aware) |
| **Pattern surfacing** | Episodic → Procedural | A recurring routine is detected |

Trigger it on demand:

```text
memory_consolidate({})
```

```rust
let report = cortex.run_consolidation()?;
```

## Common mistakes to avoid

1. **Treating Semantic as a flat store.** It's confidence-scored — filter with `memory_context`'s `min_confidence`, or read the confidence on each fact, so a superseded value (e.g. an old employer) doesn't read as current truth.
2. **Expecting an auto-extracted fact instantly.** Inference runs on ingest, but cross-episode promotion happens on consolidation — trigger `memory_consolidate` if you need it immediately.
3. **Storing secrets in any tier.** Cortex is **memory**, not a vault. Keep tokens in a secrets manager; store *references*, not values. (Memories are also Private by default and never leave the device unless you mark them `shared`.)

## See also

- [vs-other-memory.md](vs-other-memory.md) — migration surface vs mem0 / Zep / LangMem / a text file
- [integrations.md](integrations.md) — framework integration (Python SDK, MCP, HTTP)
- [README.md](../../README.md) — feature comparison and benchmark tables
- [QUICK_START_PROMPT.md](../../QUICK_START_PROMPT.md) — copy-paste prompt for Claude Code
