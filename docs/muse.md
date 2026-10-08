# Cortex × Meta Muse — give Muse your memory, keep your privacy

Your memory archive stays on your device. Optional encrypted sync stores ciphertext in
your own cloud drive. Connecting Muse explicitly shares selected excerpts with Cortex
Cloud and Meta; the rest of your archive is not uploaded by this integration.

The hosted service is `https://cortex.alvinsclub.ai`. Custom-connector availability depends
on your Muse account. Problems? [Open an issue](https://github.com/gambletan/cortex/issues).

## Connect in two steps (recommended)

1. Tell your AI (Claude, etc. with Cortex installed): **"connect my memory to Muse"**.
   It previews the exact excerpts, asks for your confirmation, and gives you a personal
   `https://cortex.alvinsclub.ai/t/<public-id>/mcp` link. Use that complete link, not the site root.
2. On your phone, paste the link into Muse. A Cortex page opens: tap **Allow**. Done.

Muse now works from your phone even when your computer is off. Later, just talk to your
AI: "also share that I'm vegetarian", "stop sharing my address", "what did Muse ask to
remember?", "disconnect Muse".

**How this keeps your privacy.** Only the memories you agreed to share go to Cortex
Cloud, the always-on service that answers Muse. Those are the memories Muse (Meta) will
see anyway. Everything else stays on your devices, and synced copies are encrypted in
your own drive. In Cortex Cloud, your shared memories live in a tenant-specific encrypted
database; Muse can read at most 30 of them a day; and "disconnect Muse" deletes
everything there. To be precise: while it answers Muse, the Cortex Cloud server can read
the memories you shared. If you prefer to host the service on your own machine, run the gateway yourself
([self-hosted setup](#self-hosted-setup)).

**Turn off Muse's own memory.** Whatever Muse reads becomes visible to Meta, and Muse may
save its own copy into its built-in memory (in our first test it did: "I've also saved these
to my long-term memory"). Cortex asks Muse with every answer not to keep copies, but can't
enforce that. Turn off Muse's own memory to reduce retained copies. Unsharing or
disconnecting stops future Cortex reads; it cannot delete copies Meta already retained,
and turning off Muse's memory does not guarantee those copies are erased.

The link works once, for 30 minutes. If someone else used your link before you did, your
own Muse won't connect: ask your AI to connect again, which cancels any earlier
connection.

## Existing connections: reconnect on the new domain

If your Muse connector used `studio.alvinsclub.ai`, ask your AI to run `muse_connect`
again, then connect Muse using the new link and tap **Allow**. Reconnecting revokes earlier
grants and invalidates the previous public link. The old device-management routes remain
available during migration, so an existing device can request the new link without
manually editing its saved state.

## Choose hosted or self-hosted

| Control | Hosted Cortex Cloud (recommended) | Self-hosted gateway |
|---|---|---|
| Availability | Works while your computer is off | Your gateway machine must stay on |
| Data exposed to the host | Only your approved export and Muse inbox proposals | The host can access the local database |
| Consent | Personal enrollment link and web **Allow** | Approve the displayed code on your computer |
| Stop sharing | `muse_unshare`; `muse_disconnect` deletes the cloud tenant | `gateway revoke`; `gateway off` suspends access |
| Read history | `muse_status`, backed by the tenant's audit log | `gateway audit`, local metadata-only log |
| Save proposals | Append-only inbox; review with `muse_inbox` | Optional `--enable-remember`; review with `gateway inbox` |

Both modes disclose only the `muse-export` namespace and enforce daily budgets. Approved
exports are copies: the original memory can remain Private. Private classification does
not prevent you from explicitly approving a separate copy for Muse.

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

## Let Muse propose memories (`remember`)

With the hosted service, ask your AI to run `muse_inbox`. Review each proposal, then
choose **keep** or **discard**. Keeping creates a local Private memory and a shared export
copy; discarding removes the proposal. Muse cannot search, edit or delete inbox items.

For the self-hosted gateway, enable proposals explicitly:

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

Approving means "Muse may see this", not "this is true". Your approved primary memory
is stored locally, and its export copy is available to Muse. Approval does not erase any
copy Meta already retained.

## Day-to-day control

For hosted connections, ask your AI to use:

- `muse_status`: shared excerpts, connection time, today's reads and pending proposals.
- `muse_share`: preview and confirm additional excerpts.
- `muse_unshare`: remove ids from the status tool's `shared` list.
- `muse_disconnect`: revoke access and delete the cloud tenant, including its inbox and audit.

Check that a cloud update succeeded. If a push fails, Muse may still see the previous
export until a later Muse action successfully reconciles it. Read counts cover retained
audit records for **currently shared items**, per UTC day; they are not a complete lifetime
history. Unreadable audit state is reported as `muse_read_today: null` with
`muse_read_today_error`, rather than an empty history. Inbox errors are also explicit.

For self-hosted connections only:

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
export list and daily budget). Meta may retain what it receives.

**Meta cannot see:** everything else in your memory: the rest of the archive, your
history, people graph and beliefs, unless you explicitly choose to export their contents.
Cortex does not send unselected archive contents to Muse.

**Your cloud drive provider can see:** that encrypted Cortex files exist, plus their sizes,
timestamps and your account. It cannot see the contents.

## Honest limits

- Cortex cannot recall responses already delivered to Meta or delete copies Meta retained.
  Review Meta's own memory and data controls; Cortex does not enforce those policies.
- Turning off Muse's own memory can reduce retained copies, but does not prove deletion.
- The Cortex Cloud operator can read the shared slice and inbox proposals while serving
  requests. Encryption at rest does not hide these from the running service.
- Anyone controlling a self-hosted gateway machine can read its local database.
- Redaction only catches email addresses. Treat the export list itself as your real
  control, and only export what you'd be comfortable telling Muse directly.
- Only `cortex-mcp-server gateway` should go through the tunnel. **Never tunnel
  `cortex-http`.** It is a local admin API with no authentication.

Design and threat model: [hosted service](design/muse-cloud.md) and
[self-hosted gateway](design/muse-gateway.md). Operators: [deployment guide](../deploy/cortex-cloud/README.md)
and [verified release](../deploy/cortex-cloud/RELEASE_2026-10-07.md).
