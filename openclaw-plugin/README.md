# @cortex-ai-memory/cortex-memory

Persistent memory plugin for [OpenClaw](https://github.com/openclaw/openclaw) — powered by [Cortex](https://github.com/gambletan/cortex).

4-tier memory architecture (Working → Episodic → Semantic → Procedural) with Bayesian beliefs, people graph, and multi-signal retrieval. Local-first, zero-cloud, Rust-native.

## Prerequisites

Install the Cortex MCP server binary:

```bash
# One-line install
curl -fsSL https://raw.githubusercontent.com/gambletan/cortex/main/install.sh | bash

# Or from source
git clone https://github.com/gambletan/cortex.git
cd cortex && cargo install --path cortex-mcp-server
```

## Install

```bash
openclaw plugin add @cortex-ai-memory/cortex-memory
```

## Configuration

In your OpenClaw config:

```json
{
  "plugins": {
    "@cortex-ai-memory/cortex-memory": {
      "dbPath": "~/.cortex/memory.db",
      "binaryPath": "cortex-mcp-server",
      "autoCapture": true,
      "autoRecall": true,
      "topK": 10
    }
  }
}
```

| Option | Default | Description |
|--------|---------|-------------|
| `dbPath` | `~/.cortex/memory.db` | Path to SQLite database |
| `binaryPath` | auto-detect | Path to `cortex-mcp-server` binary |
| `autoCapture` | `true` | Store conversation context after each turn |
| `autoRecall` | `true` | Inject relevant memories before each turn |
| `topK` | `10` | Max memories to retrieve |

## Tools

The plugin registers these tools for your AI agent:

- **memory_search** — Search memories by query (ranked by similarity, recency, salience)
- **memory_store** — Save information to persistent memory
- **memory_get** — Get comprehensive context from all memory tiers
- **belief_observe** — Update beliefs with Bayesian evidence
- **fact_add** — Store structured facts (subject-predicate-object triples)
- **preference_set** — Store user preferences
- **person_resolve** — Cross-channel identity resolution

## CLI

```bash
openclaw cortex search "user preferences"
openclaw cortex context
openclaw cortex beliefs
openclaw cortex store "User prefers dark mode"
```

## License

MIT
