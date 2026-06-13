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
- **Tests / CI: GREEN as of 2026-06-11.** The **full** workspace suite passes
  (`cargo test --workspace --exclude cortex-python --exclude cortex-wasm`). NOTE: CI had been
  silently **red for ~4 iterations** — `test_sync.rs` failed to compile (missing `hmac` field),
  which aborts the whole `cargo test`, and ~11 more integration tests had rotted. `cargo test
  --lib` masks this; always run the full workspace suite. See memory `cortex-ci-testing`.
- **Security:** iterations 11–14 shipped — manifest HMAC, OPLOG HMAC + tampering detection,
  timing-attack hardening (privacy padding + constant-time compares), private-delete privacy leak.
- **Privacy architecture fix (2026-06-11):** the iter-10 `privacy != Private` filter in
  fact/preference storage queries was relocated to the context-export boundary
  (`generate_context` + `ContextConfig::for_remote_llm`). Storage now returns the owner's own
  data faithfully; Private is excluded only when context is bound for a remote LLM.
- **Surfaces:** core (Rust), HTTP API, MCP server, Python + WASM bindings, Obsidian + OpenClaw
  plugins, npm package. Multi-surface and fairly mature.
- **Known environment quirks (not bugs):** `cargo build --workspace` fails to link the
  `cortex-python` pyo3 dylib standalone on macOS (needs interpreter symbols); zstd link-time
  macOS-version warnings are cosmetic.

### Shipped this cycle
- ✅ Frecency ranking in `retrieval.rs` (P2) — access frequency + recency boost, tested.
- ✅ CI restored to green; sync + fact/preference/contradiction integration tests revived.
- ✅ Privacy enforcement relocated to the context boundary (above).
- ✅ **Iteration 15 — Key Rotation & forward secrecy (P1 flagship).** Versioned `ENC2`
  envelope (ENC1 = v0, backward compatible), passphrase-derived per-version keys,
  `SyncEngine::rotate_key()`. See docs/design/key-rotation.md. Follow-ups: passphrase
  change (vs key-only rotation), optional `compact_to_current_version()`, wire rotation
  into the HTTP/MCP surface.
- ✅ **Iteration 17 — Per-memory privacy opt-in + persistent sync (2026-06-13).** Dogfooding
  iCloud sync surfaced two gaps: (a) everything defaults to `Private` and never syncs, but no
  surface could mark a memory `Shared` — sync synced nothing; (b) sync config lived only in
  process memory — any restart silently disabled sync. Shipped: `ingest_with_options` +
  `set_memory_privacy` in core (demote-to-Private records a sync **delete**, retracting the
  memory from other devices), MCP `privacy`/`scope` args + new `memory_set_privacy` tool
  (write group, 30 tools now), CLI `ingest --privacy`, `sync_settings` table (passphrase
  NEVER stored) + macOS-keychain/env passphrase resolution + `Cortex::resume_sync()` on
  server start and sync-relevant CLI paths. Follow-ups: batch per-item privacy, HTTP surface,
  `sync disable` command.
- ✅ **Iteration 16 — Bounded query budget + MCP deny-by-default capability grants (P2).**
  Every retrieval is bounded by `QueryBudget` (candidate cap + wall-clock cap, graceful
  degradation — never an error, which would itself be a store-size oracle). The MCP
  surface gates `tools/list` + `tools/call` behind a capability policy
  (`capabilities.json`: `read`/`write`/`sync`/`plugins`/`all` groups or exact names);
  no policy = legacy allow-all with warning, malformed/missing-explicit policy fails
  CLOSED. See docs/design/query-budget-and-mcp-capabilities.md. Follow-ups below.

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

- ~~**Frecency ranking in `retrieval.rs`**~~ ✅ shipped.
- ~~**Bounded-time query budget**~~ ✅ shipped (Iteration 16).
- ~~**Deny-by-default capability grants on the MCP surface**~~ ✅ shipped (Iteration 16) —
  `tools/list` filtering also delivers the progressive-disclosure win below.
- **Progressive MCP tool disclosure** (source: agent-skills trend). Partially covered by
  capability-filtered `tools/list`; contextual tool *groups* remain open.
- **Unify cortex-http under the capability policy** (from Iteration 16 review). The HTTP
  surface exposes ingest/search/import with no policy gating — asymmetric with MCP now.
- **Capability denial as JSON-RPC error code** (from Iteration 16 review, LOW). Denials
  currently use the MCP `isError` content envelope; a distinct error code would let
  clients tell "tool failed" from "tool not permitted".

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
