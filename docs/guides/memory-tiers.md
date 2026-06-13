# Cortex Memory Tiers

Cortex organizes memory into four tiers. This guide describes what each tier is for, how data flows between them, and how to write code that respects the tier boundaries.

> **TL;DR**
> - **Working** = current session scratch pad, lives in RAM
> - **Episodic** = raw experiences with timestamps, persisted
> - **Semantic** = distilled facts / preferences / relationships, persisted
> - **Procedural** = learned routines and user-specific workflows, persisted
> - The **Consolidation Engine** promotes Episodic → Semantic, decays stale entries, and updates Procedural.

---

## Tier overview

| Tier | Lifetime | Stored as | Typical size | Default promotion / decay |
|---|---|---|---|---|
| Working | Single session | RAM (in-memory ring) | KB | Dropped on session end |
| Episodic | Days to weeks | SQLite + indexed by `created_at` | Hundreds to thousands of rows per session | Promoted to Semantic by Consolidation; stale rows decay |
| Semantic | Indefinite | SQLite + vector index + facts table | Bounded by user-stated facts | Updated by Bayesian belief update; never auto-decays |
| Procedural | Indefinite | SQLite + workflow table | Bounded by distinct routines | Updated when a pattern recurs ≥ N times |

## Working memory

### What it is

The scratch pad for the current session — the LLM's "RAM". Holds:

- The system prompt
- Recent conversation turns (with optional summarization once the budget is exceeded)
- The current tool-call plan

### How to use it

In the **MCP server**, working memory is implicit. You do not write to it directly; it is maintained as the LLM emits and consumes tokens. In the **Python SDK**, you can read it via `cortex.working.context()` and force a flush via `cortex.working.flush()`.

### When it disappears

At session end, or when you call `flush()` manually. Anything you want to survive a session must be promoted to Episodic or Semantic before the session ends.

## Episodic memory

### What it is

Raw experiences with timestamps and source metadata. Each entry is a row in the `episodes` table, keyed by `(created_at, episode_id)`.

### How to write to it

```python
# Python SDK
cortex.episodic.ingest(
    content="Met with Sarah from Stripe about payment integration last Tuesday.",
    source="telegram",
    occurred_at="2026-06-01T15:00:00Z",
)
```

```text
# MCP server (29 tools)
memory_ingest({
  "content": "Met with Sarah from Stripe about payment integration last Tuesday.",
  "source": "telegram",
  "occurred_at": "2026-06-01T15:00:00Z"
})
```

The ingestion path also runs **proactive inference**: it extracts facts (e.g. "Sarah → works_at → Stripe"), updates the people graph, and records the episode. See [docs/guides/integrations.md](integrations.md) for the framework-specific setup.

### How to query it

Episodic search is **temporal + lexical + vector** (fused at query time). Useful patterns:

- "Recent conversations with Sarah" → `episodic.search("Sarah", time_window="30d")`
- "What happened last Tuesday" → `episodic.search(time_window="2026-05-25..2026-06-01")`
- "All episodes mentioning Stripe" → `episodic.search("Stripe")` (lexical / vector)

## Semantic memory

### What it is

Distilled facts, preferences, and relationships. Stored as **Subject → predicate → Object** triples with a confidence score. Each fact is updated by Bayesian inference — a new episode that contradicts an existing fact lowers the fact's confidence rather than deleting it.

### How to write to it

Most facts are **auto-extracted** during episodic ingest (see the `Met with Sarah from Stripe` example above). You can also write facts directly:

```python
cortex.semantic.assert_fact(
    subject="user",
    predicate="preferred_language",
    obj="Rust",
    confidence=0.92,
)
```

### How to query it

```python
# All facts about the user
cortex.semantic.facts_about("user")

# Specific relationship
cortex.semantic.facts_where(subject="Sarah", predicate="works_at")
# → [{"object": "Stripe", "confidence": 0.87, "last_updated": "2026-06-01"}]

# Top-N most confident facts
cortex.semantic.facts(top_by="confidence", limit=10)
```

## Procedural memory

### What it is

Learned routines and user-specific workflows. Cortex observes recurring tool-call sequences and promotes them to a named procedure once the pattern recurs N times (configurable, default 3).

### How to write to it

Procedures are **auto-extracted** from observed agent tool-call traces. You can also assert a procedure manually:

```python
cortex.procedural.assert_procedure(
    name="user-onboarding-flow",
    steps=[
        ("tool", "send_email", {"to": "user@example.com"}),
        ("tool", "wait_for_reply", {"timeout": "24h"}),
        ("tool", "create_record", {"template": "new_user"}),
    ],
    trigger_pattern="user says 'sign me up'",
)
```

### How to query it

```python
# Find a procedure that matches an intent
cortex.procedural.match("user wants to onboard")

# Inspect a specific procedure
cortex.procedural.get("user-onboarding-flow")
```

## Consolidation: how the tiers move data

The **Consolidation Engine** runs on a configurable cycle (default every 5 minutes, also manually triggerable). It performs three operations:

| Operation | Direction | Trigger |
|---|---|---|
| **Promotion** | Episodic → Semantic | A fact recurs across ≥ 2 episodes with similar context |
| **Decay** | Episodic → drop | An episode is older than the configured TTL AND has not been re-referenced |
| **Pattern promotion** | Episodic → Procedural | The same tool-call sequence recurs ≥ 3 times |

You can configure the cadence and the thresholds via `cortex.config.consolidation.{decay_ttl, promotion_threshold, pattern_threshold}`.

## Common mistakes to avoid

1. **Writing everything to Episodic.** If you have a fact that should never decay (a user-stated preference), assert it as **Semantic** directly, do not let it ride the Episodic decay curve.
2. **Reading Semantic as a flat store.** Semantic is **Bayesian** — read with `min_confidence=0.5` or you will get stale, low-confidence facts. Use the confidence score to filter.
3. **Bypassing Consolidation.** If you write to Episodic and then immediately query Semantic, the fact may not be promoted yet. Either trigger `cortex.consolidate()` manually after a write, or wait one cycle.
4. **Storing code or secrets in any tier.** Cortex is **memory**, not a vault. Use a secrets manager for tokens; store *references* in Cortex, not the values.

## See also

- [integrations.md](integrations.md) — framework integration (Python SDK, MCP, HTTP)
- [README.md](../../README.md) — feature comparison and benchmark tables
- [QUICK_START_PROMPT.md](../../QUICK_START_PROMPT.md) — copy-paste prompt for Claude Code

---

_Written as a docs-only contribution. No src/ changes; CI (`cargo test`) is unaffected._

_(Posted from an AI agent account — happy to revise tone, scope, or section ordering if maintainer prefers a different organization.)_
