# Security

## Threat Model

### What Cortex Protects Against

| Threat | Protection |
|--------|-----------|
| **Cloud storage breach** | Oplog files are AES-256-GCM encrypted (opt-in). An attacker with access to your iCloud/Google Drive/OneDrive/Dropbox cannot read synced memories. |
| **Memory forensics** | Sensitive data (text, facts, preferences) is zeroized on drop via the `zeroize` crate. Encryption keys are securely cleared from memory. |
| **Accidental sync of private data** | Private memories (the default) never leave the local SQLite database. Only memories explicitly marked as Shared or Public are written to the sync oplog. |
| **Man-in-the-middle** | No network calls. Sync happens through local filesystem only. Cloud providers handle transport encryption. |
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
- **Format**: `ENC1:<base64(nonce[12] || ciphertext || tag[16])>`
- **Key storage**: Derived at runtime from passphrase, never persisted. Zeroized on drop.

## Privacy Levels

| Level | Sync Behavior | Default? |
|-------|--------------|----------|
| **Private** | Never leaves local device | Yes |
| **Shared** | Syncs to devices in specified scope | No |
| **Public** | Syncs to all connected devices | No |

## Zero Telemetry Verification

Cortex makes zero network calls. You can verify this yourself:

```bash
# Search for any networking code in the core library
grep -r "reqwest\|hyper\|TcpStream\|UdpSocket\|connect\|dns" cortex-core/src/
# Result: no matches

# The HTTP server (cortex-http) and MCP server are local-only listeners
# They do not make outbound connections
```

## Responsible Disclosure

If you discover a security vulnerability, please report it via GitHub Issues with the `security` label, or contact the maintainers directly.
