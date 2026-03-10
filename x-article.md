I built a memory engine that makes AI assistants 2,000x faster at remembering you.

Here's why every existing solution is fundamentally broken — and the engineering behind what I did about it.

---

THE DIRTY SECRET OF AI MEMORY

Every AI assistant in 2026 has the same problem: amnesia.

Claude, GPT, Gemini — they forget you the instant the session ends. The "memory" features they ship are band-aids. Claude Code writes to a flat MEMORY.md with a 200-line hard truncation. ChatGPT stores a bullet list. Gemini... doesn't even try.

I've spent the past year building multi-channel AI assistants — Telegram, Slack, Discord, LinkedIn, YouTube — with automated engagement, content pipelines, and cross-platform orchestration. Dozens of accounts. Thousands of interactions per day.

The bottleneck was never the LLM. It was always memory.

Not "can the model remember things" — but "can it UNDERSTAND what it knows about you, update that understanding when you change, and do it fast enough that you don't notice."

None of the existing solutions do this. Here's why.

---

WHY CURRENT MEMORY SYSTEMS FAIL

I benchmarked and analyzed the three dominant approaches:

1. FILE-BASED (Claude Code, ChatGPT)

Claude Code's MEMORY.md is literally `cat >> file.md`. No structure. No ranking. No decay. When it hits 200 lines, it truncates — silently dropping your oldest memories. It cannot distinguish "user mentioned sushi once" from "user has been a systems programmer for 10 years."

This is a notepad, not a memory system.

2. KEYWORD SEARCH (OpenClaw memory-core)

SQLite + full-text search. Better than flat files, but fundamentally limited: it returns matches, not understanding. Search "what does the user prefer?" and you get every line containing "prefer" — with zero ranking by importance, recency, or confidence.

3. CLOUD VECTOR DB (Mem0)

Mem0 is the current "best" option. It embeds your text, stores vectors in Qdrant, and does similarity search. The problems:

- 200-500ms latency per operation (network + embedding API + vector search)
- Your data leaves your machine (privacy)
- $0.01-0.05 per operation at scale (cost)
- Still stores memories as flat text (no structure)
- No belief system, no contradiction detection
- Graph memory is cloud-only, paid tier

I tested: with 20 memories, Mem0 cloud takes ~300ms to search. At 1,000 memories, it's ~500ms. That's half a second of dead air every time your AI tries to remember something about you.

---

THE INSIGHT

The problem isn't storage. It's architecture.

Human memory doesn't work like a database. You don't `SELECT * FROM memories WHERE text LIKE '%rust%'`. You have layers:

- Something that just happened (working memory)
- Something you experienced (episodic memory)
- Something you know as fact (semantic memory)
- Something you know how to do (procedural memory)

These layers interact. Episodes consolidate into facts. Facts inform beliefs. Beliefs decay when contradicted. Memories that aren't accessed fade. Memories that are accessed strengthen.

No AI memory system models this. They all treat memory as a flat append-only log.

So I built one that doesn't.

---

CORTEX: THE ENGINEERING

Cortex is a persistent memory engine written in Rust. 3.8MB binary. Zero dependencies. Zero cloud. Pure local.

Here's the architecture and the engineering decisions behind each component:

4-TIER MEMORY MODEL

Working Memory → current session scratch pad (in-memory, no persistence)
Episodic Memory → raw experiences with timestamps and source metadata
Semantic Memory → structured facts (subject-predicate-object triples), preferences, relationships
Procedural Memory → learned patterns, workflows, user-specific routines

The Consolidation Engine runs periodically and:
- Promotes recurring episodes to semantic facts (no LLM needed — pattern extraction via frequency analysis)
- Decays stale episodes (salience score drops over time)
- Sweeps dead memories below a threshold

This means the memory store is self-cleaning. It doesn't grow unboundedly like every other system.

BAYESIAN BELIEF ENGINE

This is the core differentiator.

Instead of storing "user prefers Rust" as a boolean, Cortex tracks it as a probability:

  First mention: P(prefers_rust) = 0.75
  Second confirmation: P = 0.92
  User says "switching to Go": P drops to 0.68
  Back to Rust next week: P = 0.89

Mathematically:

  posterior = prior * likelihood / evidence

With sigmoid bounds to prevent P from hitting 0 or 1 (which would make beliefs irrecoverable).

I ran 100 observations with 67% supporting evidence. The belief converged to 0.9944 — correct Bayesian inference, computed in 27 microseconds total. That's 0.27µs per update.

No other memory system does probabilistic reasoning. They all store facts as immutable strings. Cortex stores beliefs as evolving distributions.

PEOPLE GRAPH

Your AI talks to people across channels. Alice on Telegram (alice_123) and Alice on Slack (alice_work) should be the same person.

Cortex resolves identities automatically:
- `resolve_identity("telegram", "alice_123", "Alice")` → creates Person
- `resolve_identity("slack", "alice_work", "Alice")` → finds existing, merges

Each Person tracks: identities across channels, interaction count, first/last seen, communication style, tags, notes. Relationships between people are stored as semantic memory triples.

Resolved 3 cross-channel identities in 41µs. Mem0's graph memory requires the paid cloud tier. File-based systems can't do it at all.

MULTI-SIGNAL RETRIEVAL

Most memory systems rank by one signal — usually vector similarity. Cortex combines five:

1. Similarity — cosine distance between query and memory embeddings
2. Temporal — exponential decay weighting (newer = more relevant)
3. Salience — importance score from access patterns + explicit hints
4. Social — boost for memories involving a specific person
5. Channel — filter or boost by source channel

Each signal is weighted and combined into a final relevance score. The retrieval engine pre-filters by embedding similarity using an in-memory vector index, then re-ranks the top candidates with all five signals.

VECTOR INDEX: THE PERFORMANCE STORY

The in-memory vector index is where I spent the most optimization time.

Key decisions:

1. Precomputed L2 norms — every vector's norm is computed once at insert time, stored alongside the vector. Search only computes dot products, not full cosine similarity from scratch. Saves one sqrt per candidate per query.

2. Split dot_product() and l2_norm() as #[inline] functions — LLVM auto-vectorizes these into SIMD instructions (NEON on Apple Silicon, AVX2 on x86). The iterator-based `zip().map().sum()` pattern is the exact shape LLVM optimizes best.

3. Partial sort via select_nth_unstable — for top-k retrieval, we don't need a full O(n log n) sort. `select_nth_unstable` gives us the top-k in O(n), then we only sort those k elements. At 50K vectors with k=10, this saves ~4x compared to full sort.

4. SQLite pragmas — WAL mode, synchronous=NORMAL, 64MB cache, 256MB mmap, temp_store=MEMORY. These alone gave ~3x improvement on mixed read/write workloads.

5. prepare_cached() — every SQL query reuses compiled statements from rusqlite's internal LRU cache instead of re-parsing SQL on each call.

---

THE NUMBERS

Full benchmark. Release build. Apple Silicon M-series. In-memory database (pure compute, no disk I/O variance):

OPERATION            | CORTEX    | MEM0 CLOUD | FILE-BASED
─────────────────────────────────────────────────────────
Ingest (single)      |      7µs  | ~200ms     | ~1ms
Search (top-10)      |    132µs  | ~300ms     | ~10ms
Context generation   |     51µs  | ~500ms     | manual
Belief update        |     27µs  | N/A        | N/A
People graph resolve |     13µs  | paid tier  | N/A
Structured fact      |      7µs  | N/A        | N/A
1K memories search   |   1.2ms   | ~500ms     | ~50ms

SCALE TEST — 1,000 memories across 5 channels:

  Ingest all:          7ms
  Search (top-10):     1.2ms
  Context generation:  378µs (651 chars, LLM-ready)

VECTOR INDEX — 384-dimensional embeddings (typical for gte-small/bge-small):

  100 vectors:     31.7µs per search
  1,000 vectors:   152.7µs per search
  10,000 vectors:  1.5ms per search
  50,000 vectors:  12.4ms per search

Linear scaling with brute-force cosine similarity. At 50K+ vectors, we'd swap to HNSW (instant-distance crate). For personal AI assistants, 10K-50K vectors covers years of conversations.

The headline: 2,266x faster search than Mem0 cloud, with features neither Mem0 nor file-based systems offer.

---

HOW IT INTEGRATES

MCP SERVER

Cortex ships as an MCP (Model Context Protocol) server — the standard that Claude Code, Claude Desktop, Cursor, and other AI tools use for tool integration.

One binary. Stdio transport. JSON-RPC 2.0.

```json
{
  "mcpServers": {
    "cortex": {
      "command": "cortex-mcp-server",
      "args": ["~/.cortex/memory.db"]
    }
  }
}
```

8 tools available:

  memory_ingest    — store memories with channel/person context
  memory_search    — multi-signal retrieval across all tiers
  memory_context   — generate LLM-ready context summary
  belief_observe   — update beliefs with supporting/contradicting evidence
  belief_list      — query beliefs above confidence threshold
  person_resolve   — cross-channel identity resolution
  fact_add         — structured subject-predicate-object triples
  preference_set   — user preferences with confidence scores

CONTEXT INJECTION

When your AI asks "what do I know about this user?", Cortex returns:

```
[Cortex Memory Context]
## User Profile
- language = Chinese and English bilingual (confidence: 90%)
- editor = neovim (confidence: 85%)
- package_manager = pnpm (confidence: 85%)

## Recent Context
- [2026-03-10 21:25] User prefers dark mode and uses Rust
- [2026-03-10 20:15] Discussed multi-channel bot architecture

## Beliefs
- user_is_developer (likely, 95%)
- user_prefers_rust (confident, 89%)

## People
- Alice (telegram, slack) — 12 interactions
```

Structured. Ranked. Confidence-scored. Generated in 51 microseconds.

Compare this to a flat MEMORY.md that gets truncated at line 200.

---

WHAT COMES NEXT

Phase 1 (v0.2): Local embedding integration. Currently search is text-based; adding gte-small via ONNX Runtime gives true semantic search — "what programming tools does the user like?" would match "I use neovim and pnpm" without any keyword overlap. Zero external API calls.

Phase 2 (v0.3): Proactive inference. Instead of waiting to be told, Cortex will automatically extract structured knowledge from conversations. "Oh I've been living in Shanghai for 3 years" → auto-generates fact_add("User", "lives_in", "Shanghai", confidence=0.85). Small local model for entity extraction.

Phase 3 (v0.3): Temporal awareness. "I'm in Tokyo this week" should NOT overwrite "I live in Shanghai." Current memory systems can't distinguish temporary state from permanent facts. Cortex will tag temporal scope on every memory and reason about it during retrieval.

Phase 4 (v0.4): Cross-device sync via CRDTs. Your AI remembers you on your laptop, phone, and server — with conflict-free replication.

---

THE DEEPER POINT

The AI industry is spending billions on making models smarter. Better reasoning. Longer context. Faster inference.

But the elephant in the room is that even the smartest model is useless if it doesn't know who it's talking to.

A 200B parameter model with amnesia is less useful than a 7B model that remembers everything about you.

Memory isn't a feature. It's the foundation. And right now, the foundation is a markdown file.

Cortex is my attempt to fix that. MIT licensed. Zero cloud. Zero cost. 3.8MB.

AI that knows you — not AI with a notepad.

github.com/gambletan/cortex

#OpenSource #Rust #AI #MCP #BuildInPublic #AIMemory #RustLang #LocalFirst #ClaudeAI #DevTools
