# Cortex

[![GitHub stars](https://img.shields.io/github/stars/gambletan/cortex?style=social)](https://github.com/gambletan/cortex/stargazers)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](https://opensource.org/licenses/MIT)

> **If Cortex helps your AI remember, [give it a ⭐](https://github.com/gambletan/cortex/stargazers)** — it takes 1 second and helps others discover the project.

[中文文档](README_CN.md)

### Memory for fully decentralized AI agents.

**Persistent memory engine that runs entirely on your hardware.** Pure Rust. Local-first. 3.8MB. Zero cloud.

> **Philosophy:** We build for a future where AI agents are fully decentralized — running on your device, owning your data, answering only to you. No cloud middleman. No vendor lock-in. No one else sees your memories. Cortex is the memory layer for sovereign AI agents that need to remember, learn, and evolve — without ever phoning home.

LLMs start blank every session. Your assistant forgets your name, your preferences, the conversation you had yesterday, the decision you made last week. Current "memory" solutions are flat text files, keyword grep, or cloud APIs that add 200-500ms latency, charge you for the privilege, and send your personal data to someone else's server.

Cortex fixes this. It gives your AI a structured, queryable, self-evolving long-term memory that persists across sessions, channels, and contexts — with Bayesian beliefs that self-correct, a people graph that resolves identities across platforms, and sub-millisecond performance on everything. All running locally, all yours.

### Cortex vs Mem0 vs OpenAI Memory

| | Cortex | Mem0 | OpenAI Memory |
|---|---|---|---|
| **Privacy** | 100% local, zero cloud | Cloud API (your data on their servers) | OpenAI servers |
| **Latency** | **156µs** ingest, **568µs** search | ~200-500ms | ~300-800ms |
| **Cost** | Free, forever | $99+/mo (Pro) | ChatGPT Plus ($20/mo) |
| **Memory tiers** | 4 (Working/Episodic/Semantic/Procedural) | 1 (flat) | 1 (flat) |
| **Bayesian beliefs** | Self-correcting with evidence | No | No |
| **People graph** | Cross-channel identity resolution | Paid tier only | No |
| **Conversation compression** | Automatic session summarization | No | No |
| **Relationship inference** | Pattern-based (EN + CN) | No | No |
| **Temporal retrieval** | Intent-aware ("recently" / "first time") | No | No |
| **Contradiction detection** | Automatic with confidence scores | No | No |
| **Consolidation** | Episodic → Semantic auto-promotion | No | No |
| **Context injection** | Token-budgeted LLM-ready output | Manual | Automatic but opaque |
| **Import/Export** | Full JSON backup & restore | API only | No export |
| **Self-hosted** | Native binary, Docker, MCP | Cloud only | Cloud only |
| **Binary size** | 3.8 MB | npm package | N/A |
| **Dependencies** | 0 runtime deps | Node.js + cloud | N/A |
| **Open source** | MIT | Partial | No |
| **Chinese NLP** | Native (inference, retrieval, relationships) | No | Limited |
| **Namespace isolation** | Per-user/context memory separation | No | No |
| **Plugin system** | Compile-time hooks for ingest/retrieve/consolidation | No | No |
| **MCP tools** | 25 tools for Claude/LLM integration | 3rd party | N/A |

### Performance Benchmarks

| Operation | Cortex | Mem0 (cloud) | File-based |
|-----------|--------|-------------|------------|
| Ingest | **156µs** | ~200ms | ~1ms |
| Search (top-10) | **568µs** | ~300ms | ~10ms |
| Context generation | **621µs** | ~500ms | manual |
| Belief update | **66µs** | N/A | N/A |
| People graph | **51µs** | paid tier | N/A |
| Structured facts | **45µs** | N/A | N/A |
| 1K memories search | **1.6ms** | ~500ms | ~50ms |

**528x faster** than Mem0 cloud. With features neither Mem0 nor OpenAI Memory offer.

> **Note:** Benchmarks include proactive inference (auto-extracting facts, preferences, relationships) on every ingest. Raw ingest without inference is ~15µs. Numbers from `cargo bench` on M-series Mac.

## Architecture

Cortex implements a 4-tier memory model inspired by human cognition:

```
                    +---------------------+
                    |   Working Memory    |  Current session context
                    +---------------------+
                              |
                    +---------------------+
                    |   Episodic Memory   |  Raw experiences: conversations, events, observations
                    +---------------------+
                              |  consolidation (decay, promotion, pattern extraction)
                    +---------------------+
                    |   Semantic Memory   |  Distilled facts, preferences, relationships
                    +---------------------+
                              |
                    +---------------------+
                    | Procedural Memory   |  Learned routines, user-specific workflows
                    +---------------------+
```

**Working** holds the current session scratch pad. **Episodic** stores raw experiences with timestamps and source metadata. The **Consolidation Engine** periodically promotes recurring patterns into **Semantic** facts and decays stale episodes. **Procedural** captures learned workflows and routines.

## Key Components

### People Graph
Cross-channel identity resolution. The same person messaging you on Telegram, emailing you, and showing up in calendar events gets unified into a single identity node. Interactions, relationship strength, and communication patterns are tracked per-person.

### Bayesian Belief System
Self-correcting understanding of the world. Beliefs are formed from evidence, updated with each new observation, and can be contradicted. Confidence scores reflect actual certainty rather than recency bias.

```rust
cortex.observe_belief("user_prefers_morning_meetings", true, 0.8)?;
cortex.observe_belief("user_prefers_morning_meetings", false, 0.6)?;
// Confidence adjusts automatically via Bayesian update
```

### Consolidation Engine
Episodic-to-semantic promotion, decay of stale memories, and pattern extraction. Runs as a background cycle that keeps the memory store lean and queryable. Returns a report of what was promoted, decayed, and merged.

### Multi-signal Retrieval
Queries combine five signals for relevance ranking:
- **Similarity** -- vector cosine distance against query embedding
- **Temporal** -- recency weighting with configurable decay
- **Salience** -- importance scoring from access patterns and explicit hints
- **Social** -- boost for memories involving specific people
- **Channel** -- filter or boost by source channel

### Context Injection Protocol
Generates LLM-ready context strings from memory state. Pass a token budget, optional channel/person filters, and get back a structured text block your LLM can consume directly.

### Storage
SQLite for persistence, in-memory vector index for fast similarity search. Single-file database, no external services required. Designed for edge deployment -- runs on a laptop, a Raspberry Pi, or a server.

## Prerequisites

Install the [Rust toolchain](https://rustup.rs/) (provides `cargo`):

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

After installation, either restart your terminal or run:
```bash
source "$HOME/.cargo/env"
```

Verify:
```bash
cargo --version
```

## Real-World Example: A Personal AI That Actually Remembers

Imagine your AI assistant across a week of real conversations:

```
# Day 1 — You chat on Telegram
You: "Meeting with Sarah from Stripe went well. She's interested in our API."

  Cortex auto-extracts:
  ├── episodic memory stored (156µs)
  ├── fact: Sarah → works_at → Stripe (confidence: 0.85)
  ├── fact: Sarah → interested_in → our API
  └── person resolved: sarah_telegram

# Day 2 — Sarah emails you
From: sarah@stripe.com
"Here's the technical spec we discussed."

  Cortex:
  ├── person resolved: sarah@stripe.com → merged with sarah_telegram
  │   (same person, different channel — automatic identity resolution)
  └── fact: Sarah → sent → technical spec

# Day 3 — You ask your AI
You: "What's the status with Stripe?"

  Cortex retrieves (568µs):
  ├── Sarah works at Stripe (semantic fact)
  ├── Meeting went well, interested in API (episodic, Day 1)
  ├── She sent technical spec (episodic, Day 2)
  └── Cross-channel context: Telegram + Email unified under one person

  Your AI responds with full context — no "sorry, I don't remember" 🎯

# Day 5 — New information arrives
You: "Turns out Sarah moved to Anthropic last month."

  Cortex:
  ├── contradiction detected: Sarah works_at Stripe vs Sarah works_at Anthropic
  ├── old fact confidence decayed: Stripe (0.85 → 0.15)
  ├── new fact stored: Sarah → works_at → Anthropic (0.90)
  └── belief updated via Bayesian inference — self-correcting, no manual cleanup

# Day 7 — Consolidation runs
  Cortex auto-consolidation:
  ├── 3 episodic memories about Sarah → promoted to semantic summary
  ├── stale memories from other topics → decayed
  └── pattern detected: you have recurring Monday meetings
```

All of this happens **locally in <1ms per operation**. No cloud. No API calls. No one else sees your data.

## Quick Start

```rust
use cortex_core::Cortex;

// Open (or create) a memory database
let cortex = Cortex::open("memory.db")?;

// Ingest a memory from a Telegram conversation
let embedding = your_embedding_fn("Met with Alice about the Q3 roadmap");
cortex.ingest(
    "Met with Alice about the Q3 roadmap",
    "telegram",               // source channel
    Some("alice_123"),         // user ID (triggers identity resolution)
    Some(0.8),                 // salience hint
    Some(embedding),           // vector embedding
)?;

// Add a semantic fact directly
cortex.add_fact(
    "Alice", "works_at", "Acme Corp",
    0.95, "telegram", None,
)?;

// Store a preference
cortex.add_preference("timezone", "America/Los_Angeles", 0.9)?;

// Retrieve relevant memories
let results = cortex.retrieve(
    "What do I know about Alice?",
    5,                         // top-k
    None,                      // any channel
    None,                      // any person
    Some(query_embedding),     // vector for similarity search
)?;

// Generate LLM-ready context (token-budgeted)
let context = cortex.get_context(
    2000,                      // max tokens
    Some("telegram"),          // channel filter
    None,                      // no person filter
)?;
// Pass `context` as system/user message prefix to your LLM

// Run consolidation (call periodically)
let report = cortex.run_consolidation()?;
println!("Promoted: {}, Decayed: {}", report.promoted, report.decayed);
```

## Python Bindings

Coming soon via [PyO3](https://pyo3.rs). The `cortex-python` crate will expose the full API as a native Python module:

```python
from cortex import Cortex

cx = Cortex.open("memory.db")
cx.ingest("Had lunch with Bob at the Thai place", channel="imessage", user_id="bob")
results = cx.retrieve("Where does Bob like to eat?", limit=5)
```

## Integration with unified-channel-hub

Cortex is designed as the memory layer for [unified-channel-hub](https://github.com/gambletan/unified-channel-hub). Messages flow in from any channel adapter, Cortex ingests and indexes them, and the context injection protocol feeds relevant memory back to your LLM before each response.

```
Telegram ─┐                          ┌─ Context
Discord  ─┤  unified-channel-hub  →  │  Cortex  →  LLM
Email    ─┤  (ingest)                 │  (retrieve + inject)
Calendar ─┘                          └─ Response
```

## MCP Server (Claude Code / Claude Desktop)

Cortex ships as an MCP server — works with any MCP-compatible client.

### Setup

**1. Build & install the binary:**

```bash
mkdir -p ~/.local/bin ~/.cortex
cargo build --release -p cortex-mcp-server
cp target/release/cortex-mcp-server ~/.local/bin/
```

**2. Register as MCP server:**

Claude Code (CLI):
```bash
# Global (all projects)
claude mcp add cortex --scope user -- ~/.local/bin/cortex-mcp-server ~/.cortex/memory.db

# Or per-project
claude mcp add cortex -- ~/.local/bin/cortex-mcp-server ~/.cortex/memory.db
```

Claude Desktop — add to `~/Library/Application Support/Claude/claude_desktop_config.json`:
```json
{
  "mcpServers": {
    "cortex": {
      "command": "/Users/you/.local/bin/cortex-mcp-server",
      "args": ["/Users/you/.cortex/memory.db"]
    }
  }
}
```

**3. Allow tools in "don't ask" mode:**

Add to `~/.claude/settings.json` → `permissions.allow`:
```json
"mcp__cortex__*"
```

> Note: MCP tool permissions do not support parentheses format (e.g. `mcp__cortex__memory_ingest(*)`). Use the wildcard `mcp__cortex__*` instead.

**4. Make it automatic** — add to your `CLAUDE.md` (project or global `~/.claude/CLAUDE.md`):

```markdown
# Memory (Cortex)
You have persistent memory via Cortex MCP tools. Use them automatically:
- Start of conversation: call `memory_context` to load what you know about the user
- When the user shares a preference, fact, or personal info: call `memory_ingest` to store it
- When you learn a structured fact: call `fact_add` (e.g. "User works_at Google")
- When you detect a preference: call `preference_set` (e.g. editor=neovim)
- When evidence supports or contradicts a belief: call `belief_observe`
- When talking to someone new: call `person_resolve` to track identity
- Periodically: call `memory_consolidate` to clean up stale memories
```

**5. Auto-inject memory on session start** (Claude Code hooks — fully automatic):

Create `~/.claude/hooks/cortex-memory-inject.sh`:
```bash
#!/bin/bash
CORTEX_BIN="${CORTEX_BIN:-$HOME/.local/bin/cortex-mcp-server}"
CORTEX_DB="${CORTEX_DB:-$HOME/.cortex/memory.db}"
[ -x "$CORTEX_BIN" ] || exit 0

printf '%s\n%s\n%s\n' \
  '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"hook","version":"1.0"}}}' \
  '{"jsonrpc":"2.0","method":"notifications/initialized"}' \
  '{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"memory_context","arguments":{"max_tokens":1500}}}' \
  | "$CORTEX_BIN" "$CORTEX_DB" 2>/dev/null \
  | grep '"id":2' \
  | python3 -c "import sys,json; r=json.load(sys.stdin); print(r['result']['content'][0]['text'])" 2>/dev/null
```

Add to `~/.claude/settings.json`:
```json
{
  "hooks": {
    "SessionStart": [
      {
        "matcher": "",
        "hooks": [
          {
            "type": "command",
            "command": "~/.claude/hooks/cortex-memory-inject.sh"
          }
        ]
      }
    ]
  }
}
```

Now every new Claude Code session automatically loads your memory context — **zero manual effort**. Claude learns as you work and remembers across sessions.

### Multi-Project Isolation

Working across multiple projects? Use **separate databases** for physical memory isolation — no cross-project leakage, zero code changes needed.

```
~/.cortex/
├── global.db          # User preferences, people graph, cross-project knowledge
├── my-app.db          # Project A memories
└── my-api.db          # Project B memories
```

**Global config** (`~/.claude/settings.json`) — user-level knowledge:
```json
{
  "mcpServers": {
    "cortex-global": {
      "command": "~/.local/bin/cortex-mcp-server",
      "args": ["~/.cortex/global.db"]
    }
  },
  "permissions": { "allow": ["mcp__cortex-global__*", "mcp__cortex-project__*"] }
}
```

**Per-project config** (`~/.claude/projects/<path>/settings.json`) — project-specific:
```json
{
  "mcpServers": {
    "cortex-project": {
      "command": "~/.local/bin/cortex-mcp-server",
      "args": ["~/.cortex/my-app.db"]
    }
  }
}
```

Then add these memory isolation rules to your project's `CLAUDE.md`:

```markdown
## Memory Isolation

Two Cortex MCP servers: `cortex-project` (project DB) and `cortex-global` (global DB).

### Write Policy
- Save to `cortex-project` if the memory is about this repo's architecture, code,
  modules, tests, workflows, configs, bugs, decisions, or terminology.
- Save to `cortex-global` only for long-term user preferences, communication style,
  cross-project habits, or personal background useful across repos.
- **Default: if uncertain, save to `cortex-project`.**

### Read Policy
1. Query `cortex-project` first.
2. Query `cortex-global` second, only for user-level preferences.
3. Prefer project memory when they conflict.

### Anti-Leak Rules
- Never auto-copy from `cortex-project` into `cortex-global`.
- Never store repo-specific paths, module names, or account names in `cortex-global`.
- Never treat project implementation details as user-global preferences.

### Update Rule
- Cortex is append-only. To update: search old entry → delete → ingest new.
```

This gives you two independent Cortex instances per project — complete isolation with shared user knowledge.

### 25 Tools

| Tool | Purpose |
|------|---------|
| `memory_ingest` | Store a memory (text, channel, person context) |
| `memory_search` | Semantic search across all memory tiers |
| `memory_context` | Generate LLM-ready context summary (token-budgeted) |
| `memory_consolidate` | Run decay + promotion + sweep cycle |
| `memory_infer` | Preview inference without storing |
| `memory_compress` | Compress old conversation sessions |
| `memory_stats` | Get memory statistics (counts per tier, index size) |
| `memory_decay` | Run temporal decay on episodic memories |
| `belief_observe` | Update a Bayesian belief with evidence |
| `belief_list` | Query beliefs above confidence threshold |
| `fact_add` | Store structured knowledge (subject-predicate-object) |
| `fact_query` | Query facts by entity (SQL-indexed) |
| `preference_set` | Store user preference with confidence |
| `preference_query` | Query preferences by key pattern |
| `person_resolve` | Cross-channel identity resolution |
| `person_list` | List all known people |
| `contradiction_check` | Check for fact contradictions |
| `relationship_extract` | Extract relationships from text |

## OpenClaw Plugin

Also ships as an OpenClaw memory plugin with auto-recall and auto-capture hooks. See `openclaw-plugin/` for the full integration.

## Project Structure

```
cortex/
├── cortex-core/          # Rust core library (all memory logic)
│   ├── src/
│   │   ├── lib.rs              # Cortex entry point
│   │   ├── types.rs            # MemObject, MemoryTier, etc.
│   │   ├── inference.rs        # Proactive inference (EN + CN)
│   │   ├── episode.rs          # Episodic memory store
│   │   ├── semantic.rs         # Semantic facts + preferences
│   │   ├── working.rs          # Working memory (session scratch pad)
│   │   ├── procedural.rs       # Learned routines
│   │   ├── people.rs           # People graph + identity resolution
│   │   ├── belief.rs           # Bayesian belief system
│   │   ├── consolidation.rs    # Episodic→semantic promotion + decay
│   │   ├── retrieval.rs        # Multi-signal retrieval engine
│   │   ├── context.rs          # LLM context generation
│   │   └── storage/            # SQLite + in-memory vector index
│   └── benches/                # Performance benchmarks
├── cortex-http/          # HTTP REST API (axum, local-only)
├── cortex-mcp-server/    # MCP server binary (3.8MB)
├── cortex-python/        # Python bindings (PyO3, WIP)
├── openclaw-plugin/      # OpenClaw memory plugin
├── Dockerfile            # Self-hosted Docker image
└── Cargo.toml            # Workspace root
```

## HTTP API

Cortex ships a lightweight HTTP server for integration with any language or framework. Binds to `127.0.0.1` by default — your data never leaves your machine.

```bash
# Build & run
cargo build --release -p cortex-http
./target/release/cortex-http --port 3315 --db ~/.cortex/memory.db

# Or via Docker (pre-built from GHCR)
docker run -v ~/.cortex:/data -p 3315:3315 ghcr.io/gambletan/cortex/cortex-http:latest

# Or build locally
docker build -t cortex .
docker run -v ~/.cortex:/data -p 3315:3315 cortex
```

### Endpoints

| Method | Path | Description |
|--------|------|-------------|
| GET | `/health` | Health check |
| POST | `/v1/memories` | Ingest a memory |
| POST | `/v1/memories/search` | Semantic search |
| GET | `/v1/memories/context` | Generate LLM context |
| POST | `/v1/memories/consolidate` | Run consolidation cycle |
| POST | `/v1/memories/infer` | Preview inference (no store) |
| POST | `/v1/facts` | Add a semantic fact |
| POST | `/v1/facts/contradictions` | Check for contradictions |
| POST | `/v1/preferences` | Set a preference |
| GET | `/v1/beliefs` | List beliefs |
| POST | `/v1/beliefs/observe` | Update belief with evidence |
| POST | `/v1/people` | Resolve person identity |
| POST | `/v1/memories/compress` | Compress old conversation sessions |
| POST | `/v1/relationships/extract` | Extract relationships from text |
| GET | `/v1/export` | Export all data (JSON backup) |
| POST | `/v1/import` | Import data from backup |

### Examples

```bash
# Store a memory
curl -X POST http://localhost:3315/v1/memories \
  -H 'Content-Type: application/json' \
  -d '{"text": "I prefer dark mode", "channel": "cli"}'

# Search
curl -X POST http://localhost:3315/v1/memories/search \
  -H 'Content-Type: application/json' \
  -d '{"query": "preferences", "limit": 5}'

# Export all data (backup to iCloud, NAS, etc.)
curl http://localhost:3315/v1/export > ~/iCloud/cortex-backup.json

# Import from backup
curl -X POST http://localhost:3315/v1/import \
  -H 'Content-Type: application/json' \
  -d @~/iCloud/cortex-backup.json
```

## Roadmap

- **v0.2** ✅ — Local embedding integration (all-MiniLM-L6-v2/ONNX), batch queries, importance-aware decay + auto-consolidation
- **v0.3** ✅ — Proactive inference (auto-extract facts), temporal awareness, contradiction detection, Chinese NLP
- **v0.4** ✅ — HTTP REST API (axum), import/export (JSON backup), Docker packaging
- **v0.5** ✅ — Conversation compression, relationship inference (EN + CN), temporal retrieval enhancement, 112 tests
- **v1.0** ✅ — Feature comparison table, benchmark update, 18-feature Cortex vs Mem0 vs OpenAI
- **v1.1** ✅ — HNSW vector index (50K search: 12ms → 91µs), Python SDK (`pip install cortex-ai-memory`)
- **v1.2** ✅ — Negation detection (EN + CN), multi-hop retrieval, 117 tests
- **v1.3** ✅ — Context quality optimization, query expansion, bidirectional relationships, 126 tests
- **v1.4** ✅ — Incremental HNSW, SQL-indexed entity queries, LLM summarizer hook, 18 MCP tools, configurable decay, LLM-assisted inference, 131 tests
- **v1.5** ✅ — Docker image (GHCR auto-publish), batch ingest, dedup, namespace isolation, plugin system, event bus, archival, 351 tests
- **v1.6** ✅ — Int8 quantization (75% storage reduction), materialized column indexes, FTS5 triggers, LRU caches (MemObject + entity-facts), rayon parallel decay, Arc embedding, generation-based cache invalidation, 25 MCP tools, batch inference, enhanced Chinese NLP
- **v2.0** — Cross-device sync (CRDTs, no cloud), mobile (iOS/Android)

---

If you find Cortex useful, please consider giving it a star ⭐ — it helps others discover the project and motivates continued development!

[![Star History Chart](https://api.star-history.com/svg?repos=gambletan/cortex&type=Date)](https://star-history.com/#gambletan/cortex&Date)

## License

MIT
