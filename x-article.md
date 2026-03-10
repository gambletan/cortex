I built a memory engine that makes AI assistants 2,000x faster at remembering you. Here's why every existing solution is broken, and what I did about it.

---

The dirty secret of AI assistants: every conversation starts from zero.

Claude, GPT, Gemini — they all forget you the moment the session ends. The "memory" features they ship? Flat text files. Keyword grep. Append-only lists with no structure, no ranking, no understanding.

I've been building personal AI assistants for the past year. Multi-channel (Telegram, Slack, Discord, LinkedIn). Multi-account. Automated engagement, content generation, the works.

And the single biggest bottleneck wasn't the LLM. It was memory.

---

THE PROBLEM

Here's what "memory" looks like in 2026:

Claude Code stores everything in a MEMORY.md file. Markdown. Flat. 200-line truncation limit. It literally cannot distinguish "user mentioned sushi once" from "user has been a systems programmer for 10 years."

OpenClaw's default memory-core? Keyword grep over SQLite. No semantic understanding. No ranking.

The "premium" alternative — Mem0 — sends your data to the cloud, adds 200-500ms latency per operation, charges you money, and still stores memories as flat text.

None of them answer the real question: what does this AI actually KNOW about me?

---

WHAT'S ACTUALLY MISSING

1. No user model.

Current systems store facts but can't reason about them. If you tell Claude "I prefer Rust" on Monday, then "I'm switching to Go" on Friday — both memories coexist forever. No contradiction detection. No confidence tracking. No evolution.

2. No memory lifecycle.

Everything lives at the same level. A passing mention ("nice weather today") gets equal weight to a core preference ("I deploy everything to Cloudflare Workers"). Nothing consolidates. Nothing decays. Nothing matures.

3. No identity across channels.

Talk to your AI on Telegram, then Slack, then Discord. That's three strangers. Your AI has zero concept of "this is the same person across different platforms." No relationship graph. No interaction history.

4. Latency kills flow.

Mem0 cloud adds 200-500ms per memory operation. When your AI assistant checks memory on every turn — which it should — that's noticeable delay compounding across every single message.

---

THE SOLUTION: CORTEX

I built Cortex — a persistent memory engine for AI assistants, written in Rust.

No cloud. No API keys. No data leaving your machine. 3.8MB binary with zero dependencies.

Here's what makes it different:

4-TIER MEMORY ARCHITECTURE

Not all memories are equal. Cortex models this explicitly:

Working Memory → what's happening right now (current conversation context)
Episodic Memory → what happened (raw events, conversations, interactions)
Semantic Memory → what I know (structured facts, preferences, beliefs)
Procedural Memory → what I do (patterns, workflows, habits)

Memories automatically consolidate upward. A raw episode like "user asked about Rust async patterns three times this week" gets promoted to a semantic fact: "user actively learning Rust async." This happens without any LLM calls.

BAYESIAN BELIEF SYSTEM

This is the part I'm most proud of.

Instead of storing "user prefers dark mode" as a boolean, Cortex tracks it as a probability with confidence:

- First mention: P(prefers_dark_mode) = 0.75
- Second confirmation: P = 0.92
- User says "actually trying light mode": P drops to 0.68
- Back to dark mode next week: P = 0.89

Self-correcting. Probabilistic. Every belief evolves with evidence.

After 100 observations with 67% supporting evidence, the belief converges to 0.9944 — mathematically correct Bayesian inference, computed in 27 microseconds.

No other memory system does this.

PEOPLE GRAPH

Cross-channel identity resolution. Alice on Telegram (alice_123) and Alice on Slack (alice_work) are automatically linked to the same Person entity.

Every person has: interaction count, last seen, communication style, tags, notes, and relationship context. All queryable.

Resolved 3 identities in 41µs. Mem0 charges extra for graph memory. File-based systems can't do it at all.

MULTI-SIGNAL RETRIEVAL

When you search memories, Cortex doesn't just keyword-match. It ranks by:

- Semantic similarity (vector cosine distance)
- Temporal recency (newer = more relevant)
- Salience (importance score with decay)
- Social context (who said it matters)
- Channel affinity (same channel = boost)

Five signals, combined. Every search returns the most contextually relevant memories, not just the most recent.

---

THE BENCHMARKS

Here's where it gets fun. Full benchmark, release build, Apple Silicon:

Operation          | Cortex   | Mem0 Cloud | File-based
---------------------------------------------------------
Ingest (single)    |     7µs  | ~200ms     | ~1ms
Search (top-10)    |   132µs  | ~300ms     | ~10ms
Context generation |    51µs  | ~500ms     | manual
Belief update      |    27µs  | N/A        | N/A
People graph       |    13µs  | paid tier  | N/A
Structured facts   |     7µs  | N/A        | N/A
1K memories search |  1.2ms   | ~500ms     | ~50ms

That's 2,266x faster search than Mem0 cloud. Not a typo. Two thousand two hundred sixty-six times.

And at scale: 1,000 memories ingested in 7ms. Search still returns in 1.2ms. Context generation in 378µs.

The secret: precomputed L2 norms on every vector, partial sort (select_nth_unstable) instead of full sort, SQLite with WAL + prepared statement caching, and Rust doing what Rust does.

---

HOW IT WORKS WITH YOUR AI

Cortex ships as an MCP (Model Context Protocol) server. One binary, stdio transport.

Configure it in Claude Code:

```json
// ~/.claude/.mcp.json
{
  "mcpServers": {
    "cortex": {
      "command": "cortex-mcp-server",
      "args": ["~/.cortex/memory.db"]
    }
  }
}
```

Now Claude has 8 new tools:

- memory_ingest — store any memory with channel/person context
- memory_search — multi-signal retrieval across all tiers
- memory_context — generate LLM-ready context summary
- belief_observe — update probabilistic beliefs with evidence
- belief_list — query confident beliefs
- person_resolve — cross-channel identity resolution
- fact_add — structured subject-predicate-object triples
- preference_set — user preferences with confidence

It also ships as an OpenClaw plugin with auto-recall (inject memories before each turn) and auto-capture (store key facts after each turn).

---

WHAT CONTEXT GENERATION LOOKS LIKE

When Claude asks Cortex for context, it gets back something like:

```
[Cortex Memory Context]
## User Profile
- language = Chinese and English bilingual (confidence: 90%)
- editor = neovim (confidence: 85%)

## Recent Context
- [2026-03-10 21:25] User prefers dark mode and uses Rust
- [2026-03-10 20:15] Discussed multi-channel bot architecture

## Beliefs
- user_is_developer (likely, 95%)
- user_prefers_rust (confident, 89%)
```

Structured. Ranked. Confidence-scored. Generated in 51 microseconds.

Compare this to a 200-line MEMORY.md that gets truncated halfway through.

---

THE ARCHITECTURE

```
cortex/
├── cortex-core/        # Rust library — all memory logic
│   ├── belief.rs       # Bayesian belief engine
│   ├── consolidation.rs # Episodic → Semantic promotion
│   ├── context.rs      # LLM-ready context generation
│   ├── episode.rs      # Episodic memory store
│   ├── people.rs       # People graph + identity resolution
│   ├── retrieval.rs    # Multi-signal retrieval engine
│   ├── semantic.rs     # Facts, preferences, knowledge
│   └── storage/
│       ├── sqlite.rs   # SQLite with WAL, prepared cache
│       └── memory_index.rs # Vector index, precomputed norms
├── cortex-mcp-server/  # MCP server binary (3.8MB)
├── cortex-python/      # PyO3 bindings (WIP)
└── openclaw-plugin/    # OpenClaw integration
```

Pure Rust. SQLite (bundled, no external dependency). parking_lot for concurrency. Zero async runtime needed for the core engine.

---

WHAT'S NEXT

Phase 1: Local embedding integration (gte-small via ONNX) for true semantic search without external API calls.

Phase 2: Proactive inference — automatically extract structured knowledge from conversations. "User mentioned living in Shanghai" → auto-generates fact triple without being told.

Phase 3: Temporal awareness — distinguish "I'm in Tokyo this week" (temporary) from "I live in Shanghai" (permanent). No memory system handles this today.

Phase 4: Cross-device sync via CRDTs. Your AI remembers you everywhere.

---

MIT licensed. Zero cloud. Zero cost. 3.8MB.

The code: github.com/gambletan/cortex

If you're building AI assistants and tired of the "memory = append to a text file" paradigm, try it.

Your AI should know you. Not start from scratch every time.

#OpenSource #Rust #AI #MCP #BuildInPublic #AIMemory #RustLang #LocalFirst #ClaudeAI #DevTools
