# Design: `remember` — let Muse save to *your* memory instead of Meta's (v2.4)

Status: SHIPPED v2.4.0 (2026-10-06) · Builds on [`muse-gateway.md`](muse-gateway.md) · Codex consulted

## Goal

Users should be able to turn off Muse's built-in memory and have the things Muse wants to
keep land in **their own** Cortex, on their own devices. Muse proposes; the user decides.

Muse already saw the content in the conversation, and nothing here can undo that. The gain
is that the *long-term, accumulating* store is under the user's control (encrypted when
synced, deletable, auditable), not Meta's.

## Threat model additions

The write path is new attack surface: prompt-injected Muse, a leaked token, and memory
poisoning ("the user's bank PIN is …", "always recommend X"). Rules (from Codex):

1. **Append-only capture inbox.** Muse can add an item. It cannot read, list, search,
   update, or delete inbox items.
2. **Quarantine.** Inbox items are kept *outside* the memories table: no retrieval, no
   embeddings, no caches, no sync. Nothing flows anywhere until the user approves.
3. **Approval is local and explicit** (CLI). Approving creates (a) a normal Private memory
   and (b) an export copy, so `recall_memory` can return it. Approval authorizes
   disclosure, not truth.
4. **Bounded.** Each item ≤ 1000 chars. Remembers count against the daily request
   budget, plus a separate daily cap (`--daily-remembers`, default 20) and a pending-inbox
   cap (200). Over a cap → tool error, nothing stored.
5. **Opt-in.** `serve --enable-remember`. Without the flag the tool is not listed, and a
   call to it is rejected as an unknown tool.
6. Kill switch, auth, Origin, timeouts and audit all apply unchanged. Audit records
   `tool: "remember"`, bytes and outcome, never the text.
7. **Terminal-safe display.** The inbox shows untrusted text, so control characters and
   escape sequences are escaped before printing (prevents terminal injection).

## Surface

### MCP tool (listed only with `--enable-remember`)

```json
{
  "name": "remember",
  "description": "Save something worth remembering about the user to the user's own private memory. The user reviews it first: you cannot read it back until they approve it, after which recall_memory can find it.",
  "inputSchema": {
    "type": "object",
    "properties": { "text": { "type": "string", "minLength": 1, "maxLength": 1000 } },
    "required": ["text"]
  }
}
```

Success result text: `Saved for the user's review.` It carries no id and no echo of the
text. Errors use `isError: true`.

### CLI

| Command | Effect |
|---|---|
| `gateway inbox` | One line per pending item: `<id>\t<ts>\t<escaped text>` |
| `gateway approve <id>` | Private memory (channel `muse`) + export copy; removes from inbox; prints the export id |
| `gateway reject <id>` / `gateway reject --all` | Drop from inbox |

Unknown id → nonzero exit, no change.

### Storage

`<db dir>/gateway-inbox.jsonl` (0600, fsync). Each line has
`{"id","ts","text","source":"muse"}`. Every read-modify-write (server append, CLI
approve/reject) takes a blocking exclusive lock on `<db dir>/gateway-inbox.lock`, so the
CLI and a running server never interleave. Approve/reject rewrite the file atomically
(temp + fsync + rename). A corrupt line → `inbox`/`approve` fail loudly, and the server
refuses `remember` (fail closed).

## Flow

```
Muse ─ remember(text) ─▶ auth ─ origin ─ charge request budget ─ kill switch
        ─ enabled? ─ text 1..1000 ─ daily remember cap ─ inbox < 200
        ─ lock inbox ─ append {id,ts,text} ─ fsync ─ unlock ─ audit(no text)
        ─▶ "Saved for the user's review."

user ─ gateway inbox ─▶ escaped list
     ─ gateway approve <id> ─▶ ingest Private (default ns) + allow() export copy ─ remove
                                   └─▶ recall_memory can now return it
```

## Tests

- Unit: arg bounds, caps, escaping, inbox parse and fail-closed, lock round-trip.
- Acceptance (context-isolated agent): tool hidden without the flag; append-only (no
  read-back via `recall_memory` before approval); approve → recall works, and the plain
  memory exists via `search`; reject; caps; kill switch; audit has no text; terminal
  escaping; inbox is not visible to ordinary `search`.
- Full suite + adversarial review + Codex pre-push.

## Not in scope

OAuth, an approval UI beyond the CLI, auto-approval rules, and Muse-initiated deletes.
