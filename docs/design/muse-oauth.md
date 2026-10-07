# Design: OAuth 2.1 for the Muse gateway (v2.5)

Status: APPROVED for implementation (2026-10-06) · Builds on [`muse-gateway.md`](muse-gateway.md),
[`muse-remember.md`](muse-remember.md) · Codex consulted (all P0/P1 adopted; deviations noted)

## Why

Muse custom connectors sign in with OAuth: discovery, dynamic client registration (DCR),
PKCE, callback `https://agent.meta.ai/api/hatch/oauth/callback`. The v2.3 gateway only
takes a static bearer token, so Muse cannot connect. This adds a minimal, single-user
OAuth 2.1 authorization server built into `gateway serve`, following the MCP authorization
spec (RFC 9728 + RFC 8414 + RFC 7591 + PKCE + RFC 8707).

## The rule

The gateway URL is public (tunnel). Anyone on the internet can register a client and
start an authorization. **Nobody gets a token unless the user confirms the request on
their own computer** (`gateway connect <CODE>`, interactive y/N). The browser page has no
password field: nothing reusable is ever typed into a browser that may be Meta's in-app
webview.

**Residual risk (not closed):** relay phishing. An attacker can register a client named
"Muse", start an authorization, and talk the user into running `gateway connect` with the
attacker's code; the attacker then reads the redirect from the wait page and redeems the
code with their own PKCE verifier. The callback allowlist does not stop this. Mitigations
only reduce it: `connect` shows the full redirect URI, resource, permission, request age,
labels the client name as *unverified*, and requires typing `y`; docs say "only approve a
code that appeared right after **you** clicked Connect in Muse, on **your** screen".

## Surface

`serve --oauth --public-url https://<host>`. The public URL must be https with no path,
query or fragment; it is the issuer, and the protected resource (audience) is
`<public-url>/mcp`. With `--oauth`, `CORTEX_GATEWAY_TOKEN` is optional (still accepted if
set; `disconnect --all` does not revoke it — unset it and restart).

### Discovery

| Endpoint | Body |
|---|---|
| `GET /.well-known/oauth-protected-resource/mcp` and the root alias `/.well-known/oauth-protected-resource` | `{resource: "<issuer>/mcp", authorization_servers: [issuer], bearer_methods_supported: ["header"], scopes_supported: ["memory"]}` (identical on both) |
| `GET /.well-known/oauth-authorization-server` | `issuer`, `authorization_endpoint`, `token_endpoint`, `registration_endpoint`, `scopes_supported: ["memory"]`, `response_types_supported: ["code"]`, `grant_types_supported: ["authorization_code","refresh_token"]`, `code_challenge_methods_supported: ["S256"]`, `token_endpoint_auth_methods_supported: ["none","client_secret_post","client_secret_basic"]`, `authorization_response_iss_parameter_supported: true` |

No `openid-configuration` (add only if Muse is shown to need it).

`POST /mcp` without a valid token → `401` with
`WWW-Authenticate: Bearer resource_metadata="<issuer>/.well-known/oauth-protected-resource/mcp", scope="memory"`
(plus `error="invalid_token"` when a token was presented).

### `POST /register` (RFC 7591)

- JSON, ≤ 8 KiB. `redirect_uris` required, 1–5 entries, each ≤ 512 chars, **exact match**
  against the allowlist (default: Muse's callback; more via repeatable
  `--oauth-redirect <uri>`, which must be https or `http://127.0.0.1|localhost`, plain host[:port], no userinfo). No
  fragments.
- `client_name` ≤ 100 chars, no control/bidi/invisible characters (stored, shown only as
  unverified, on the sign-in page too). Other metadata
  (`logo_uri`, `client_uri`, …) is ignored and never fetched.
- `grant_types` ⊆ {authorization_code, refresh_token} (omitted → both, returned as the
  effective metadata, enforced at `/token`); `response_types` ⊆ {code}; else
  `invalid_client_metadata`.
- `token_endpoint_auth_method`: `none` | `client_secret_post` | `client_secret_basic`;
  **omitted means `client_secret_basic`** (RFC 7591 default) and a secret is issued.
  The effective metadata is returned and the method is enforced on every token request
  (no downgrade). A secret only binds later requests to the registrant; it does not prove
  the client is Meta.
- Max 50 clients. When full, evict the oldest client that has no grant, no pending request
  and no unredeemed code; if none → `503`. Rate limit 10/min.

### `GET /authorize`

Validation order: `client_id` known and `redirect_uri` exact-registered — if either fails,
an HTML 400 page (never redirect to an unvalidated URI). Then, as error redirects
(`error`, `state`, `iss`): `response_type=code`, `code_challenge` present with
`code_challenge_method=S256`, `resource` (if given) equals our resource
(`invalid_target`), kill switch off (`access_denied`). Scope: unknown scopes are not
granted; the grant is always exactly `memory`, returned in the token response
(RFC 6749 §3.3). *Deviation from Codex (who suggested rejecting unknown scopes): rejecting
risks breaking Muse if it sends an extra scope; narrowing is spec-compliant.*

`state` ≤ 1024 chars (else HTML 400). Success: a pending request `{req_hash, display_code, client_id, redirect_uri, state,
code_challenge, scope: "memory", resource, created}`, all immutable. `req` = 256-bit
random (a capability: never logged, page sends `Referrer-Policy: no-referrer`);
`display_code` = `XXXX-XXXX` from a 32-char unambiguous alphabet (no 0/O/1/I). TTL 10 min, max 10
pending (expired ones are swept on every write), rate limit 20/min. Response: HTML page
with the code and the command, `<meta http-equiv="refresh" content="3;url=/authorize/wait?req=…">`.

### `GET /authorize/wait?req=`

| Pending state | Response |
|---|---|
| malformed `req` (not 43-char base64url) / unknown / expired | HTML 400 "this request expired; start again in Muse" (no disk write; rate limit 120/min) |
| waiting | same page (refresh) |
| denied, or kill switch on | `302 redirect_uri?error=access_denied&state&iss`, request deleted |
| approved | **atomically** (under the state lock): delete the pending request, mint the code, store its hash with the bound transaction, persist, then `302 redirect_uri?code&state&iss`. A later poll finds nothing (400). Code TTL 60 s starts here. If the response is lost, the user starts again. |

### `POST /token`

Form-encoded, ≤ 8 KiB. Client authentication per the registered method (secret compared
against its hash, constant time); mismatch → `401 invalid_client`. A client id that isn't
shaped like ours is refused before any disk access. Rate limit: 60/min **per client**
(a stranger can't spend Muse's budget without its client_id). Kill switch on →
`503 temporarily_unavailable` for every grant type (transient: the grant is suspended, the
client must keep its refresh token). The grant type must be one the client registered
(`unauthorized_client`). Everything below runs under the state lock; changes are persisted
before responding, failures that change nothing are not written (`Cache-Control: no-store`).

- `authorization_code`: code hash found, not expired, same `client_id`, `redirect_uri`
  equal, `resource` (if given) equal, `SHA256(code_verifier)` = challenge. The code is
  deleted on any attempt (single use). An already-redeemed code presented again revokes the
  grant it produced (RFC 6749 §4.1.2; redeemed hashes kept until the code's expiry). Creates a grant
  `{id, client_id, scope, resource, created, last_used, access_hash, access_exp,
  refresh_hash, refresh_exp, used_refresh_hashes}`.
- `refresh_token`: hash matches a grant's current refresh token, same client, not
  expired → rotate both tokens. Hash matches a grant's *used* refresh token → **revoke the
  grant** (all its tokens) and `invalid_grant`. Concurrent refreshes or a lost refresh
  response therefore revoke a legitimate grant; Muse must reconnect (documented).
- Lifetimes: access 1 h; refresh 30 d sliding; grant absolute max 180 d. Used refresh
  hashes are kept for the grant's lifetime (cap 64, oldest dropped).

### `POST /mcp`

Accepts the static token (if configured) or a live OAuth access token (hash lookup,
unexpired, grant's resource = ours). Only token-shaped bearer values are looked up, under a
shared lock, inside the bounded worker pool. While the kill switch is on a valid token is
**not** answered with 401 (that reads as "revoked"): `tools/call` gets the usual audited
tool error, every other method a 403. The state file is re-read per request, so
`disconnect` takes effect on the next request (no cache). Budgets, kill switch, Origin,
audit unchanged; audit adds `client` (`static` or grant id).

### Kill switch (`gateway off`)

Blocks `/mcp` (every method), `/authorize`, code delivery on the wait page, `/token`
(all grants), and `gateway connect`. `off` also deletes all pending requests and
unredeemed codes. Existing grants are **suspended** (unusable while off), not revoked;
`disconnect --all` revokes them. The gateway has no long-lived streams (`GET /mcp` is 405).

### Response hardening

HTML: `X-Frame-Options: DENY`, `Content-Security-Policy: default-src 'none'; style-src 'unsafe-inline'; frame-ancestors 'none'`,
`Cache-Control: no-store`, `Referrer-Policy: no-referrer`, all client-supplied strings
HTML-escaped. JSON endpoints: `Cache-Control: no-store`. Body read with the same absolute
deadline as `/mcp`.

### CLI

| Command | Effect |
|---|---|
| `gateway connect <CODE>` | Prints the request (client name marked *unverified* and terminal-escaped, full redirect URI, resource, permission "read your Muse export (and add to the review inbox if remember is on)", age) and asks `Approve? [y/N]` on stdin. Only `y`/`yes` approves. Kill switch on / unknown / expired → nonzero exit. |
| `gateway clients` | Live grants: id, client name (escaped), created, last used. |
| `gateway disconnect <id>` / `--all` | Revokes grants immediately. `--all` also drops approved-but-unclaimed sign-ins and unredeemed codes. |

## Storage

`<db dir>/gateway-oauth.json` (0600, `O_NOFOLLOW`, temp 0600 + fsync + rename + dir
fsync); every read-modify-write under an exclusive lock on `gateway-oauth.lock` (0600),
shared by server and CLI. Only SHA-256 hashes of `req`, codes, secrets and tokens are
stored. Hashes protect against disk *disclosure*, not against someone who can *modify*
the file (they can already modify the database). Corrupt file → OAuth endpoints and
OAuth-authenticated `/mcp` fail closed; the static token keeps working.

## Threats

| Threat | Mitigation | Status |
|---|---|---|
| Stranger obtains a token | Local interactive `connect` | Closed |
| Relay phishing of the display code | Full details + unverified label + y/N + docs | **Reduced, not closed** |
| Code interception | PKCE S256 mandatory, single use, 60 s, transaction bound, exact redirect | Closed |
| Error redirect to attacker URI | Client + redirect validated before any redirect | Closed |
| Token theft from disk | Hash-only storage | Closed for disclosure |
| Refresh token theft | Rotation + reuse revokes grant; `disconnect` | Reduced |
| Registration / pending flooding | Caps, per-endpoint rate limits, TTL sweep, no eviction of referenced clients | Reduced (DoS of connecting only, never disclosure) |
| Clickjacking / XSS / terminal injection | Frame-deny CSP, no form, HTML escape, terminal escape | Closed |
| Audience confusion | Every grant bound to `<issuer>/mcp`; `resource` checked | Closed |

## Not in scope

Multi-user accounts, consent scopes beyond `memory`, introspection/revocation endpoints
(CLI does it), CIMD, JWT tokens, `openid-configuration`.

## Tests

Unit: PKCE, code/refresh lifecycle, reuse detection, allowlist, field caps, eviction
rules, expiry sweep, HTML/terminal escaping, metadata shape, kill-switch paths.
Acceptance (context-isolated agent): full Muse-shaped flow over HTTP (discover → register
→ authorize → CLI connect with `y` on stdin → wait redirect → token → MCP call → refresh →
replayed refresh revokes → disconnect → 401) plus negatives: wrong verifier, code reuse,
code after 60 s, redirect not allowlisted, unknown client (no redirect), `n` at the prompt,
kill switch during approval, concurrent polls (one code), concurrent refresh, restart
between approval and poll. **Release gate:** a real Muse account connects through a tunnel
(discovery, DCR defaults, webview polling, refresh).
