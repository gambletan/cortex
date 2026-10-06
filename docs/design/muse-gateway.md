# Design: Muse gateway (`cortex-mcp-server gateway`) — explicit, budgeted memory disclosure to Meta Muse

Status: APPROVED (2026-10-06) · Reviewed by: Codex (advisor) + eng review (see bottom)

## Goal

Let a Meta Muse user give Muse access to **a small, explicitly chosen slice** of their
Cortex memory, while everything else stays on their device — and make every disclosure
visible, bounded, and stoppable.

Positioning (honest): *"Your memory lives on your machine. Muse only sees what you
explicitly export, within a budget you set, and you can cut it off instantly."*
This feature does **not** claim "100% local" — anything returned to Muse lives in Meta's
cloud (Muse VM + conversation trajectory) and cannot be recalled.

## Constraints (verified 2026-10-06)

- Any Muse user (US) can add a custom connector; Meta does not review them.
- Connectors: remote MCP (streamable HTTP) or OpenAPI; auth bearer / API key / OAuth.
- Muse runs in Meta's cloud → endpoint must be public HTTPS (user runs a tunnel).
- Hindsight's self-hosted Muse path uses OAuth 2.1 + DCR → **bearer-only acceptance by
  Muse is unverified** (spike before claiming it works; see Risks).

## Non-goals (MVP)

- No write tools (no ingest from Muse — memory poisoning risk).
- No `memory_context`, people, beliefs, facts, sync, delete, admin.
- No OAuth (follow-up if the spike shows Muse requires it).
- No change to `cortex-http` (it has no auth and CORS `*`; it must never be tunneled).

## Architecture

A **subcommand of `cortex-mcp-server`**: `cortex-mcp-server gateway <serve|token|allow|list|
revoke|preview|audit|off|on>`. `gateway serve` is a separate long-running process on its own
port; it opens the same SQLite DB and exposes exactly one MCP tool. No shared router with
`cortex-http`, no dashboard, no admin routes. Ships in the existing release tarballs and
Docker image; reuses the MCP JSON-RPC types and `redact_emails`.

```
Muse (Meta cloud) ──HTTPS──▶ tunnel (cloudflared) ──▶ 127.0.0.1:3316 cortex-mcp-server gateway
                                                         │  auth → kill switch → budget
                                                         │  → retrieve(ns = export ns)
                                                         │  → re-check ns → redact → cap
                                                         └─ audit log (metadata only)
```

### Disclosure allowlist = a dedicated namespace (default `muse-export`)

- Only memories whose `namespace == export_ns` are ever returned. **Not** Shared/Public —
  the sync boundary is not consent to disclose to Meta.
- **No global index, no cache.** Every request reads only `namespace = export_ns` rows
  straight from SQLite (all tiers, ≤ 1000 rows), scores them locally (cosine on stored
  embeddings vs. the query embedding; keyword overlap when embeddings are off), and returns
  top-k. Non-export rows are never loaded, and `revoke` takes effect on the very next request
  even though it runs in another process (the stale-cache class of Iteration 17 cannot occur).
- Defense in depth: `mem.namespace == export_ns` is re-checked on every row right before
  serialization.
- `allow` refuses once the export holds 1000 memories (the scan bound).
- Users add items explicitly:
  - `cortex-mcp-server gateway allow "<text>"` → ingests into `export_ns` (Private, so it never syncs).
  - `cortex-mcp-server gateway allow --from <memory-id>` → copies an existing memory's text into
    `export_ns` (copy, not move: the original keeps its privacy/namespace).
  - `cortex-mcp-server gateway list` / `cortex-mcp-server gateway revoke <id>` (deletes the export copy).
  - The namespace is **reserved**: core rejects ordinary ingest (single, batch, MCP, HTTP, bindings) and sync-peer writes into it. The gateway additionally serves only rows `allow` created (Private + content-hash marker). `import` of a user's own backup restores it as-is.

### MCP surface (streamable HTTP, JSON responses only)

`POST /mcp` JSON-RPC 2.0: `initialize`, `notifications/initialized` (→ 202), `ping`,
`tools/list`, `tools/call`. `GET /mcp` → 405 (no SSE). Anything else → 404.

One tool:

```json
{
  "name": "recall_memory",
  "description": "Search the user's exported personal memory. Returns only what the user explicitly shared with Muse.",
  "inputSchema": {
    "type": "object",
    "properties": {
      "query": {"type": "string", "maxLength": 500},
      "limit": {"type": "integer", "minimum": 1, "maximum": 5, "default": 3}
    },
    "required": ["query"]
  }
}
```

Result: `content: [{type:"text", text: <JSON {results:[{text, created_at}]}>}]`.
No ids, scores, embeddings, channel, person, metadata.

### Security controls

| Control | Detail |
|---|---|
| Auth | `Authorization: Bearer <token>`; token from `CORTEX_GATEWAY_TOKEN`, ≥32 chars or refuse to start; constant-time compare; 401 without detail. `cortex-mcp-server gateway token` prints a fresh random token. |
| Bind | `127.0.0.1` default; warn loudly on non-loopback bind. |
| Origin | Request with an `Origin` header not in `--allow-origin` → 403 (DNS-rebinding defense per MCP spec). |
| Body | ≤ 64 KiB; JSON-RPC batch arrays rejected. |
| Per-request cap | `limit` ≤ 5; each snippet ≤ 500 chars; total response ≤ 4 KiB. |
| Cumulative budget | Per UTC day: ≤ `--daily-requests` (default 100) calls, ≤ `--daily-disclosures` (default 30) **distinct** memories disclosed. Re-returning an already-disclosed memory costs nothing. Every authenticated call is charged *before* any other check (so refusals are not a free search oracle). Over the request budget → tool error. Over the disclosure budget → unseen matches are **withheld silently** (indistinguishable from no match). Persisted (atomic + fsync, 0600) in `<db dir>/gateway-state.json` so restarts don't reset it; one `serve` per DB (file lock). |
| Kill switch | If `<db dir>/gateway.disabled` exists (or can't be checked), every tool call fails closed. `cortex-mcp-server gateway off` / `on`. Checked per request (no restart). |
| Redaction | Emails redacted before serialization (shared redactor moved to `cortex-core::redact`). |
| Audit | `~/.cortex/gateway-audit.jsonl`: `ts, method, tool, n_results, memory_ids, bytes, outcome`. **No query text, no snippet text.** `cortex-mcp-server gateway audit` prints it. |
| Preview | `cortex-mcp-server gateway preview "<query>"` prints exactly what Muse would receive (same code path, no budget/audit charge). |

### Code layout (post-review)

- `cortex-core/src/lib.rs` — add `pub fn embed_query(&self, text) -> Option<Vec<f32>>` (wraps the private `auto_embed`).
- `cortex-mcp-server/src/gateway.rs` — HTTP + MCP dispatch + CLI actions.
- `cortex-mcp-server/src/gateway/disclose.rs` — gate, scoring, caps, budget, audit (pure functions where possible).
- `cortex-mcp-server/src/main.rs` — `Gateway` subcommand wiring. `Cargo.toml` — `axum`.
- `docs/muse.md` + README section. Release workflows unchanged (same binary).

## Tests

- Implementer unit tests: gate (non-export memories never returned, incl. Private/Shared/
  Public in other namespaces), caps, budget rollover + persistence, kill switch, auth,
  origin, batch rejection, redaction.
- **Acceptance tests by a context-isolated agent** that sees only this doc + the tool
  schema (project rule), driving the real binary over HTTP.
- Adversarial review (third context) before merge.
- Full suite: `cargo test --workspace --exclude cortex-python --exclude cortex-wasm`.

## Risks / open questions

1. **Muse may require OAuth 2.1 + DCR** (Hindsight precedent). Mitigation: ship as
   "preview"; spike with a real Muse account (US) before marketing it as working. If needed,
   follow-up adds OAuth with DCR in front of the same gate.
2. Disclosed snippets are irrecoverable (Muse VM, trajectory, possibly training after
   de-identification). Docs tell users to disable training use in Muse settings.
3. Tunnel exposes a local process to the internet: single tool, auth first, no admin
   surface, budget caps the blast radius of a leaked token.
4. Semantic leakage: redaction only catches emails; the allowlist is the real control.

## Eng review outcome (2026-10-06)

Decisions: (1A) subcommand of `cortex-mcp-server`, not a new crate — fewer files, same
release artifacts, DRY with JSON-RPC + redaction. (2A) per-request direct read of the export
namespace, no shared index/cache — fixes a revoke-not-honored stale-cache leak in the original plan.

Failure modes handled (fail closed unless noted):

| Path | Failure | Handling |
|---|---|---|
| budget state file | corrupt / unreadable | tool call refused; never silently reset |
| audit append | write error | tool call refused (no disclosure without a record) |
| embedding model | first-load download / unavailable | preloaded at `serve` start; falls back to keyword scoring |
| export namespace | > 1000 rows (written by another client) | scan capped at 1000 most recent; `serve` logs a warning |
| kill switch | file created while serving | checked per request, effective immediately |
| token | env missing / < 32 chars | `serve` refuses to start |

Test diagram:

```
POST /mcp ─┬─ no/bad bearer ............................ 401          [unit]
           ├─ Origin not allowed ....................... 403          [unit]
           ├─ body > 64KiB / batch array ............... 413 / -32600 [unit]
           ├─ initialize / ping / tools/list ........... ok, 1 tool   [unit+accept]
           └─ tools/call recall_memory
                ├─ kill switch on ...................... error, no data [unit+accept]
                ├─ over request/disclosure budget ...... error, no data [unit+accept]
                ├─ query > 500 / limit out of range .... clamped/error  [unit]
                └─ gate: export rows only ............. Private/Shared/Public outside ns never returned [unit+accept]
                     ├─ revoke in another process ...... gone next call [accept]
                     ├─ redaction + 500-char/4KiB caps .. [unit]
                     └─ audit line w/o query/snippet ... [unit+accept]
GET /mcp → 405                                                       [unit]
CLI: token / allow / allow --from / list / revoke / preview / audit / off / on   [accept]
```

NOT in scope: OAuth 2.1 + DCR (spike first), write tools, `memory_context`, multi-user,
tunnel automation. Existing code reused: MCP JSON-RPC types, `redact_emails`, storage
namespace query, embedder, release pipeline.

## Pre-push Codex review fixes (2026-10-06)

- Gateway and its HTTP stack (`axum`, `tower`, `tower-http` → `hyper`) sit behind the
  `gateway` cargo feature (default on). The `--no-default-features` lite binary keeps
  **zero** HTTP/network crates (verified with `cargo tree`).
- The 4 KiB cap is enforced on the tool result as serialized (results JSON is embedded as a
  string, so escaping doubles), with an escaping-heavy regression test.

## Adversarial review fixes (2026-10-06)

Third-context review found no HIGH/CRITICAL. Fixed: (M1) refusals were uncharged → unmetered
boolean search oracle; now every call is charged first and disclosure-budget overflow
withholds silently, audit log rotates at 10 MiB. (M2) export namespace was writable by any
ingest/sync path → reserved in core + gateway requires the `allow` marker. (M3) no timeouts →
10 s request timeout + 16 concurrent requests (header-phase slowloris is absorbed by the
tunnel). LOW: kill switch fails closed on I/O error; token needs ≥ 8 distinct chars; budget
writes fsync'd, 0600, unique temp name; single `serve` per DB via file lock; 401 carries
`WWW-Authenticate: Bearer`. Not changed: `revoke` deletes the row but SQLite free pages/WAL
may retain bytes until VACUUM — the goal of revoke is to stop disclosure, and the text is a
copy of something the user already holds locally.
