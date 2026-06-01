# Distribution / listing submission kit

Cortex already shows up via Google + a lobehub listing. These are the highest-leverage directories to add. Each entry below is ready to paste. Submit from the **gambletan** account.

## Canonical one-line description (reuse everywhere)

> **Cortex** — Private. Free. Local. A pure-Rust memory engine for AI agents. 4-tier memory, Bayesian beliefs, people graph, AES-256-GCM encrypted sync through your own cloud. 29 MCP tools, 3.8 MB binary, 73.7% on LoCoMo (beats Mem0).

Repo: https://github.com/gambletan/cortex • Demo: https://gambletan.github.io/cortex/

---

## 1. Official MCP registry (`server.json`)
You already ship `server.json` (schema 2025-12-11). Publish/refresh it with the MCP publisher CLI:
```bash
# https://github.com/modelcontextprotocol/registry
mcp-publisher login github      # auth as gambletan
mcp-publisher publish           # reads ./server.json
```
> ⚠️ Before publishing: bump `version` to match the latest release and confirm the `ghcr.io/...` image tag actually exists. Tool count in the description is now corrected to 29.

## 2. punkpeye/awesome-mcp-servers  (largest community list)
Repo: https://github.com/punkpeye/awesome-mcp-servers — fork, add under a relevant category (e.g. 🧠 *Knowledge & Memory*), open PR:
```markdown
- [gambletan/cortex](https://github.com/gambletan/cortex) 🦀 🏠 - Local-first, encrypted memory engine for AI agents. 4-tier memory, Bayesian beliefs, people graph, cross-device sync via your own cloud. 29 tools, 3.8 MB Rust binary.
```
(Legend in that repo: 🦀 = Rust, 🏠 = local service.)

## 3. wong2/awesome-mcp-servers
Repo: https://github.com/wong2/awesome-mcp-servers — same PR pattern, "Community Servers" section.

## 4. Smithery  (smithery.ai)
Registry + hosted catalog. Submit at https://smithery.ai/new (connect the GitHub repo). Needs a `smithery.yaml` or it reads the MCP config — point it at `cortex-mcp-server`.

## 5. mcp.so
Directory at https://mcp.so — submit via https://mcp.so/submit (GitHub URL + the one-liner above).

## 6. glama.ai MCP directory
https://glama.ai/mcp/servers — auto-indexes public MCP repos; claim/refresh the listing once it appears.

## 7. PulseMCP
https://www.pulsemcp.com — submit via their "Add a server" form.

---

## Non-MCP lists (privacy / Rust angle)

## 8. awesome-rust
https://github.com/rust-unofficial/awesome-rust — fits under *Database* or *Applications → Artificial Intelligence*. Note their inclusion bar (some maturity / tests) — Cortex's 489 tests + releases qualify.
```markdown
- [cortex](https://github.com/gambletan/cortex) — Local-first, encrypted long-term memory engine for AI agents (4-tier memory, HNSW vector search, MCP server). [MIT]
```

## 9. awesome-selfhosted / privacy-focused lists
https://github.com/awesome-selfhosted/awesome-selfhosted — the "your data on your hardware, sync through your own cloud" story fits. Check their PR guidelines (needs an open license + active maintenance — both true).

## 10. Agent-framework integration docs
You already document CrewAI / AutoGen / LangGraph / DeerFlow in `docs/guides/integrations.md`. Open small PRs / issues against those projects' "memory" or "integrations" docs linking Cortex as a memory backend — that's where buyers with intent actually look.

---

### Submission checklist
- [ ] MCP official registry (`mcp-publisher publish`)
- [ ] punkpeye/awesome-mcp-servers PR
- [ ] wong2/awesome-mcp-servers PR
- [ ] Smithery
- [ ] mcp.so
- [ ] glama.ai (claim listing)
- [ ] PulseMCP
- [ ] awesome-rust PR
- [ ] awesome-selfhosted PR (check guidelines first)
- [ ] Integration-doc PRs to CrewAI / AutoGen / LangGraph
