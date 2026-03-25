# X Thread: How to make your Claude's memory work across all your devices

---

**Tweet 1 (Hook)**

Your Claude forgets everything when you switch devices.

Your MacBook Claude doesn't know what your iMac Claude learned yesterday.

I fixed this. Here's how to give Claude cross-device memory in 2 minutes:

---

**Tweet 2 (Problem)**

The problem:
- Claude Code stores memory per machine (~/.claude/)
- Switch laptop? Start from zero
- Different project on different device? No shared context

Your AI should remember YOU, not just your current machine.

---

**Tweet 3 (Solution intro)**

I built Cortex — a local memory engine that syncs through YOUR cloud storage.

- iCloud, Google Drive, OneDrive, Dropbox
- AES-256-GCM encrypted (even if your cloud is hacked, memories stay private)
- 156us writes. 568us search. Pure Rust. 3.8MB.

---

**Tweet 4 (How it works)**

How sync works:

```
Mac A → writes to iCloud/cortex-sync/
Mac B → reads from iCloud/cortex-sync/

No server. No API. No account.
Just your own cloud folder.
```

Each device writes its own append-only log. No conflicts ever.

---

**Tweet 5 (Setup)**

Setup (2 minutes):

```bash
# Install
curl -fsSL https://raw.githubusercontent.com/gambletan/cortex/main/install.sh | bash

# Register as Claude MCP server
claude mcp add cortex --scope user -- ~/.local/bin/cortex-mcp-server ~/.cortex/memory.db
```

Then just tell Claude: "Enable cross-device sync"

Claude calls sync_enable, auto-detects your cloud provider, done.

---

**Tweet 6 (What happens next)**

Now when you chat with Claude on Device A:

"I prefer dark mode, use Neovim, and my timezone is Asia/Shanghai"

Switch to Device B, Claude already knows. No setup. No copy-paste. Just works.

Beliefs, facts, preferences, people graph — all synced.

---

**Tweet 7 (Privacy)**

Privacy first:

- Private memories (default) NEVER leave your device
- Only Shared/Public memories sync
- End-to-end encrypted with your passphrase
- Zero telemetry. Zero cloud servers. Verify: grep the source code yourself.

Your memories are yours. Period.

---

**Tweet 8 (Benchmarks)**

vs Mem0 cloud:
- 528x faster
- Free (Mem0: $99/mo)
- 100% local
- 4-tier memory (Mem0: flat)
- Bayesian beliefs, people graph, contradiction detection

LoCoMo benchmark (ACL 2024): Cortex 73.7% vs Mem0 66.9% vs OpenAI Memory 52.9%

---

**Tweet 9 (CTA)**

Cortex is MIT licensed, 100% open source.

Give it a try. Give it a star if it helps.

github.com/gambletan/cortex

Now in English, Chinese, Japanese, and Korean.

---

## Hashtags
#Claude #ClaudeCode #AI #MCP #Rust #OpenSource #AIMemory #Privacy
