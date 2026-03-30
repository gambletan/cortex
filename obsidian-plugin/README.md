# Cortex Memory — Obsidian Plugin

Browse and manage AI memories from [Cortex](https://github.com/gambletan/cortex) directly inside Obsidian. All data stays local with sub-millisecond latency.

## Status

**Work in progress.** This is a scaffold — the MCP communication layer and UI are not yet implemented.

## Features (planned)

- **Memory Browser** — sidebar view listing stored memories with search and filter
- **Inject Context** — command that calls `memory_context` and inserts the result at the cursor position in the active note

## Requirements

- [cortex-memory](https://www.npmjs.com/package/cortex-memory) installed (`npm i -g cortex-memory`)
- Obsidian 1.0.0+, desktop only

## Development

```bash
npm install
npm run build
# Copy dist/main.js + manifest.json to your vault's .obsidian/plugins/cortex-memory/
```
