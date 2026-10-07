# Design: Cortex Cloud for Muse — always-on, phone-friendly, export-only (v2.6)

Status: APPROVED (scope A, eng review 2026-10-06), Codex reviewed · Supersedes the tunnel paths in [`muse-oauth.md`](muse-oauth.md) as the default
(self-hosted gateway stays for purists)

## Problem

Muse is used mostly on phones. A gateway on the user's laptop is unreachable whenever the
laptop sleeps, and the setup (tunnel, flags, a second terminal) is beyond normal users.
Hindsight is easy because it hosts **all** memory in its cloud and runs every save through
an LLM. We want the same three-step setup without that.

## Principle

Split by what Meta can see anyway:

| Data | Where | Who can read it |
|---|---|---|
| Full memory archive | User's devices; synced copies E2E-encrypted in the user's own drive | Only the user's devices |
| **Export slice** (items the user chose for Muse, ≤ 1000) | Cortex Cloud | Cortex Cloud (encrypted at rest) and Muse/Meta when it asks |
| Muse `remember` proposals | Cortex Cloud inbox until the user decides | Cortex Cloud; never searchable before approval |

The cloud holds **only what the user already decided to give Meta**. Nothing else ever
leaves the device in plaintext.

## User flow — two actions, nothing to install, no account, no password

Cortex users already talk to their memory through an AI agent (Claude etc. via MCP). The
agent does everything; the user never touches a terminal.

1. User to their AI: "connect my memory to Muse". The agent calls `muse_connect`
   (MCP tool): it proposes what to share ("these 10 memories — OK?"), the user says yes,
   and the tool returns **one link**. Behind the scenes the device created its cloud
   identity (key in the OS keychain) and pushed the chosen slice.
2. Phone: paste the link into Muse. Muse runs OAuth; the Cortex page shows "Allow Muse to
   read the 10 memories you shared?" → **Allow**. Done — works with the laptop off.

The link is the only credential the human ever sees: an enrollment capability, single use,
30 min, shown only in the user's own AI chat. No passkey, QR, email or password.

Hardening that costs the user nothing (Codex review, revised after adversarial review):
- ~~Pairing code~~ — dropped: the code would be per window, so whoever races the link sees
  the same code; it detects nothing. Instead, a late click on a used link says **"This link
  was already used to connect. If that wasn't you, ask your AI to connect Muse again"**, and
  `muse_status` returns `muse_connected_at` with an instruction for the agent to ask the user
  "was that you?". Reconnecting revokes every earlier grant.
- **Sharing is two-step**: `muse_connect` / `muse_share` first return exactly what would be
  shared and a confirmation code bound to that exact list (15 min); nothing is shared until
  a second call carries the code. (A prompt-injected agent can still call twice; the MCP
  client's own per-tool approval is the human gate. This makes the agent show the list.)
- Only the enrollment hash is stored. GETs, link previews and discovery never consume it;
  **POST Allow** consumes the enrollment and mints the code in one SQL transaction.
- Consent page: session-bound CSRF token + **required** same-origin `Origin`, Secure/HttpOnly/SameSite cookie,
  frame-ancestors none, no-store, `Referrer-Policy: same-origin` (`no-referrer` would make browsers send `Origin: null` on the Allow POST), no third-party assets, link redacted from logs.
  The page binds client, exact redirect_uri, resource, PKCE challenge and the export
  version shown.
- After a connection the agent says once: "Muse connected at T. Was that you?" with a
  one-line revoke.
- `client_name` from DCR is never treated as proof that the client is Muse.

**Stated residual:** the link is a bearer capability. If it leaks (e.g. AI-vendor chat
history) and an attacker uses it first, they get a read grant on the shared slice; the
pairing code makes that visible but does not prevent it. Closing it fully would need an
extra owner step, which the product rejects.

Everything else is conversational: "share this with Muse too", "stop sharing X",
"disconnect Muse" (kill switch + revoke all), "what did Muse see this week?" (audit).
Muse `remember` proposals arrive in the cloud inbox; the device pulls them and the agent
asks in the next conversation ("Muse wants to remember X — keep it?").

## Scope (eng review decision: A — "tenant = directory")

The self-hosted gateway already is "one directory = one user" (export DB, budget, audit,
OAuth state, kill switch all live next to the DB). The cloud reuses it unchanged per tenant
instead of a new multi-tenant schema. New code is only: routing by tenant id, the device
API (signed), and enrollment-link consent. Owner decision: **`remember` and request
signing are in v2.6** (not deferred).

```
                      ┌──────────────── cortex-cloud (one process) ────────────────┐
Muse ── /mcp/<rid> ──▶│ route rid → tenant dir ─▶ Gateway (existing code, per tenant) │
     ── /t/<rid>/… ──▶│   OAuth (existing oauth.rs, per-tenant state + issuer)       │
device ── /api/… ────▶│ register · put export · new enrollment · delete tenant      │
                      │ tenants/<rid>/export.db  (SQLCipher, key = HKDF(master,rid)) │
                      │ tenants/<rid>/gateway-*.json|jsonl (budget, audit, oauth)    │
                      └─────── master key: /etc/cortex-cloud/master.key (0600) ──────┘
```

### Decisions from the eng review
1. **Isolation by directory.** Every request resolves exactly one tenant dir from the URL
   (`rid` = 128-bit random, validated charset) and never touches another. Tests assert a
   token from tenant A is useless on `/mcp/<B>`.
2. **At rest:** each tenant's export DB is SQLCipher (already the `cortex-core` default)
   with a raw 256-bit key = HMAC-SHA256(master key, "db" ‖ rid) — raw key
   (`Cortex::open_with_raw_key`), so no per-open PBKDF cost; one read connection per
   tenant. Inbox text sealed with AES-256-GCM under HMAC(master, "inbox" ‖ rid). Master key outside the data dir. **No backups** of tenant data: the device is
   the source of truth and re-pushes.
3. **OAuth per tenant:** issuer `https://<host>/t/<rid>` (RFC 8414 path issuer), PRM at
   `/.well-known/oauth-protected-resource/mcp/<rid>`, so `oauth.rs` state stays per
   directory. Consent mode `Enrollment` replaces `gateway connect` (CLI) on the cloud: the
   consent page shows the pairing code; POST Allow consumes the enrollment and marks the
   pending request approved in one locked transaction. If the spike shows Muse can't handle
   path issuers, fall back to one shared issuer with tenant bound in pending/code/grant.
4. **Device API, signed (Ed25519):** the device key lives in the OS keychain; every call
   carries `X-Cortex-Device: <key id>`, `X-Cortex-Timestamp`, `X-Cortex-Nonce` (128-bit) and
   `X-Cortex-Signature` over `method \n path?query \n sha256(body) \n timestamp \n nonce`.
   Server: ±300 s skew, nonce stored per tenant until it falls out of the window (file under
   the tenant lock, so replay protection survives restarts); a bad signature, stale time or
   reused nonce → 401. The tenant is created by the first signed `POST /api/tenants` (key
   registered; trust on first use; registration nonces persisted too). The tenant, the key
   and the timestamp are checked from headers **before any body byte is read**; bodies are
   capped per route (8 KiB, export 24 MiB — any valid snapshot fits) with an absolute read deadline. Registration is
   rate-limited per client network (IPv6 /64) and globally; at most 20 000 tenants; a tenant
   that never pushes within a day is reclaimed. Export pushes carry a `version` (device ms
   when the snapshot was taken) and are serialized per tenant; an older version is refused
   (409), so a slow stale push can't resurrect an unshared item. Export items ≤ 2000 chars,
   embeddings exactly 384-dim. Endpoints: `POST /api/tenants` (create; returns rid +
   token; rate limit per IP + global cap), `PUT /api/tenants/<rid>/export` (full snapshot:
   text + vector, ≤ 1000 items, replaces atomically), `POST …/enroll` (new link + pairing
   code; revokes existing grants), `GET …/inbox` + `POST …/inbox/ack` (remember items,
   below), `DELETE …` (wipe the directory). Tenants idle 90 days are deleted.
7. **`remember` (v2.6):** the existing gateway inbox, per tenant (append-only for Muse,
   1000 chars, 20/day, 200 pending, never searchable). The device pulls pending items on its
   next run; the agent asks "Muse wants to remember X — keep it?"; keep = local Private memory
   + added to the export (pushed back); either way the device acks and the cloud deletes the
   item. Inbox text is in the tenant's SQLCipher DB, not a plaintext jsonl.
5. **Local side:** MCP tools in `cortex-mcp-server`: `muse_connect` (pick → push → link),
   `muse_share` / `muse_unshare` (edit export + push), `muse_disconnect` (delete tenant),
   `muse_status` (what is shared, last access from audit). Cloud URL default compiled in,
   overridable by env.
6. **Failure:** cloud down → Muse gets an error, nothing else breaks; local Cortex never
   depends on the cloud.

### Code quality
- Refactor first, then build (no behaviour change in the first commit): split `gateway.rs`
  into the per-tenant core (disclosure, budgets, audit, inbox, OAuth) and the CLI/serve glue,
  so `cortex-cloud` links the core.
- One `oauth.rs` for both; consent is an enum (`LocalCli` | `Enrollment`), not a copy.

### Performance
- **One shared embedder** for all tenants (today each `Cortex` lazily loads its own model,
  ~90 MB each): construct once, inject into every tenant instance.
- Tenant `Cortex` handles in a true LRU cache (cap 64; ~2 file descriptors each), at most
  4 concurrent opens; the process raises its fd limit. Two concurrency lanes (device API
  64, Muse 192). Deletion renames the tenant dir out of the namespace first, so no request
  can resurrect it.
- Per-request cost stays as in the gateway (≤ 1000 rows scored, file-state reads).

## Architecture (original sketch)

```
laptop: cortex-mcp-server ──(device key, signed)──▶  Cortex Cloud (Rust, axum)  ◀──OAuth/MCP── Muse
   │  export slice push (full replace, versioned)        │ SQLite (WAL), export rows AES-GCM at rest
   │  inbox pull / ack                                   │ /mcp  /authorize /token /register /.well-known
   │  config (budgets, off)                              │ (no web account UI: control via the device)
   └ full archive stays local / E2E in own drive         └ Caddy in front (TLS, Let's Encrypt)
```

One new crate `cortex-cloud` (binary). Reuses from the gateway: OAuth logic (factored into a
shared module parameterized by tenant + storage), `rank`/redaction/caps, budgets, audit
format, terminal/HTML escaping. Embeddings: the device uploads each export item's vector
(same all-MiniLM-L6-v2); the server embeds only queries, with the same model
(`cortex-core` `embeddings` feature).

### Identity

- **Device**: Ed25519 key in the OS keychain (macOS), else a 0600 file next to the local
  database; every device API request is signed (see decision 4). No bearer device secret.
- **Human**: no account. Control happens on the device (through the agent), which is
  already the user's trust anchor. Consent for Muse = the enrollment link (single use,
  30 min, 256-bit) + one tap.
- **Muse**: OAuth 2.1 as in `muse-oauth.md` (DCR allowlist = Muse callback, PKCE, rotating
  refresh, reuse revocation, resource-bound tokens). `/authorize` without a live enrollment
  for that account → "ask your AI for a new Muse link". One enrollment → one grant.

### Storage

SQLite: `accounts`, `devices`, `enrollments`, `clients`, `pending`, `codes`, `grants`,
`exports(account, id, ciphertext, vector, updated)`, `inbox`, `budget`, `audit`. Export text
and inbox text encrypted with AES-256-GCM under a per-account data key, wrapped by a server
master key (file, 0600, not in the DB or backups). Tokens/codes/secrets: SHA-256 only.

### Limits (per account, same defaults as the gateway)

100 requests / 30 distinct disclosures / 20 remembers per UTC day, 1000 exports, 200 inbox,
1000 chars per item, 4 KiB response. Global: per-IP and per-account rate limits, body
limits, timeouts, separate lane for unauthenticated endpoints.

## Threat model (delta from the self-hosted gateway)

| Threat | Mitigation / residual |
|---|---|
| Cortex Cloud operator or a server compromise reads the export slice | **Residual, stated plainly**: the slice is plaintext to the running server (Muse needs plaintext answers). It is only what the user already chose to give Meta. Encrypted at rest; master key outside the DB |
| Server compromise reads the archive | Impossible: the archive is never sent |
| Stolen device key pushes a poisoned export | Signed requests only push the export (no read of other data); the agent can list what the cloud holds; "disconnect Muse" revokes devices' pushes and grants |
| Enrollment link leaks before use | 30 min, single use; the user sees Muse fail and asks for a new link, which revokes the stray grant (`muse_connect` revokes all grants by default) |
| Muse token theft | As in `muse-oauth.md` |
| Cross-tenant leak | Every query keyed by account id from the authenticated token; tests per endpoint |

## Link format and enrollment window

OAuth clients build `/authorize` from discovery metadata, not from the pasted URL, so a
query secret in the link (`?e=`) would not reach the consent step. Instead:

- The link is the tenant's MCP URL: `https://<host>/t/<rid>/mcp`. `rid` is 128-bit random,
  shown only in the user's AI chat.
- `muse_connect` asks the cloud (signed) to open an **enrollment window**: 30 min, one
  grant, with a 4-digit pairing code the agent tells the user. Opening a window revokes
  existing grants.
- `/t/<rid>/authorize` outside a window → "Ask your AI for a new Muse link". Inside: the
  consent page with the pairing code and **Allow** (POST, CSRF + Origin checked). Allow marks
  the request approved and closes the window in one locked step; the existing wait/code
  machinery then completes OAuth.
- Discovery: issuer `https://<host>/t/<rid>`; metadata served at both the RFC 8414/9728
  inserted forms (`/.well-known/oauth-authorization-server/t/<rid>`,
  `/.well-known/oauth-protected-resource/t/<rid>/mcp`) and the appended forms under
  `/t/<rid>/.well-known/...`.

The Muse spike still checks: which discovery URLs it fetches, DCR body, and that path
issuers work. Fallback if they don't: one shared issuer with the tenant bound in
pending/code/grant.

## Multi-tenant OAuth (from `oauth.rs`)

Tenant + export version are carried immutably through pending → code → grant → request
context (`{tenant, grant, scope, resource}`), all in SQLite transactions with constraints
(enrollment and code consumption, refresh rotation). DCR clients may be shared across
tenants, so a client id is never a tenant identity. Cross-tenant and concurrent-redeem
tests are mandatory.

## MVP cuts / must-keep

Cut first: continuous sync (push a full snapshot on change), multiple grants per tenant
(one active Muse grant), any dashboard.
Never cut: atomic redemption, tenant isolation, consent CSRF, PKCE/resource/callback checks,
revoke and delete, log redaction, at-rest key separate from the DB, AES-GCM nonce
discipline, the plainly stated server-sees-the-slice residual.

## Test plan

```
NEW FLOW                                   TESTS (cortex-cloud acceptance unless noted)
muse_connect (local MCP tool) ───────────── unit: picks only confirmed ids; push payload has no
                                             non-export rows; returns link + pairing code
signed device API ───────────────────────── bad sig / stale ts / reused nonce (also after
                                             restart) → 401; key of tenant A on B → 401
POST /api/tenants ───────────────────────── rate limit; global cap
PUT export ──────────────────────────────── replaces atomically; >1000 refused; wrong token 401;
                                             token of tenant A on tenant B 401
POST enroll ─────────────────────────────── revokes old grants; link single use; 30 min expiry
discovery /mcp/<rid> ────────────────────── PRM + AS metadata per tenant; unknown rid 404
/authorize + consent GET ────────────────── shows pairing code; GET never consumes enrollment;
                                             link preview (HEAD/GET) harmless
POST Allow ──────────────────────────────── CSRF token + Origin required; consumes enrollment
                                             atomically; concurrent Allow → one grant
/token, refresh, /mcp ───────────────────── existing oauth acceptance suite, per tenant
cross-tenant ────────────────────────────── grant A on /mcp/B 401; audit/budget files separate
DELETE tenant / disconnect ──────────────── directory gone; tokens 401; rid 404
at rest ─────────────────────────────────── export.db unreadable without key (SQLCipher)
remember → inbox ────────────────────────── Muse append-only; caps; not searchable; device
                                             pull + ack deletes; keep → appears in recall after push
shared embedder ─────────────────────────── 50 tenants → one model load (unit)
Muse spike (manual, real account) ───────── path + query preserved; discovery URL; DCR body
```

Failure modes: SQLCipher key mismatch after master-key rotation → tenant 500 (test: clear
error, device re-push recovers); enrollment consumed but redirect lost → user asks for a new
link (tested); disk full on push → 507 and old export kept (atomic replace, tested).

## Not in scope (v2.6)

A web dashboard, backups of tenant data, billing, multi-device conflict UI (last push wins, versioned), web editing of the export,
non-Muse clients (same OAuth works, not marketed), E2E-encrypted export (impossible while
Muse needs plaintext).

## Open decisions for the owner

1. Server and domain (TLS needs a domain; e.g. `muse.<our domain>`).
2. Free for everyone at launch? (Cost is one small server.)
3. Keep the laptop gateway as the "fully self-hosted" option (recommended: yes).
