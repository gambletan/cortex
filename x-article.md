# Why Your AI's Memory Shouldn't Live on Someone Else's Server

Your AI assistant knows your name, your job, your preferences, your relationships, what you discussed yesterday, and what decisions you made last week. That's a lot of intimate data.

Now ask yourself: where does it go?

## The Problem

Every major AI memory solution today sends your data to someone else's server:

- **Mem0** stores your memories on their cloud. You pay $99/month for the privilege. Their server can read everything.
- **OpenAI Memory** lives on OpenAI's infrastructure. You have zero control, zero export, zero visibility into what's stored or how it's used.
- **Custom RAG pipelines** typically use Pinecone, Weaviate, or similar cloud vector databases. Your personal context sits alongside thousands of other users' data.

This isn't just a privacy concern — it's a fundamental architectural mistake.

## Why Cloud Memory Is a Bad Idea

**1. Your memories are the most valuable data an AI can have about you.**

They contain your preferences, habits, relationships, work context, health information, financial details, and decision patterns. This is orders of magnitude more sensitive than your search history.

**2. Cloud providers are targets.**

A breach of a memory service exposes not individual queries but complete, structured profiles of every user. It's the difference between someone seeing one search vs. reading your entire diary.

**3. Latency kills the experience.**

Cloud memory adds 200-500ms to every query. You feel it. Your AI hesitates before it can recall anything. Cortex does the same operation in 253µs — that's 1000x faster. The difference between "instant recall" and "thinking..."

**4. You lose control.**

Can you delete a specific memory from Mem0's servers? Can you verify it's actually gone? Can you export your data and move to a competitor? In most cases: no, no, and sort of.

## The Alternative: Local-First Memory

We built [Cortex](https://github.com/gambletan/cortex) on a simple principle: **your memories should live on your device, encrypted, under your control.**

### How It Works

```
Your Device                    Your Cloud Storage
┌──────────────┐         ┌──────────────────────┐
│  SQLCipher DB │         │  iCloud / GDrive /   │
│  (encrypted)  │ ──────> │  OneDrive / Dropbox  │
│               │ <────── │                      │
│  62µs ingest  │         │  AES-256-GCM oplog   │
│  253µs search │         │  (your key, your     │
│  3.8MB binary │         │   account, encrypted) │
└──────────────┘         └──────────────────────┘
```

- **All computation is local.** SQLite database on your disk, in-memory vector index, sub-millisecond operations. Nothing leaves your machine unless you explicitly enable sync.

- **Sync goes through YOUR cloud storage.** Not our servers — your iCloud Drive, your Google Drive, your Dropbox. We never see your data. The sync protocol writes encrypted changelog files to a folder that your cloud provider syncs.

- **Encryption is end-to-end.** AES-256-GCM with Argon2id key derivation. Even if someone compromises your cloud account, they get meaningless ciphertext.

- **Private by default.** Every memory is `Private` unless you explicitly mark it `Shared`. Private memories never leave the local database. They can't be synced, exported, or leaked.

### The Architecture

Cortex implements a 4-tier memory model inspired by human cognition:

```
Working Memory  →  current session context
      ↓
Episodic Memory →  raw experiences with timestamps
      ↓ (consolidation: decay + promotion)
Semantic Memory →  distilled facts, preferences, relationships
      ↓
Procedural      →  learned behavioral patterns
```

Each tier has different retention characteristics. Episodic memories decay over time (importance-aware exponential decay). Recurring patterns get promoted to Semantic facts. Bayesian beliefs self-correct as new evidence arrives.

The retrieval engine combines 5 signals — vector similarity, temporal recency, salience, social context, and channel relevance — to find the right memory for each query.

### Cross-Device Sync Without a Server

The sync protocol is changelog-based:

1. Each device writes append-only operation logs to its own subfolder in the sync directory
2. Each device reads other devices' logs and replays them locally
3. Conflicts are resolved using Hybrid Logical Clocks (LWW per entity)
4. Beliefs merge as CRDTs (observation lists are add-only sets)

No two devices ever write to the same file, so cloud providers never see write conflicts.

### Numbers

| Operation | Cortex | Mem0 (cloud) |
|-----------|--------|-------------|
| Ingest | **62µs** | ~200ms |
| Search (top-10) | **253µs** | ~300ms |
| Context generation | **111µs** | ~500ms |
| Belief update | **28µs** | N/A |

Binary size: **3.8 MB**. Dependencies: **0 runtime**. Cost: **$0, forever**.

## Try It

```bash
# Store a memory
cortex-mcp-server ~/.cortex/memory.db ingest "I prefer dark mode and use Rust"

# Search
cortex-mcp-server ~/.cortex/memory.db search "preferences"

# Check sync status
cortex-mcp-server ~/.cortex/memory.db sync
```

Or use it as an MCP server with Claude Code, Cursor, or any MCP-compatible AI:

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

27 tools for memory management, fact storage, belief tracking, people resolution, and more — all running locally on your machine.

## The Future

We believe AI agents will be fully decentralized — running on your device, owning your data, answering only to you. Memory is the foundation of that future. If your AI can't remember privately, it can't think independently.

**Cortex is open source (MIT), free forever, and designed to never phone home.**

[GitHub](https://github.com/gambletan/cortex) · [SECURITY.md](https://github.com/gambletan/cortex/blob/main/SECURITY.md) · 420+ tests · 0 network calls

---

*Built with Rust. Encrypted with AES-256-GCM. Synced through your own cloud. Private by default.*
