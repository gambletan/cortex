# Cortex × Meta Muse — give Muse your memory, keep your privacy

> **Preview.** Muse is US-only. Since v2.5 the gateway signs Muse in with OAuth (what Muse
> custom connectors use), and every sign-in must be approved on your own computer. Problems?
> [Open an issue](https://github.com/gambletan/cortex/issues).

Muse is far more useful when it knows you: your kid's peanut allergy, that you always take
the aisle seat, which coffee you like. Typical memory connectors for Muse keep your memory
on a hosted service, or on a self-hosted server. Muse can then read whole memory banks, and
an LLM processes everything you store.

Cortex works the other way round: **your memory archive stays on your device, and synced
copies are encrypted in your own cloud drive (iCloud / Google Drive / Dropbox / OneDrive)
with a key that only your devices hold.** Muse gets only the excerpts you allow, under a
daily budget, and you can cut it off with one command.

Don't connect your whole Google Drive to Muse. That gives Meta the plaintext of every file.
Connect Cortex instead, and Muse gets only what you put in the export.

## Connect in two steps (recommended)

1. Tell your AI (Claude, etc. with Cortex installed): **"connect my memory to Muse"**.
   It suggests what to share, you say which ones, and it gives you a link.
2. On your phone, paste the link into Muse. A Cortex page opens: tap **Allow**. Done.

Muse now works from your phone even when your computer is off. Later, just talk to your
AI: "also share that I'm vegetarian", "stop sharing my address", "what did Muse ask to
remember?", "disconnect Muse".

**How this keeps your privacy.** Only the memories you agreed to share go to Cortex
Cloud, the always-on service that answers Muse. Those are the memories Muse (Meta) will
see anyway. Everything else stays on your devices, and synced copies are encrypted in
your own drive. In Cortex Cloud, your shared memories live in their own encrypted
database; Muse can read at most 30 of them a day; and "disconnect Muse" deletes
everything there. To be precise: while it answers Muse, the Cortex Cloud server can read
the memories you shared. If you'd rather not use any server, run the gateway yourself
([self-hosted setup](#self-hosted-setup)).

**Turn off Muse's own memory.** Whatever Muse reads becomes visible to Meta, and Muse may
save its own copy into its built-in memory (in our first test it did: "I've also saved these
to my long-term memory"). Cortex asks Muse with every answer not to keep copies, but can't
enforce that. With Muse's memory off, Cortex is the only place your memory lives: Muse reads
it live each time, and unsharing or disconnecting really takes effect.

The link works once, for 30 minutes. If someone else used your link before you did, your
own Muse won't connect: ask your AI to connect again, which cancels any earlier
connection.

## Why this is different

| | Cortex gateway | Typical hosted/self-hosted memory connector |
|---|---|---|
| Where your memory lives | Your devices (SQLite). Synced copies are encrypted in your own drive | Vendor cloud or your server (plaintext DB) |
| Is your memory sent to an LLM to process? | No. No LLM in the pipeline | Yes, every save is processed by an LLM provider |
| What Muse can read | Only the memories you add to the export, one by one | Whole memory banks (scoping is per bank) |
| Can Muse write or delete? | No. It gets one read-only tool | Usually yes (retain, bank tools) |
| Daily cap on what leaves | Yes: requests and distinct memories per day | No |
| Kill switch | `gateway off` takes effect on the next request | Revoke the OAuth grant |
| Who can sign Muse in | Only you, by typing `gateway connect <code>` on your computer | Whoever completes the web login |
| Record of what was shared | Local audit log (ids, counts, outcome; never your query or text) | Vendor-side, often an enterprise feature |
| Preview before sharing | `gateway preview "<question>"` | — |
| Cost | Free, MIT, no account | Usage-based pricing or your own LLM bill |

Your private memories are never put into a response. The gateway's database query only
asks for the `muse-export` namespace, so other rows are never loaded at all. Revoking an
item takes effect on the next request, even while the server is running.

## Self-hosted setup

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

# 3. Give it an HTTPS address Muse can reach
tailscale funnel --bg 3316     # recommended: TLS terminates on YOUR machine
                               # → https://<machine>.<tailnet>.ts.net

# 4. Start the gateway (listens on 127.0.0.1 only) with OAuth for that address
cortex-mcp-server gateway serve --oauth --public-url https://<machine>.<tailnet>.ts.net
```

Use a tunnel that terminates TLS **on your machine**, such as Tailscale Funnel or an opaque
TCP relay. Tunnels that terminate TLS at their edge (for example `cloudflared` quick tunnels)
can see the plaintext of every request and response.

Run the gateway on a machine you own that stays on (a home mini-PC, a NAS, or a desktop).
When it is off, Muse simply can't reach your memory. That fails closed, which is
intentional. Don't run it on a rented cloud server: the server would hold your key, so you
would be trusting that provider instead of Meta.

5. In Muse, say *"create a custom connector"* and give it `https://<your-tunnel>/mcp`
   (MCP, OAuth). Muse opens a sign-in page that shows a code such as `K7QM-2XDF`.
6. On your computer, run the command the page shows and answer `y`:

   ```bash
   cortex-mcp-server gateway connect K7QM-2XDF
   ```

   The page then continues by itself and Muse is connected.

**Only approve a code that appeared right after *you* clicked Connect in Muse, on *your*
screen.** Anyone can open the sign-in page of your gateway, but nobody gets in unless you
run `connect`, so the one way in is to trick you into approving their code. The client
name shown by `connect` is whatever the client chose to call itself; it is not verified.
Nothing is ever typed into the sign-in page, so Meta's in-app browser never sees a password
or token it could reuse.

```bash
cortex-mcp-server gateway clients            # who is signed in
cortex-mcp-server gateway disconnect <id>    # sign one out (or --all), effective immediately
```

Muse gets a 1-hour access token and a 30-day refresh token that rotates on every use. If an
old refresh token is ever used again, which means someone holds a copy, that sign-in is
revoked and Muse has to be approved again. The gateway stores only hashes of tokens.

Prefer a fixed token (scripts, other MCP clients)? Set
`CORTEX_GATEWAY_TOKEN=$(cortex-mcp-server gateway token)` before `serve`; it works with or
without `--oauth`. `disconnect --all` does not affect it: unset it and restart.

## Let Muse save to *your* memory (`remember`, v2.4)

You can turn off Muse's built-in memory and have Muse save to Cortex instead:

```bash
cortex-mcp-server gateway serve --enable-remember     # adds the `remember` tool
cortex-mcp-server gateway inbox                       # what Muse asked to keep
cortex-mcp-server gateway approve <id>                # keep it (and let Muse recall it)
cortex-mcp-server gateway reject <id>                 # or: reject --all
```

- **Muse can only append.** It can't read, search, edit or delete the inbox. Muse gets
  back only "Saved for the user's review.", with no id and no echo of the text.
- **Nothing moves until you approve.** Inbox items aren't searchable, aren't embedded, and
  never sync. Approving an item makes it a normal Private memory and adds a copy to the
  export, so `recall_memory` can find it.
- **Bounded:** 1000 characters per item, 20 per day (`--daily-remembers`), 200 pending at
  most. Calls also count against the daily request budget. The kill switch applies.
- `gateway inbox` escapes control and bidi characters, so text written by Muse can't
  control your terminal or disguise what you're approving.

Approving means "Muse may see this", not "this is true". Muse saw the text in the
conversation anyway; what changes is that the long-term copy lives with you and can be
deleted with you, not with Meta.

## Day-to-day control

```bash
cortex-mcp-server gateway off        # kill switch: every request and sign-in refused, immediately
cortex-mcp-server gateway on
cortex-mcp-server gateway revoke <id>
cortex-mcp-server gateway audit      # what was disclosed, when (no query or snippet text)
```

Budgets: `serve --daily-requests 100 --daily-disclosures 30` are the defaults. The
disclosure budget counts *distinct* memories per UTC day, so Muse cannot drain your export
by asking many questions. Requests over budget are refused, and nothing is disclosed.

## What Meta can and cannot see

**Meta can see:** what you ask Muse, and the excerpts the gateway returns (bounded by your
export list and daily budget). Muse keeps those in its VM and conversation history.

**Meta cannot see:** everything else in your memory: the rest of the archive, your
history, people graph, beliefs, and anything Private. These never leave your devices
except as ciphertext in your own drive.

**Your cloud drive provider can see:** that encrypted Cortex files exist, plus their sizes,
timestamps and your account. It cannot see the contents.

## Honest limits

- What Muse receives lives in Meta's cloud (your Muse VM and conversation history) and
  cannot be recalled. Meta may use de-identified conversations for training. **Turn that off
  in Muse's settings.**
- Turning off Muse's own memory doesn't prove Meta deleted anything, and doesn't stop
  training on de-identified conversations. Use Muse's settings for that.
- Anyone who controls the machine running the gateway can read your memory. It holds the key.
- Redaction only catches email addresses. Treat the export list itself as your real
  control, and only export what you'd be comfortable telling Muse directly.
- Only `cortex-mcp-server gateway` should go through the tunnel. **Never tunnel
  `cortex-http`.** It is a local admin API with no authentication.

Design and threat model: [`docs/design/muse-gateway.md`](design/muse-gateway.md).
