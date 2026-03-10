# Cortex

### AI that knows you — not AI with a notepad.

**Persistent memory engine for personal AI assistants.** Pure Rust. Local-first. 3.8MB. Zero cloud.

LLMs start blank every session. Your assistant forgets your name, your preferences, the conversation you had yesterday, the decision you made last week. Current "memory" solutions are flat text files, keyword grep, or cloud APIs that add 200-500ms latency and charge you for the privilege.

Cortex fixes this. It gives your AI a structured, queryable, self-evolving long-term memory that persists across sessions, channels, and contexts — with Bayesian beliefs that self-correct, a people graph that resolves identities across platforms, and sub-millisecond performance on everything.

### Benchmarks

| Operation | Cortex | Mem0 (cloud) | File-based |
|-----------|--------|-------------|------------|
| Ingest | **7µs** | ~200ms | ~1ms |
| Search (top-10) | **132µs** | ~300ms | ~10ms |
| Context generation | **51µs** | ~500ms | manual |
| Belief update | **27µs** | N/A | N/A |
| People graph | **13µs** | paid tier | N/A |
| 1K memories search | **1.2ms** | ~500ms | ~50ms |

**2,266x faster** than Mem0 cloud. With features neither Mem0 nor file-based systems offer.

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

**8 tools available:** `memory_ingest`, `memory_search`, `memory_context`, `belief_observe`, `belief_list`, `person_resolve`, `fact_add`, `preference_set`

## OpenClaw Plugin

Also ships as an OpenClaw memory plugin with auto-recall and auto-capture hooks. See `openclaw-plugin/` for the full integration.

## Project Structure

```
cortex/
├── cortex-core/          # Rust core library (all memory logic)
│   ├── src/
│   │   ├── lib.rs              # Cortex entry point
│   │   ├── types.rs            # MemObject, MemoryTier, etc.
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
├── cortex-mcp-server/    # MCP server binary (3.8MB)
├── cortex-python/        # Python bindings (PyO3, WIP)
├── openclaw-plugin/      # OpenClaw memory plugin
└── Cargo.toml            # Workspace root
```

## Roadmap

- **v0.2** — Local embedding integration (gte-small/ONNX), batch queries (N+1 elimination), memory decay + auto-consolidation
- **v0.3** — Proactive inference (auto-extract facts from conversations), temporal awareness (temporary vs permanent), contradiction detection
- **v0.4** — Conversation compression, relationship inference, multi-modal memory, cross-device sync (CRDTs)
- **v1.0** — Memory-as-a-Service HTTP API, import/export (ChatGPT, Claude, Mem0), plugin marketplace

## License

MIT
