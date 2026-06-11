# Design — Iteration 15: Key Rotation & Forward Secrecy

**Status:** DRAFT for review (no code yet)
**Owner:** CTO
**Date:** 2026-06-11
**Tracking:** docs/ROADMAP.md → Priority 1

## 1. Goal

Let a user rotate the sync encryption key (e.g. after a suspected passphrase
compromise, or on a routine schedule) **without re-encrypting the entire history in
place**, and give newly-written data **forward secrecy** relative to old keys. Today
`EncryptionManifest.key_version` exists and is read by `derive_key`, but it is a no-op:
there is exactly one key, derived once from the passphrase, used for every oplog line
and snapshot forever. A leaked passphrase compromises all past and future sync data with
no recovery path short of tearing down the sync folder.

## 2. Non-goals

- Re-keying data **at rest in SQLite** (separate concern; this is sync-oplog/snapshot only).
- Changing the AEAD (stay on AES-256-GCM) or the KDF family (stay on Argon2id).
- Per-record key wrapping / envelope encryption (heavier; revisit if multi-recipient sync lands).
- Automatic rotation scheduling (this design enables a rotation **operation**; policy is later).

## 3. Current state (as built)

- Line format: `ENC1:<base64(nonce[12] || ciphertext || tag[16])>` (see `sync/crypto.rs`).
- `derive_key(passphrase, manifest)` → Argon2id over `manifest.salt` → 32-byte AES key +
  a domain-separated HMAC key. `key_version` is read then ignored.
- `EncryptionManifest { algorithm, kdf, salt, kdf_params, key_version, hmac_salt, hmac }`,
  integrity-protected by an HMAC verified at `SyncEngine::new`.
- One `CryptoContext` (one AES key) is held for the engine's lifetime.

## 4. Design

### 4.1 Versioned line format `ENC2`

Introduce a forward-compatible line format that records which key version encrypted it:

```
ENC2:<version:u16 LE, 2 bytes><nonce[12]><ciphertext><tag[16]>   (all base64-encoded together)
```

- `ENC1:` lines remain readable forever and are treated as **version 0**. No migration of
  existing data is required; old lines decrypt with the v0 key.
- New writes use `ENC2:` stamped with the manifest's **current** `key_version`.
- `is_encrypted_line` accepts both prefixes; `decrypt_line` dispatches on prefix → for
  `ENC2` it parses the version and selects the matching key.

Rationale for an in-payload version (vs. a side channel): the oplog reader already
processes lines independently; embedding the version keeps each line self-describing and
avoids a second lookup, matching the existing self-contained `ENC1` design.

### 4.2 Per-version key derivation

Keep v0 exactly as today (backward compatible). For version `n > 0`, derive from the same
passphrase but a **version-specific salt**, layering version-specific PBKDF2 rounds on top
of the Argon2id output so each version's key is independent and a leaked v(k) key does not
yield v(k+1):

```
base   = Argon2id(passphrase, manifest.salt)          // as today (v0 AES key)
key(0) = base
key(n) = PBKDF2-HMAC-SHA256(base, salt = manifest.salt || LE(n), rounds = ROUNDS)   for n > 0
```

- Deterministic from `(passphrase, manifest.salt, n)`, so any device with the passphrase
  derives every version's key on demand — no key material is ever stored or synced.
- `ROUNDS` fixed constant (e.g. 100k), recorded conceptually by the version scheme (not
  per-line) so it can be bumped via a new KDF id if ever needed.
- HMAC key derivation gets the same per-version treatment (the `"HMAC"` domain separator
  is applied before the PBKDF2 layer).

This is exactly the path the existing `derive_key` comment already promises, and is why the
`pbkdf2` dependency stays in `Cargo.toml`.

### 4.3 Manifest changes

- `key_version` becomes the **current/active** version (the one new writes use). Already
  `Option<u32>`, default `Some(0)`; no schema change, only semantics + the manifest HMAC
  recomputed on rotation.
- The manifest's own integrity HMAC continues to use the **v0-derived** HMAC key so the
  manifest stays readable/verifiable regardless of how many rotations happened (the manifest
  is not secret; it must be openable by anyone with the passphrase).

### 4.4 `CryptoContext` → keyring

Replace the single-key `CryptoContext` with a small **keyring** that lazily derives and
caches `key(n)` per version (bounded; versions are few). Encryption always uses the active
version; decryption uses the version parsed from the line (`ENC1` ⇒ 0). Keys are zeroized on
drop exactly as today (`ZeroizeOnDrop`).

### 4.5 Rotation operation

`SyncEngine::rotate_key()`:
1. `new_version = key_version + 1`.
2. Update the manifest's `key_version`, recompute + store the manifest HMAC (v0 HMAC key).
3. From now on, new oplog lines and snapshots are written as `ENC2` at `new_version`.
4. Old `ENC1`/`ENC2@<old>` lines remain readable via the keyring.

Forward secrecy property: data written after rotation is encrypted under `key(new)`, which is
**not derivable from** `key(old)` alone (PBKDF2 one-way + version-specific salt). An attacker
who captured only `key(old)` cannot read post-rotation writes. (Full forward secrecy against a
passphrase compromise also requires a passphrase change; the version scheme additionally
supports a future "rotate salt + passphrase" variant.)

### 4.6 Compaction / optional re-encryption

Out of scope for v1, noted for completeness: a later `compact_to_current_version()` could
re-encrypt old lines forward and drop old versions, trading the keep-all-versions simplicity
for the ability to fully retire a compromised key's readability. v1 keeps all versions
readable (availability over aggressive retirement).

## 5. Backward compatibility

- Existing sync folders: all lines are `ENC1` = v0; everything keeps working untouched.
- A v0-only reader (older Cortex) encountering an `ENC2` line: it must **skip-and-warn**
  (unknown format) rather than miscically decrypt — verify the current `read_oplog` unknown-line
  handling does this, and add a test. This bounds the blast radius if devices are on mixed versions.
- Manifest with `key_version > 0` opened by an old binary: old `derive_key` ignores
  `key_version` and derives v0 — it can still read v0/`ENC1` lines but not `ENC2`. Acceptable;
  document the minimum version for rotation.

## 6. Test plan (write first)

1. `ENC1` line still round-trips after the `ENC2` path is added (no regression).
2. `ENC2@0` round-trips and is byte-compatible in semantics with `ENC1@0`.
3. Rotate to v1: new writes are `ENC2@1`; a reader with the passphrase reads both old `ENC1`
   and new `ENC2@1` lines in one pull.
4. `key(1) != key(0)` and `key(1)` is **not** derivable from `key(0)` (negative test: a context
   holding only `key(0)` fails to decrypt an `ENC2@1` line).
5. Mixed-version oplog file (v0 then v1 lines) reads fully and in order.
6. Tampering detection (OPLOG HMAC, consecutive-failure alert) still fires per version.
7. Unknown future version (`ENC2@65535`) → skip-and-warn, no panic, no silent misdecrypt.
8. Manifest HMAC still verifies after rotation; wrong passphrase still rejected at init.

## 7. Risks & mitigations

- **Format mistake = unreadable data.** Mitigation: `ENC1` untouched; `ENC2` is additive;
  every path covered by round-trip tests before any write path ships.
- **Performance:** PBKDF2 layer adds cost only at key-derivation time (once per version per
  process), cached in the keyring — not per line. Negligible vs. per-line AES-GCM.
- **Cross-version device skew:** bounded by skip-and-warn + a documented minimum version.

## 8. Decision needed before implementation

- `ROUNDS` value for the PBKDF2 layer (default proposal: 100_000).
- Whether v1 ships rotation **only** (keep all versions) or also the optional
  `compact_to_current_version()` (recommended: rotation only in v1).
- Confirm `ENC2` version width (`u16` proposed — 65k rotations is plenty; `u32` if paranoid).
