# Cortex vs. other memory layers — the migration surface

This is not a benchmark fight (those are in the [README](../../README.md)). It's the
practical "if you're already on X, here's what actually changes" — the API surface, the
data model, and the trade-offs — so you can decide whether switching is worth it.

The honest one-liner: **Cortex's whole reason to exist is local-first + private.** If you
want a hosted service that someone else operates and scales, Cortex is the wrong tool and
the options below are better. If you want your agent's memory to live on your machine, in
your own SQLite file, with no account and no data leaving the device, that's the trade
Cortex makes.

## At a glance

| | Cortex | mem0 | Zep | LangMem | Plain text / CLAUDE.md |
|---|---|---|---|---|---|
| Where data lives | your device (SQLite) | mem0 cloud (or self-host OSS) | Zep cloud (or self-host) | your app's store (LangChain) | a file |
| Account / API key | none | yes (cloud) | yes (cloud) | n/a (library) | none |
| Transport | MCP / HTTP / Rust / WASM | REST SDK | REST SDK | Python library | you wire it |
| Retrieval | vector + BM25 + recency + frecency | vector (+ graph tier) | temporal knowledge graph | configurable | substring/grep |
| Cross-device sync | your own cloud, E2E encrypted | their cloud | their cloud | n/a | file sync |
| Best when | privacy/offline/own-the-data | fast managed setup, scale | temporal graph queries | already deep in LangChain | tiny / throwaway |

(mem0 and Zep both have self-hostable open-source cores; the contrast above is with their
default hosted path, which is how most people use them.)

## If you're on a plain text file / CLAUDE.md

This is where most people start, and it's fine until it isn't. What you gain moving to
Cortex: ranking (you stop pasting the whole file every turn — `memory_context` token-
budgets what's relevant), decay (stale notes fade instead of accumulating), structured
facts + contradiction handling, and search that isn't grep. What you keep: it's still just
a local file you own (now SQLite instead of Markdown).

Migration: there isn't really one — you `memory_ingest` your existing notes (or
`memory_ingest_batch` a JSON array) and let inference structure them.

## If you're on mem0

**What's similar:** an add/search memory API, automatic fact extraction on write, a
vector-ranked recall path; mem0 also has a graph memory tier.

**Migration surface:**
- `m.add(text, user_id=…)` → `memory_ingest({ "text": …, "channel": …, "user_id": … })`
- `m.search(query, user_id=…)` → `memory_search({ "query": …, "namespace": … })`
- `m.get_all(user_id=…)` → `fact_query` / `memory_context`
- mem0's `user_id` partitioning maps to Cortex **namespaces** (`namespace` arg).

**What changes:** no API key, no network round-trip (recall is local, sub-ms), your data
never leaves the device. You lose the managed-cloud convenience (you run the binary; for
multi-device you point sync at your own iCloud/Drive/Dropbox instead of their backend).

## If you're on Zep

Zep's strength is a **temporal knowledge graph** — "what was true when," entity timelines,
graph traversal. Cortex has temporal-intent retrieval (it understands "first time" /
"recently" in a query) and a people graph + fact supersession with decay, but it is **not**
a full temporal graph DB. If your use case leans hard on graph queries over a relationship
network, Zep is the better fit today. If you want local-first storage of timestamped
episodes + distilled facts that an MCP client can use with zero setup, Cortex fits.

**Migration surface:** Zep sessions/messages → episodic `memory_ingest` (channel ≈ session
source); Zep's extracted facts → Cortex semantic facts (`fact_query`). There's no
graph-traversal query API in Cortex to map Zep's graph search onto — that's the gap.

## If you're on LangMem (LangChain)

LangMem is a library inside the LangChain ecosystem. If your stack is already LangGraph/
LangChain, staying there is the lowest-friction choice. Cortex's angle is being
**framework-agnostic** (any MCP client, plus HTTP/Rust/WASM) and a separate process you
own — you can use it *from* LangGraph via [langchain-mcp-adapters](https://github.com/langchain-ai/langchain-mcp-adapters)
without rewriting your agent. So this is less "migrate" and more "point your existing
agent at a local memory server."

## Where Cortex is NOT the right choice (read this)

- **You want a hosted service you don't operate.** Cortex is a binary you run. No SaaS.
- **You need graph traversal over a large relationship network.** Zep/mem0-graph are ahead;
  Cortex's multi-hop chain recall is still weak (tracked in docs/ROADMAP.md).
- **You're multi-tenant at scale across users on one server.** Cortex is single-user-private
  by design; namespaces isolate contexts but it isn't a multi-tenant cloud.
- **You can't run anything locally / want zero ops.** A managed API wins there.

If none of those are dealbreakers and "my memory stays on my machine" matters to you,
that's the trade Cortex is built for.

## See also
- [README.md](../../README.md) — feature/benchmark tables (incl. LoCoMo)
- [memory-tiers.md](memory-tiers.md) — the four-tier model
- [integrations.md](integrations.md) — LangGraph / DeerFlow / MCP setup
