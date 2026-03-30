# Cortex Quick Start Prompt

Copy the prompt below and paste it into Claude Code to automatically install Cortex and demo its memory features in one shot.

---

```
Install Cortex memory engine and configure it as my MCP server, then demo the key features.

## Step 1: Install
Run: curl -fsSL https://raw.githubusercontent.com/gambletan/cortex/main/install.sh | bash -s -- --ide claude

## Step 2: Verify
Run: cortex-mcp-server info
Run: cortex-mcp-server ~/.cortex/memory.db stats

## Step 3: Demo — store some memories
Use the memory_ingest tool to store these:
- "I'm a software engineer working at Google"
- "I live in Shanghai and speak Chinese and English"
- "I prefer Rust over C++ for systems programming"
- "Met with Sarah from Stripe about payment integration last Tuesday"

## Step 4: Demo — query
- Search for "Sarah" and show what comes back
- Run memory_context to show the AI-ready context summary
- Query facts about "User" to show extracted knowledge
- List beliefs to show Bayesian inference
- Show stats

## Step 5: Summary
Print a summary of what was set up and the capabilities now available.
```
