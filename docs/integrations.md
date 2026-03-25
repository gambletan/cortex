# Cortex Integration Guide

Cortex provides persistent, local-first memory for AI agents. This guide shows how to integrate Cortex with popular multi-agent frameworks.

**What Cortex adds to any framework:**

- **Persistent memory** -- 4-tier (Working / Episodic / Semantic / Procedural) that survives across sessions
- **Structured facts** -- Subject-predicate-object triples with confidence scores
- **Bayesian beliefs** -- Self-correcting understanding that updates with new evidence
- **People graph** -- Cross-channel identity resolution (Telegram + Email + Slack = one person)
- **Sub-millisecond performance** -- 156us ingest, 568us search, 100% local

**Two integration paths:**

| Method | Best for | Latency |
|--------|----------|---------|
| **Python SDK** (`pip install cortex-ai-memory`) | Python frameworks (CrewAI, AutoGen) | ~156us (in-process) |
| **MCP Server** (`cortex-mcp-server`) | MCP-compatible clients (Claude, LangGraph) | ~1-5ms (IPC) |
| **HTTP API** (`cortex-http`) | Any language, microservices | ~5-10ms (HTTP) |

---

## Table of Contents

- [CrewAI](#crewai)
- [AutoGen](#autogen)
- [LangGraph](#langgraph)
- [DeerFlow](#deerflow-bytedance)
- [OpenClaw](#openclaw)

---

## CrewAI

Use Cortex as a persistent memory backend for [CrewAI](https://github.com/crewAIInc/crewAI) agents. Memories persist across crew runs, so agents remember past research, decisions, and user preferences.

### Install

```bash
pip install crewai cortex-ai-memory
```

### Python SDK Integration

```python
from crewai import Agent, Task, Crew
from cortex_ai_memory import PyCortex

# Initialize Cortex (creates or opens a local SQLite database)
cx = PyCortex("~/.cortex/crewai.db")


def cortex_recall(query: str) -> str:
    """Retrieve relevant memories from Cortex."""
    results = cx.retrieve(query, limit=5)
    if not results:
        return "No relevant memories found."
    lines = []
    for memory_id, score, text in results:
        lines.append(f"[{score:.2f}] {text}")
    return "\n".join(lines)


def cortex_remember(text: str, channel: str = "crewai") -> str:
    """Store a new memory in Cortex."""
    mem_id = cx.ingest(text, channel)
    return f"Stored memory {mem_id}"


# Create agents with Cortex-backed memory tools
researcher = Agent(
    role="Research Analyst",
    goal="Research topics thoroughly, building on past findings",
    backstory="You have access to a persistent memory system. "
              "Always check memory before starting research, and "
              "store important findings for future sessions.",
    tools=[cortex_recall, cortex_remember],
)

writer = Agent(
    role="Technical Writer",
    goal="Write clear reports using research findings and past context",
    backstory="You can recall previous reports and user preferences "
              "from persistent memory.",
    tools=[cortex_recall, cortex_remember],
)

# Tasks
research_task = Task(
    description=(
        "First, call cortex_recall('previous research on {topic}') to check "
        "what we already know. Then research {topic} and store key findings "
        "with cortex_remember."
    ),
    expected_output="Research summary with sources",
    agent=researcher,
)

writing_task = Task(
    description=(
        "Call cortex_recall('user writing preferences') to check style "
        "preferences. Write a report based on the research findings."
    ),
    expected_output="Polished report",
    agent=writer,
)

# Run the crew
crew = Crew(agents=[researcher, writer], tasks=[research_task, writing_task])
result = crew.kickoff(inputs={"topic": "edge AI inference"})

# After the crew finishes, store structured facts for future runs
cx.add_fact("edge_ai", "trend", "moving to on-device inference", 0.85, "crewai")
cx.add_preference("report_style", "concise with bullet points", 0.9)
```

### Storing Facts and Beliefs Between Runs

```python
from cortex_ai_memory import PyCortex

cx = PyCortex("~/.cortex/crewai.db")

# Store structured knowledge that agents can query later
cx.add_fact("Alice", "works_at", "Stripe", 0.95, "crewai")
cx.add_fact("Q3_deadline", "is", "September 30", 0.90, "crewai")

# Track evolving beliefs with Bayesian updates
cx.observe_belief("market_is_growing", True, 0.7)
# Later, if counter-evidence arrives:
cx.observe_belief("market_is_growing", False, 0.4)
# Confidence adjusts automatically

# Track people across channels
cx.add_person("Alice", "slack", "alice_s")
cx.add_person("Alice", "email", "alice@stripe.com")
# Cortex merges these into a single identity node

# Run consolidation periodically to promote patterns to semantic memory
scanned, promoted, swept, patterns = cx.run_consolidation()
```

---

## AutoGen

Use Cortex with [Microsoft AutoGen](https://github.com/microsoft/autogen) to give agents persistent memory across multi-turn conversations and sessions.

### Install

```bash
pip install autogen-agentchat cortex-ai-memory
```

### Python SDK Integration (AutoGen v0.4+)

```python
from autogen_agentchat.agents import AssistantAgent
from autogen_agentchat.teams import RoundRobinGroupChat
from autogen_agentchat.conditions import TextMentionTermination
from autogen_ext.models.openai import OpenAIChatCompletionClient
from cortex_ai_memory import PyCortex

# Initialize Cortex
cx = PyCortex("~/.cortex/autogen.db")


def memory_search(query: str) -> str:
    """Search Cortex memory for relevant context."""
    results = cx.retrieve(query, limit=5)
    if not results:
        return "No memories found."
    return "\n".join(f"- [{score:.2f}] {text}" for _, score, text in results)


def memory_store(text: str) -> str:
    """Store information in persistent memory."""
    mem_id = cx.ingest(text, "autogen")
    return f"Stored: {mem_id}"


def memory_add_fact(subject: str, predicate: str, obj: str) -> str:
    """Store a structured fact (subject-predicate-object)."""
    mem_id = cx.add_fact(subject, predicate, obj, 0.85, "autogen")
    return f"Fact stored: {subject} {predicate} {obj}"


def get_context() -> str:
    """Get a token-budgeted summary of all relevant memory."""
    return cx.get_context(1500, channel="autogen")


model = OpenAIChatCompletionClient(model="gpt-4o")

# Agent with memory tools
memory_agent = AssistantAgent(
    name="memory_agent",
    model_client=model,
    tools=[memory_search, memory_store, memory_add_fact, get_context],
    system_message=(
        "You are a memory-augmented assistant. At the start of every conversation, "
        "call get_context() to load what you know. When the user shares new "
        "information, store it with memory_store. For structured facts, use "
        "memory_add_fact. Always check memory_search before saying you don't know."
    ),
)

analyst = AssistantAgent(
    name="analyst",
    model_client=model,
    tools=[memory_search, memory_add_fact],
    system_message=(
        "You analyze data and store conclusions as structured facts. "
        "Always search memory first for prior analysis."
    ),
)

# Group chat with shared memory
termination = TextMentionTermination("TERMINATE")
team = RoundRobinGroupChat(
    participants=[memory_agent, analyst],
    termination_condition=termination,
)


# Run a task
async def main():
    result = await team.run(task="What do you remember about our project?")
    print(result)


import asyncio
asyncio.run(main())
```

### Session Lifecycle Pattern

```python
from cortex_ai_memory import PyCortex

cx = PyCortex("~/.cortex/autogen.db")

# --- Start of session: load context ---
context = cx.get_context(2000)
# Inject `context` into your agent's system message

# --- During session: store as you go ---
cx.ingest("User wants weekly status reports in bullet format", "autogen")
cx.add_preference("report_format", "weekly bullets", 0.9)
cx.observe_belief("user_prefers_async_updates", True, 0.75)

# --- End of session: consolidate ---
scanned, promoted, swept, patterns = cx.run_consolidation()
print(f"Consolidated: {promoted} memories promoted, {swept} stale swept")
```

---

## LangGraph

Cortex integrates with [LangGraph](https://github.com/langchain-ai/langgraph) via [langchain-mcp-adapters](https://github.com/langchain-ai/langchain-mcp-adapters). All 27 Cortex MCP tools become available to your LangGraph agent automatically.

### Install

```bash
pip install langgraph langchain-mcp-adapters langchain-openai
cargo build --release -p cortex-mcp-server
cp target/release/cortex-mcp-server ~/.local/bin/
```

### Integration

```python
from langchain_mcp_adapters.client import MultiServerMCPClient
from langgraph.prebuilt import create_react_agent
from langchain_openai import ChatOpenAI

model = ChatOpenAI(model="gpt-4o")

async with MultiServerMCPClient({
    "cortex": {
        "command": "cortex-mcp-server",
        "args": ["~/.cortex/memory.db"]
    }
}) as client:
    agent = create_react_agent(model, client.get_tools())
    # Agent now has all 27 Cortex memory tools
    result = await agent.ainvoke({
        "messages": [{"role": "user", "content": "What do you remember about Alice?"}]
    })
```

Your LangGraph agent gets instant access to `memory_search`, `memory_ingest`, `fact_add`, `belief_observe`, `person_resolve`, and 22 more tools -- all running locally.

See the [LangGraph section in README.md](../README.md#integration-with-langgraph) for full details.

---

## DeerFlow (ByteDance)

Cortex works as a persistent memory layer for [DeerFlow](https://github.com/bytedance/deer-flow) -- ByteDance's open-source multi-agent orchestration platform. Zero code changes needed.

### Install

```bash
cargo build --release -p cortex-mcp-server
cp target/release/cortex-mcp-server ~/.local/bin/
```

### Integration

Add Cortex to your DeerFlow configuration:

```yaml
# Add to DeerFlow config.yaml
mcp_servers:
  cortex-memory:
    command: cortex-mcp-server
    args:
      - ~/.cortex/deerflow.db
```

All DeerFlow agents (Telegram, Slack, Feishu) get instant access to 27 memory tools -- cross-session memory, fact storage, people graph, and belief tracking across all channels.

See the [DeerFlow section in README.md](../README.md#integration-with-deerflow-bytedance) for full details.

---

## OpenClaw

Cortex provides a native plugin for [OpenClaw](https://github.com/openclaw/openclaw) with auto-recall and auto-capture.

### Install

```bash
# 1. Install Cortex binary
curl -fsSL https://raw.githubusercontent.com/gambletan/cortex/main/install.sh | bash

# 2. Install the OpenClaw plugin
openclaw plugin add @cortex-ai-memory/cortex-memory
```

### Configuration

```json
{
  "plugins": {
    "@cortex-ai-memory/cortex-memory": {
      "autoCapture": true,
      "autoRecall": true,
      "topK": 10
    }
  }
}
```

**What it does:**

- `autoCapture` -- automatically stores conversation context after each turn
- `autoRecall` -- injects relevant memories before each turn (your agent "remembers")
- 7 tools: `memory_search`, `memory_store`, `fact_add`, `belief_observe`, `person_resolve`, and more

See the [OpenClaw section in README.md](../README.md#openclaw-plugin) and [`openclaw-plugin/README.md`](../openclaw-plugin/README.md) for full configuration options.

---

## HTTP API (Any Framework)

For frameworks not listed above, or for non-Python languages, use the Cortex HTTP API. It works with any language that can make HTTP requests.

### Install

```bash
cargo build --release -p cortex-http
./target/release/cortex-http --port 3315 --db ~/.cortex/memory.db

# Or via Docker
docker run -v ~/.cortex:/data -p 3315:3315 ghcr.io/gambletan/cortex/cortex-http:latest
```

### Usage from Any Language

```bash
# Store a memory
curl -X POST http://localhost:3315/v1/memories \
  -H 'Content-Type: application/json' \
  -d '{"text": "User prefers dark mode", "channel": "cli"}'

# Search memories
curl -X POST http://localhost:3315/v1/memories/search \
  -H 'Content-Type: application/json' \
  -d '{"query": "user preferences", "limit": 5}'

# Get LLM-ready context
curl http://localhost:3315/v1/memories/context?max_tokens=2000

# Add a structured fact
curl -X POST http://localhost:3315/v1/facts \
  -H 'Content-Type: application/json' \
  -d '{"subject": "Alice", "predicate": "works_at", "object": "Stripe", "confidence": 0.95, "channel": "api"}'
```

See the [HTTP API section in README.md](../README.md#http-api) for the full endpoint reference.
# test
