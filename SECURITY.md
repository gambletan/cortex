# Security

## Threat Model

### What Cortex Protects Against

| Threat | Protection |
|--------|-----------|
| **Cloud storage breach** | Oplog files are AES-256-GCM encrypted (opt-in). An attacker with access to your iCloud/Google Drive/OneDrive/Dropbox cannot read synced memories. |
| **Memory forensics** | Sensitive data (text, facts, preferences) is zeroized on drop via the `zeroize` crate. Encryption keys are securely cleared from memory. |
| **Accidental sync of private data** | Private memories (the default) never leave the local SQLite database. Only memories explicitly marked Shared or Public are written to the sync oplog, and demoting one back to Private retracts it from other devices. Peers cannot push Private memories. |
| **Man-in-the-middle** | Cortex makes no outbound network calls (one-time embedding-model download aside, see README). Sync happens through the local filesystem; cloud providers handle transport. The only inbound listeners are local (`cortex-http` on loopback by default; the opt-in Muse gateway, see below). |
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
| **Private** | Never leaves local device | Yes |
| **Shared** | Syncs to devices in specified scope | No |
| **Public** | Syncs to all connected devices | No |

## Meta Muse Gateway (opt-in, v2.3+)

`cortex-mcp-server gateway serve` lets Meta Muse, which runs in Meta's cloud, read a slice of
memory that the user explicitly chose. It is the only Cortex component meant to be reached
from the internet, through a tunnel the user runs. Full design and threat model:
[docs/design/muse-gateway.md](docs/design/muse-gateway.md).

| Control | Detail |
|---|---|
| Scope | One read-only MCP tool. Only memories added with `gateway allow` (namespace `muse-export`, which is reserved: core rejects ordinary ingest and sync-peer writes to it). |
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

Cortex makes zero network calls. You can verify this yourself:

```bash
# Search for any networking code in the core library
grep -r "reqwest\|hyper\|TcpStream\|UdpSocket\|connect\|dns" cortex-core/src/
# Result: no matches

# The HTTP server (cortex-http) and MCP server are local-only listeners
# They do not make outbound connections. The opt-in Muse gateway accepts inbound
# requests (through your tunnel) but also never connects out.
# CI enforces this: scripts/check-no-network-egress.sh
```

## Responsible Disclosure

If you discover a security vulnerability, please report it via GitHub Issues with the `security` label, or contact the maintainers directly.
