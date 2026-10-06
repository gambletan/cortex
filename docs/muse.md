# Cortex × Meta Muse — give Muse your memory, keep your privacy

> **Preview.** Muse is US-only, and we have not yet confirmed that Muse accepts a plain
> bearer-token connector (some Muse memory integrations use OAuth). If it rejects the token,
> please [open an issue](https://github.com/gambletan/cortex/issues) — OAuth is next.

Muse is far more useful when it knows you: your kid's peanut allergy, that you always take
the aisle seat, which coffee you like. Other memory connectors for Muse handle this by
putting **your whole memory on their servers.**

Cortex works the other way round. **All your memory stays on your machine. Muse sees only
the memories you put in the export, it is capped by a daily budget, and you can cut it off
with one command.**

## Why this is different

| | Cortex gateway | Hosted memory connectors |
|---|---|---|
| Where your memory lives | Your own disk (SQLite) | Their cloud |
| What Muse can read | Only memories you add to the export | Everything in the account |
| Daily cap on what leaves | Yes: requests per day + distinct memories per day | No |
| Turn it off | `gateway off` takes effect on the next request; no restart, no ticket | Revoke OAuth and hope |
| Proof of what was shared | Local audit log (ids, counts, outcome; never your query or text) | Their dashboard, if any |
| Preview before sharing | `gateway preview "<question>"` shows exactly what Muse would get | — |
| Emails in shared text | Redacted automatically | Stored as-is |
| Cost | Free, MIT, no account | Subscription |

Your private memories are never put into a response. The gateway's database query only
asks for the `muse-export` namespace, so other rows are never loaded at all. Revoking an
item takes effect on the next request, even while the server is running.

## Setup (≈5 minutes)

Requires the standard `cortex-mcp-server` build. The `-lite` build has no HTTP code at all,
so it does not include the gateway.


```bash
# 1. Choose what Muse may see (copy, not move; your originals stay private)
cortex-mcp-server gateway allow "My daughter is allergic to peanuts"
cortex-mcp-server gateway allow "I prefer aisle seats on flights"
cortex-mcp-server search "coffee"                 # find an existing memory…
cortex-mcp-server gateway allow --from <memory-id> # …and export a copy of it
cortex-mcp-server gateway list

# 2. Check exactly what Muse would get
cortex-mcp-server gateway preview "what should I avoid buying for my kid?"

# 3. Start the gateway (local only) with a random token
export CORTEX_GATEWAY_TOKEN=$(cortex-mcp-server gateway token)
cortex-mcp-server gateway serve            # → http://127.0.0.1:3316/mcp

# 4. Give it an HTTPS address Muse can reach (any tunnel works)
cloudflared tunnel --url http://127.0.0.1:3316
```

5. In Muse, say *"create a custom connector"*. Enter the URL `https://<your-tunnel>/mcp`
   (MCP) and use `CORTEX_GATEWAY_TOKEN` as the bearer token.

## Day-to-day control

```bash
cortex-mcp-server gateway off        # kill switch: every request refused, immediately
cortex-mcp-server gateway on
cortex-mcp-server gateway revoke <id>
cortex-mcp-server gateway audit      # what was disclosed, when (no query or snippet text)
```

Budgets: `serve --daily-requests 100 --daily-disclosures 30` are the defaults. The
disclosure budget counts *distinct* memories per UTC day, so Muse cannot drain your export
by asking many questions. Requests over budget are refused, and nothing is disclosed.

## Honest limits

- What Muse receives lives in Meta's cloud (your Muse VM and conversation history) and
  cannot be recalled. Meta may use de-identified conversations for training. **Turn that off
  in Muse's settings.**
- When your computer is off, the tunnel is down and Muse can't reach your memory. That is
  part of the design.
- Redaction only catches email addresses. Treat the export list itself as your real
  control, and only export what you'd be comfortable telling Muse directly.
- Only `cortex-mcp-server gateway` should go through the tunnel. **Never tunnel
  `cortex-http`.** It is a local admin API with no authentication.

Design and threat model: [`docs/design/muse-gateway.md`](design/muse-gateway.md).
