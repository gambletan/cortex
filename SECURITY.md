# Security

## Threat Model

### What Cortex Protects Against

| Threat | Protection |
|--------|-----------|
| **Cloud storage breach** | Oplog files are AES-256-GCM encrypted (opt-in). An attacker with access to your iCloud/Google Drive/OneDrive/Dropbox cannot read synced memories. |
| **Memory forensics** | Sensitive data (text, facts, preferences) is zeroized on drop via the `zeroize` crate. Encryption keys are securely cleared from memory. |
| **Accidental sync of private data** | Private memories (the default) are excluded from device sync. Explicit Muse sharing can create a separate export copy of a Private original. Only memories explicitly marked Shared or Public are written to the sync oplog, and demoting one back to Private retracts it from other devices. Peers cannot push Private memories. |
| **Man-in-the-middle** | The local core has no network transport; device sync uses the filesystem and cloud providers handle transport. The standard MCP server can fetch embedding models and, with explicit Muse opt-in, sends signed HTTPS requests to Cortex Cloud. Muse uses an opt-in internet-facing gateway. TLS protects transport, not data from the cloud operator. |
| **Telemetry/tracking** | Zero telemetry. Zero analytics. Zero phone-home. Verify: `grep -r "reqwest\|hyper\|TcpStream\|UdpSocket" cortex-core/src/` returns nothing. |

### What Cortex Does NOT Protect Against

| Threat | Why |
|--------|-----|
| **Root access on your device** | If an attacker has root, they can read the SQLite database directly. Use full-disk encryption (FileVault, BitLocker, LUKS). |
| **Weak passphrase** | Encryption is only as strong as your passphrase. Use a strong, unique passphrase. |
| **Side-channel attacks** | No protection against timing, cache, or power analysis on cryptographic operations. |
| **Compromised build** | If you run a tampered binary, all bets are off. Build from source or verify release checksums. |

## Data Storage Locations

| Data | Location | Encrypted? |
|------|----------|-----------|
| All memories (SQLite) | User-specified path (default: local) | No (use OS full-disk encryption) |
| Sync oplog files | User's cloud storage folder | Yes, if passphrase configured (AES-256-GCM) |
| Sync manifest | `cortex-sync/manifest.json` in sync folder | No (contains only salt + schema version, no sensitive data) |
| Device metadata | `cortex-sync/devices/{id}/device.json` | No (device name + OS only) |

## Encryption Details

- **Algorithm**: AES-256-GCM (authenticated encryption with associated data)
- **Key derivation**: Argon2id (memory-hard, resistant to GPU/ASIC attacks)
  - Parameters: time_cost=3, mem_cost=64MB, parallelism=1
  - 16-byte random salt (stored in manifest.json)
- **Per-line nonce**: 12-byte random nonce per oplog line (never reused)
- **Format**: `ENC1:<base64(nonce[12] || ciphertext || tag[16])>`; since v2.2, versioned `ENC2` envelopes with per-version keys (key rotation, forward secrecy against AES-key exfiltration)
- **Integrity**: HMAC on the manifest (mandatory) and on every operation; encrypted ops without an HMAC, plaintext oplog lines, and plaintext snapshots are all rejected while encryption is on
- **Key storage**: Derived at runtime from passphrase, never persisted. Zeroized on drop.

## Privacy Levels

| Level | Sync Behavior | Default? |
|-------|--------------|----------|
| **Private** | Excluded from device sync; explicit Muse export is a separate copy | Yes |
| **Shared** | Syncs to devices in specified scope | No |
| **Public** | Syncs to all connected devices | No |

## Hosted Muse sharing (opt-in)

The default service is `https://cortex.alvinsclub.ai`. `muse_connect` and `muse_share`
preview the exact export before confirmation; the full archive is never uploaded by
these tools. The tenant export uses SQLCipher at rest; inbox text is AES-256-GCM sealed.
The running server holds the keys and can read both. This is not end-to-end encryption
between your device and Muse.

Device management uses signed Ed25519 requests, timestamp and persisted nonce checks.
Muse uses resource-bound OAuth with PKCE and rotating refresh tokens. Enrollment links
are single-use, expire after 30 minutes, and rotate on reconnect; web consent requires
CSRF and same-origin checks. Reconnecting revokes previous grants.

Export replacement, tenant deletion and OAuth replay/reuse revocation share a disclosure
lock with recall and inbox writes. Successful revocation fences later disclosures;
responses already constructed cannot be withdrawn. Stale export snapshots are rejected.
`muse_unshare` removes selected exports; `muse_disconnect` deletes the cloud tenant.
A failed cloud push must be retried: the previous export may remain accessible until
reconciliation succeeds. Self-hosted `gateway off` does not disable a cloud connection.

Metadata-only audit counts concern retained records for currently shared items. Audit
read failures return explicit errors, not an empty history. Deleting a tenant also
removes its inbox, audit history and OAuth state; there are no tenant-data backups.
Meta may retain disclosed copies, which Cortex cannot erase.

See the [user guide](docs/muse.md), [cloud threat model](docs/design/muse-cloud.md), and
[verified 2026-10-07 release](deploy/cortex-cloud/RELEASE_2026-10-07.md).

## Self-hosted Meta Muse gateway (opt-in)

`cortex-mcp-server gateway serve` lets Meta Muse, which runs in Meta's cloud, read a slice of
memory that the user explicitly chose. This self-hosted mode is reached through a tunnel the user runs. Full design and threat model:
[docs/design/muse-gateway.md](docs/design/muse-gateway.md).

| Control | Detail |
|---|---|
| Scope | Read-only recall, plus optional append-only `remember` proposals (`--enable-remember`). Only memories added with `gateway allow` (namespace `muse-export`, which is reserved: core rejects ordinary ingest and sync-peer writes to it). |
| Freshness | Each request reads the export straight from SQLite, with no shared index or cache, so `revoke` takes effect on the next request. |
| Auth | OAuth 2.1 (v2.5, `--oauth`): DCR restricted to allowlisted redirect URIs, PKCE S256 mandatory, **every sign-in approved on the user's machine** (`gateway connect <code>`, interactive; nothing typed in the browser grants access), single-use 60 s codes, 1 h access / 30 d rotating refresh tokens with reuse detection, tokens bound to the `/mcp` resource, only SHA-256 hashes stored. And/or a static bearer token (≥ 32 chars, random). Constant-time compare, checked before any body byte is read; `Origin` allowlist; 64 KiB body with an absolute 10 s read deadline; 16 concurrent requests; one server per database. |
| Budgets | Per UTC day: requests and **distinct** memories disclosed. Every call is charged first. Over the disclosure budget, unseen matches are withheld silently, so no search oracle. |
| Kill switch | `gateway off` takes effect immediately and fails closed on I/O errors. It refuses every MCP method, sign-ins and token requests, and cancels sign-ins in progress; signed-in clients are suspended until `on` (`disconnect` revokes them). |
| Audit | Local JSONL with metadata only (ids, counts, outcome), never the query or text. 0600, rotated. |

**Not protected:** anything returned to Muse is in Meta's cloud and cannot be recalled. Use a
tunnel that terminates TLS on your machine (e.g. Tailscale Funnel). Edge-terminating tunnels
can read traffic. Whoever controls the gateway host can read the database. OAuth relay
phishing is reduced, not closed: an attacker can start a sign-in and try to talk the user
into approving the attacker's code; `connect` shows the full redirect and marks the client
name as unverified ([docs/design/muse-oauth.md](docs/design/muse-oauth.md)).

## Zero Telemetry Verification

The default `cortex-core` dependency tree has no network or telemetry dependencies.
The standard MCP build includes model fetching and opt-in Muse cloud transport; absence
of telemetry does not mean absence of network traffic. The offline build omits these
features. Verify the scoped dependency and source checks with:

```bash
bash scripts/check-no-network-egress.sh
```

These checks enforce the documented local-core/offline-build boundaries. They do not
claim that an explicitly enabled Muse connection keeps approved excerpts on-device.

## Responsible Disclosure

If you discover a security vulnerability, please report it via GitHub Issues with the `security` label, or contact the maintainers directly.
