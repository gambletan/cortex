# How I Built Persistent Memory for Claude Code Agents (and Why Your AI Forgetting Everything Is a Solved Problem)

Every Claude Code session starts blank. Your agent forgets your name, your codebase conventions, the decision you made 20 minutes ago. You end up re-explaining the same context over and over.

I got tired of this and built **Cortex** — a local-first memory engine that gives AI agents real long-term memory. Here's what it does and why I think it matters.

## The Problem

Current "memory" for AI agents is one of:
- **Flat text files** — grep-based, no structure, no decay, no relationships
- **Cloud APIs (Mem0, etc.)** — 200-500ms latency per query, $99+/mo, your data on someone else's servers
- **OpenAI Memory** — opaque, no export, no control

None of these give you what human memory actually does: structured recall, belief updating, relationship tracking, and forgetting things that don't matter anymore.

## What Cortex Does Differently

Cortex is a **pure Rust MCP server** (3.8MB binary, zero runtime deps) that runs 100% locally:

- **4 memory tiers** — Working → Episodic → Semantic → Procedural (like human memory)
- **Bayesian beliefs** that self-correct with new evidence
- **People graph** with cross-platform identity resolution
- **Sub-millisecond everything** — 156µs ingest, 568µs search (528x faster than Mem0)
- **30 MCP tools** — plug into Claude Code, Claude Desktop, or any MCP client
- **AES-256-GCM encrypted sync** through your own cloud storage (iCloud/GDrive/OneDrive/Dropbox) — with key rotation, HMAC tamper detection, and per-memory privacy: everything is private-by-default and never leaves your device; mark one memory "shared" and it syncs; demote it back and it's *deleted off your other devices*
- **Deny-by-default tool authorization** — an agent gets zero memory access until you grant read/write/sync
- **Zero telemetry, zero cost, forever**

## Real-World Example: Claude Code + Cortex

I use Cortex with Claude Code for my X-Auto project (automated social media + SEO). Here's what changes:

**Before Cortex:** Every session I had to re-explain that the project uses Gemini as its LLM provider, that we push directly to main, that tests must pass before commits, and dozens of project-specific details.

**After Cortex:** Claude Code remembers all of this across sessions. It knows my code style preferences, which modules I've been working on, what decisions we made last week and why. A SessionStart hook auto-injects a memory digest into every new session — zero manual effort — and the capture protocol writes durable facts back before sessions end.

The dogfooding paid off in an unexpected way: black-box testing the sync path found a real privacy bug (a long-running process kept serving search results for a memory another device had just retracted — stale cache after sync pull). The same week, the implementer-written tests all passed. Lesson learned: acceptance tests now come from a context-isolated agent that never sees the implementation.

## Benchmark Results

We tested against the LoCoMo benchmark (ACL 2024) — 1540 QA pairs across long-term conversations:

| System | Overall |
|--------|---------|
| Cortex | **73.7%** |
| Mem0-Graph | 68.4% |
| Mem0 | 66.9% |
| OpenAI Memory | 52.9% |

Cortex beats Mem0 by 7 percentage points while running entirely on your machine with zero API costs.

## How to Try It

```bash
# Install (macOS/Linux)
curl -fsSL https://raw.githubusercontent.com/gambletan/cortex/main/install.sh | bash

# Or with npm
npx cortex-mcp
```

Register with Claude Code:
```bash
claude mcp add cortex-memory --scope user -- ~/.local/bin/cortex-mcp-server ~/.cortex/memory.db
```

Or run the one-shot script — it installs the binary, joins encrypted cloud sync, sets up the auto-recall hook, and registers the MCP server:
```bash
git clone https://github.com/gambletan/cortex && cortex/scripts/setup-device-sync.sh
```

That's it. Claude Code now has persistent memory — on every device you own.

## Open Source, MIT Licensed

Full source: **https://github.com/gambletan/cortex**

If it's useful, a star helps others find it. Questions welcome.
