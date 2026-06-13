# Cortex Launch Kit

Ready-to-post copy for a coordinated launch. Posting is yours to do (your accounts, your
timing). Recommended order: **README demo polish → Show HN → Reddit (same day, a few hours
after HN) → awesome-lists + MCP directories (anytime, passive long-tail).**

Honesty rule (this project's whole differentiator): no bought/swapped stars, no astroturf.
Every claim below is backed by the repo. If a number can't be reproduced, don't post it.

---

## 1. Show HN — the single biggest lever

**When:** Tue–Thu, ~8–10am US Eastern (HN morning peak). Avoid weekends.

**Title** (≤80 chars, no hype, no emoji):
```
Show HN: Cortex – local-first encrypted memory for AI agents (Rust, MCP)
```
Alternatives:
- `Show HN: Give Claude persistent memory that never leaves your device (Rust)`
- `Show HN: Cortex – a 3.8MB local memory engine for AI agents, zero telemetry`

**URL:** https://github.com/gambletan/cortex

**Text (the post body):**
```
I kept re-explaining the same context to Claude every session — my stack, my
conventions, decisions we made yesterday — so I built Cortex: a local-first
memory engine for AI agents.

It's a 3.8MB Rust binary that speaks MCP, so Claude Code / Claude Desktop (or any
MCP client) get persistent memory with no setup beyond registering the server.
Everything lives in a local SQLite file. No account, no API key, no cloud — your
memories never leave your device. Cross-device sync is optional and goes through
your own iCloud/Drive/Dropbox, AES-256-GCM encrypted client-side; even if your
cloud is compromised the data stays private.

What's actually different from a text file or Mem0:
- 4 memory tiers (working/episodic/semantic/procedural) with consolidation + decay
- multi-signal retrieval (vector + BM25 + recency + frecency), ~sub-ms at a few K memories
- per-memory privacy: private by default, opt a memory into sync, demote it and it's
  retracted from your other devices
- deny-by-default capability policy on the MCP tool surface
- zero telemetry, enforced in CI (the build fails if a network/telemetry crate enters
  the core; the embeddings model-fetch is the one documented, opt-out exception)

I dogfood it daily and it's been brutal on itself — black-box testing the sync path
caught a real privacy bug (a long-running process kept serving a memory another device
had just retracted). I'd rather hear what breaks for you.

Honest limits: out-of-the-box recall uses a small local embedding model (downloads
~30MB once from HF, or run fully offline with CORTEX_NO_EMBEDDINGS=1); multi-hop
"chain" recall is still weak; it's young.

Repo, benchmarks, threat model: https://github.com/gambletan/cortex
```

**First comment (post immediately after, preempts the obvious pushback):**
```
A few things I expect people to ask:

"Why not just a text file / CLAUDE.md?" — that's where I started. It doesn't scale:
no decay, no dedup, no ranking, and you hand the model the whole file every turn. Cortex
ranks and token-budgets what's relevant and forgets what isn't.

"Why not Mem0 / OpenAI memory?" — those are cloud. The entire point here is that your
memory is yours: local SQLite, zero telemetry, no key. On LoCoMo it's competitive with
Mem0 while running on-device (numbers + repro in the README; the headline run uses a
stronger embedder via Ollama — the default small model is weaker, I call that out).

"Isn't 'zero telemetry' marketing?" — it's a CI gate; the build fails if an HTTP/telemetry
crate enters the core dep tree. The one network call is the optional embedding-model
download, documented, with a one-time notice and an offline switch.

Happy to go deep on the CRDT-ish sync (HLC + last-writer-wins, per-device append-only
oplogs) or the retrieval scoring.
```

---

## 2. README polish (do BEFORE posting — lifts conversion on every channel)

Your traffic shows healthy clones but flat stars: people try it but don't get the
"wow" fast enough. Fixes, in order:
1. A 20–30s demo GIF at the very top (script below). This is the single highest-conversion
   asset. Clones-high/stars-low almost always = "no instant proof at the top."
2. The "30-second proof" runnable block (already added to the README) right under the
   tagline.

### Demo GIF script (record with your terminal; ~25s; tools: `asciinema` + `agg`, or any screen recorder)
```
# 1. (already installed) register with Claude Code — show one line
claude mcp add cortex-memory -- ~/.local/bin/cortex-mcp-server ~/.cortex/memory.db

# 2. In a Claude Code session, type:
"Remember that I deploy on Fly.io and always run tests before pushing."
   → Claude calls memory_ingest (show the tool call flash by)

# 3. Open a BRAND NEW session. Type:
"How do I deploy this project?"
   → Claude answers "Fly.io, and run tests first" — pulled from memory, not the conversation

# Caption overlay: "New session. It still remembers. 100% local."
```
Keep it under 30s, no narration, big font, one clear payoff. Put it as the first image
in the README.

---

## 3. Reddit (post a few hours after HN; tailor per sub, don't cross-post identically)

**r/LocalLLaMA** — title: `I built a local-first memory engine for AI agents (Rust, MCP, zero telemetry, 3.8MB)`
Body: lead with privacy + local + the offline `CORTEX_NO_EMBEDDINGS` mode; this crowd
cares about no-cloud and running everything themselves. Drop the GIF.

**r/ClaudeAI** — title: `Gave Claude Code persistent memory across sessions (open source, local)`
Body: lead with the Claude Code workflow — SessionStart hook auto-recalls, it learns as you
work. Show the `claude mcp add` one-liner and the GIF. This is your highest-intent audience.

**r/rust** — title: `Cortex: a local memory engine for AI agents — Rust, SQLite, HNSW, MCP`
Body: lead with the engineering — 0 runtime services, HNSW vector index, CRDT-ish sync
(HLC + LWW), CI-enforced no-network core. This crowd upvotes craft, not product.

---

## 4. awesome-lists + MCP directories (low effort, passive long-tail)

Open small PRs / submissions adding Cortex to:
- `punkpeye/awesome-mcp-servers`, `wong2/awesome-mcp-servers`
- `awesome-claude` / `awesome-claude-code` lists
- `rust-unofficial/awesome-rust` (Memory/AI section)
- an `awesome-ai-memory` list if one exists; otherwise it's a niche worth seeding
- MCP registries: mcp.so, Smithery (smithery.ai), Glama (glama.ai/mcp)

One-line blurb for these:
```
Cortex — local-first, end-to-end-encrypted memory engine for AI agents. 3.8MB Rust MCP
server, 4-tier memory, multi-signal retrieval, zero telemetry. Gives any MCP client
(Claude Code/Desktop) persistent cross-session memory that never leaves your device.
```

---

## Don't
- No buying/swapping stars, no sockpuppet upvotes — fatal for a trust-positioned project.
- Don't post the same text to multiple subreddits (filters + mods penalize it).
- Don't overclaim: the README's honesty (offline caveat, weak multi-hop, default-vs-Ollama
  benchmark) is an asset on HN/Reddit, not a liability. Technical audiences reward candor.
