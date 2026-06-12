# Security Audit — Iteration 28

**Date**: 2026-06-12
**Focus**: Sync layer authentication bypass (untrusted cloud directory)

## Finding (CRITICAL) — Plaintext oplog injection bypasses encryption + HMAC

### Summary
When sync encryption is enabled, an attacker with write access to the sync
directory (the explicit threat model — cloud storage is untrusted) could inject
arbitrary operations into a peer's local database by writing an **unencrypted,
un-HMAC'd** JSONL line.

### Root cause
`read_oplog` (cortex-core/src/sync/oplog.rs) decrypted a line only when
`is_encrypted_line()` returned true; otherwise it fell through to an `else` branch
that parsed the line as **plaintext JSON**, regardless of whether a crypto context
was present. Operation HMAC verification then ran only `if let Some(hmac) = &op.hmac`,
so an injected op that simply omitted the `hmac` field was accepted with **no
authentication at all**.

This bypassed both layers of protection simultaneously:
1. **AES-256-GCM** — the attacker never needs the key; they write plaintext.
2. **Operation HMAC** — skipped entirely when the field is absent.

The device-ID guard in `mod.rs` (`op.hlc.device_id != dir_name`) provides no
protection: the attacker sets `hlc.device_id` to match the directory name.

The injected `SyncPayload` is applied directly via `merge::apply_op`, allowing
forged `MemoryUpsert` / `MemoryDelete` / `PersonUpsert` / `LinkUpsert` / etc.

### Fix
In `read_oplog`, when a crypto context is present (`crypto.is_some()`) and a line
is **not** an encrypted envelope, the line is now rejected (logged + skipped),
never parsed. The writer always emits an `ENC1:`/`ENC2:` envelope when encryption
is enabled, so a plaintext line in encryption mode is by definition corruption or
injection. Encrypted lines remain authenticated by AES-GCM (and the HMAC where
present).

### Tests (TDD)
- `test_plaintext_injection_rejected_when_encryption_enabled` — forged plaintext
  op is not replayed (failed before fix, passes after).
- `test_encrypted_lines_still_read_in_encryption_mode` — legitimate encrypted
  round-trip still works.

Full `cargo test -p cortex-core` suite green; build and lib clippy clean.
