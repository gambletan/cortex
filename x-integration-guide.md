I gave Claude a brain. Here's how to make your AI actually remember you.

---

THE PROBLEM

Every time you open Claude Code, it starts blank. Your name, your preferences, your tech stack, the conversation you had yesterday — gone.

Claude's built-in memory? A 200-line markdown file that silently truncates your oldest memories. No ranking. No understanding. Just a notepad.

I built Cortex — a persistent memory engine that gives Claude (and any MCP-compatible AI) structured, self-evolving long-term memory. Bayesian beliefs. Cross-channel identity resolution. Sub-millisecond retrieval. 100% local.

Here's exactly how to set it up in 5 minutes.

---

SETUP: CLAUDE CODE (CLI)

1. Install the binary:

  git clone https://github.com/gambletan/cortex
  cd cortex
  cargo build --release -p cortex-mcp-server
  cp target/release/cortex-mcp-server ~/.local/bin/

2. Register as MCP server:

  claude mcp add cortex --scope user -- \
    ~/.local/bin/cortex-mcp-server ~/.cortex/memory.db

3. Allow the tools (if using "don't ask" mode):

Add these to ~/.claude/settings.json → permissions.allow:

  mcp__cortex__memory_ingest
  mcp__cortex__memory_search
  mcp__cortex__memory_context
  mcp__cortex__memory_consolidate
  mcp__cortex__belief_observe
  mcp__cortex__belief_list
  mcp__cortex__person_resolve
  mcp__cortex__fact_add
  mcp__cortex__preference_set

4. Tell Claude to use it — add to CLAUDE.md:

  # Memory (Cortex)
  You have persistent memory via Cortex MCP tools.
  - Start of conversation: call memory_context to load user context
  - When user shares a preference or fact: call memory_ingest
  - For structured facts: call fact_add
  - For preferences: call preference_set
  - Track beliefs with belief_observe

5. Make it fully automatic — add a SessionStart hook:

Create ~/.claude/hooks/cortex-memory-inject.sh that calls memory_context via JSON-RPC and outputs the context. Claude Code injects the output into every new session automatically.

  "hooks": {
    "SessionStart": [{
      "matcher": "",
      "hooks": [{"type": "command", "command": "~/.claude/hooks/cortex-memory-inject.sh"}]
    }]
  }

Now Claude remembers you before you say a word.

---

SETUP: CLAUDE DESKTOP

Add to ~/Library/Application Support/Claude/claude_desktop_config.json:

  {
    "mcpServers": {
      "cortex": {
        "command": "~/.local/bin/cortex-mcp-server",
        "args": ["~/.cortex/memory.db"]
      }
    }
  }

Restart Claude Desktop. 9 memory tools appear automatically.

---

WHAT CLAUDE GETS

9 tools that turn it from stateless to stateful:

  memory_ingest    — store what you learn about the user
  memory_search    — semantic search across all memory
  memory_context   — generate a context summary (token-budgeted)
  memory_consolidate — run decay + promotion + cleanup
  belief_observe   — "user prefers Rust" → probability 0.92
  belief_list      — what do I believe about this user?
  fact_add         — structured knowledge: User works_at Google
  preference_set   — user preferences with confidence scores
  person_resolve   — Alice on Telegram = Alice on Slack

---

WHAT HAPPENS IN PRACTICE

Session 1: You tell Claude you prefer Rust.
  → memory_ingest stores it
  → belief_observe("user_prefers_rust", supports=true, 0.8)
  → preference_set("language", "rust", 0.85)

Session 2: You open a new conversation.
  → SessionStart hook fires
  → memory_context returns:

  [Cortex Memory Context]
  ## User Profile
  - language = rust (confidence: 85%)
  - editor = neovim (confidence: 90%)

  ## Beliefs
  - user_prefers_rust (confident, 92%)
  - user_is_developer (likely, 95%)

Claude already knows you. No "Hi, I'm Claude, how can I help you today?" It just starts helping — with context.

Session 5: You say "actually I'm switching to Go."
  → belief_observe("user_prefers_rust", supports=false, 0.7)
  → Probability drops from 0.92 to 0.68
  → Not deleted — Cortex understands belief change, not binary flip

This is the difference between a notepad and a brain.

---

THE NUMBERS

  Ingest:          7µs (vs 200ms Mem0 cloud)
  Search:          95µs (vs 300ms Mem0 cloud)
  Context gen:     58µs (vs 500ms Mem0 cloud)
  Binary:          27MB (with local embeddings)
  Privacy:         100% local, zero cloud
  Cost:            $0

3,151x faster than Mem0 cloud. With features neither Mem0 nor file-based systems offer.

---

AI that knows you — not AI with a notepad.

github.com/gambletan/cortex
MIT licensed. Zero cloud. Zero cost.

#ClaudeCode #ClaudeAI #MCP #Rust #OpenSource #AIMemory #DevTools #BuildInPublic #LocalFirst #RustLang
