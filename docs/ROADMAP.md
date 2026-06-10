# Cortex Roadmap

**Owner:** CTO (engineering direction)
**Last updated:** 2026-06-10
**Mission:** the most privacy-advanced memory engine for personal AI agents — 100% local,
zero telemetry, cryptographically hardened, defense-in-depth.

This is the prioritized forward plan. The per-iteration security audits
(`SECURITY_AUDIT_ITER_*.md`) are the historical record; this is where we're going next.

---

## Current state (verified 2026-06-10)

- **Build:** library code is `clippy -D warnings` clean across cortex-core / cortex-http /
  cortex-mcp-server. (Integration-test binaries still have residual lints — see Health track.)
- **Tests:** 132 unit tests green (`cargo test --lib --workspace`).
- **Security:** iterations 11–14 shipped — manifest HMAC, OPLOG HMAC + tampering detection,
  timing-attack hardening (privacy padding + constant-time compares), private-delete privacy leak.
- **Surfaces:** core (Rust), HTTP API, MCP server, Python + WASM bindings, Obsidian + OpenClaw
  plugins, npm package. Multi-surface and fairly mature.
- **Known environment quirks (not bugs):** `cargo build --workspace` fails to link the
  `cortex-python` pyo3 dylib standalone on macOS (needs interpreter symbols); zstd link-time
  macOS-version warnings are cosmetic. Use `cargo test --lib` as the canonical gate.

---

## Priority 1 — Iteration 15: Key Rotation / Forward Secrecy  🔐 FLAGSHIP

**Why now:** the single deferred HIGH issue. The `key_version` field already exists in
`EncryptionManifest` and is read in `sync/crypto.rs::derive_key`, but it is a no-op today — there
is no way to rotate a compromised passphrase/key without re-encrypting everything, and no forward
secrecy. This is the highest-value, most on-mission security work left.

**Scope:**
1. Versioned encrypted-payload format: record `key_version` with each encrypted oplog line /
   snapshot so decrypt can select the correct derivation.
2. `derive_key` selects derivation by version: v0 = current Argon2id (backward compatible);
   v>0 = additional version-salted PBKDF2 rounds on top of Argon2id (re-add the `pbkdf2` dep use).
3. Rotation operation: derive a new version, re-wrap/re-encrypt forward, advance manifest version.
4. Multi-version decrypt: keep prior versions readable until rotation completes.

**Risk:** changes the encryption-at-rest format — must be strictly backward compatible and
test-covered (round-trip v0↔v1, mixed-version oplogs, tamper detection still holds). **Design
review before coding.** Land behind tests, never break existing manifests.

## Priority 2 — Product-quality wins from trend-watch (parallelizable, low risk)

These came out of the daily trend-watch scan (`docs/trend-watch/`). Small, local, on-mission.

- **Frecency ranking in `retrieval.rs`** (source: `fff`). Blend recall-frequency + recency into
  ranking alongside vector score; complements existing `memory_decay`. Cleanest first PR.
- **Bounded-time query budget** (source: `pydantic/monty`). Cap per-query work/time — both a
  privacy win (mitigates residual search-timing side-channels) and a DoS guard.
- **Deny-by-default capability grants on the MCP surface** (source: `monty`). A namespace/agent
  gets zero read/write until explicitly granted — cleaner than per-query filtering.
- **Progressive MCP tool disclosure** (source: agent-skills trend). Surface tool groups
  contextually; narrows attack surface and token cost.

## Priority 3 — Security backlog (medium)

- Embedding vector auto-zeroization (memory safety).
- Snapshot HLC versioning.
- Device ID path validation.
- Graph-edge re-ranking in retrieval (source: `hivemind`) — traverse relationship edges as a
  recall signal, not only cosine similarity.

## Priority 4 — Engineering health (continuous)

- **CI gate:** `cargo clippy --all-targets -D warnings` + `cargo test --lib`. Fix residual
  integration-test lints (`tests/test_gdrive_real.rs` unused import, `tests/test_cache_and_perf.rs`
  match-single-pattern) to turn the gate on.
- **Split oversized files** (800-LoC guideline): `storage/sqlite.rs` (1861), `lib.rs` (1311),
  `inference.rs` (1279). Extract by domain, behavior-preserving, test-backed.
- **`cargo audit` / `cargo deny`** in CI for dependency CVEs.

## Priority 5 — Long-horizon research (low)

- Quantum-resistant encryption (hybrid KEM).
- Zero-knowledge proofs for advanced privacy.
- Differential privacy for aggregations.

---

## Anti-goals (explicit rejects)

- **No mmap of plaintext memory content** (even though `fff` does it for speed) — it would
  undermine encryption-at-rest + zeroization. Adopt arena/mimalloc-style perf instead.
- **No cloud-default / shared-by-default storage** (the `hivemind` posture) — local-first and
  single-user-private is the whole differentiator.
- **No telemetry, ever.**

## Recommended sequence

1. **Now (this week):** frecency ranking PR (P2) — small, proves the retrieval-quality track,
   no format risk. In parallel, turn on the clippy CI gate (P4).
2. **Next:** Iteration 15 Key Rotation (P1) — design doc first, then implement behind tests.
3. **Then:** bounded-time query budget + deny-by-default MCP caps (P2, security-flavored).

Trend-watch runs daily and feeds P2/P3 candidates into this list.
