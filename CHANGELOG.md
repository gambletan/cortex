# Changelog

## v2.4.0 — `remember`: Muse saves to your memory, you approve

### New
- `gateway serve --enable-remember` adds a `remember` tool. Muse can only **append** to a quarantined review inbox, and gets an acknowledgement with no id and no echo. Inbox items stay out of search, embeddings and sync until you `gateway approve` them; approval creates a Private memory plus an export copy. Use `gateway inbox` / `approve` / `reject [--all]`. Limits: 1000 chars per item, 20 per day, 200 pending. Inbox output escapes terminal control and bidi characters

### Security & privacy (full review of v2.2.1–v2.3.0)
- **The sync passphrase never passes through the model.** `sync_enable` doesn't return it, and it no longer accepts one as an argument (use `CORTEX_SYNC_PASSPHRASE` or the terminal CLI). A generated passphrase goes to the OS keychain; without a keychain the tool refuses
- Sync tool results **and errors** are scrubbed: account emails (incl. quoted local parts), home directory, OneDrive/Dropbox/Box org folder names, IDN and trailing-dot emails. Device names are no longer echoed
- Encrypted snapshots carry an **HMAC** bound to the file name, and the HMAC-protected manifest **pins the latest snapshot**, so restore loads exactly that file or fails closed. A replayed older snapshot, or corrupting a newer one to force an older restore, no longer works. The manifest carries a **generation** counter: a device that has synced before rejects a rolled-back manifest. Snapshots under rotated-out key versions are rejected, and restore caps file and decompressed size
- Snapshots get unique, immutable names (`snapshot-YYYY-MM-DD-<8 hex>`). The new file is written completely before the pointer moves, the pinned file is never overwritten, and older unpinned snapshots are pruned (newest 3 kept). A running device adopts another device's key rotation on its next pull or snapshot, so it never pins or writes under a retired key
- ⚠️ **Mixed versions:** once a v2.4 device pins a snapshot, older Cortex versions refuse that sync folder's manifest. Upgrade all devices together. After `rotate_key`, create a new snapshot; the pinned one uses the retired key
- Residual (documented): a brand-new device has no anchor against a fully rolled-back manifest + snapshot pair. Library API changes: `SyncEngine::rotate_key(&storage)` persists the generation immediately; `create_snapshot` / `restore_from_snapshot` take `&SqliteStorage` (and `create_snapshot` takes `&mut self`)
- Joining an encrypted sync folder without its passphrase is refused (it used to write plaintext ops). An encrypted oplog line with no key available no longer advances the cursor
- Muse gateway: state files opened without following symlinks, with exclusive random temp files; the kill switch fails closed; terminal output escapes invisible and bidi characters on every display path; a full inbox gives the same reply and the same daily-cap charge as success (no activity oracle); no shared-cache staleness; stopword-only matches don't disclose; CJK keyword matching; the near-dedup, privacy-change and import paths can't touch the export; body-read timeout and bounded worker pool; over-budget refusals audited once per day
- `server.json`: named volume so the Docker MCP server keeps memories across sessions. `release-docker` works from `workflow_dispatch`. MSRV declared: Rust 1.89

## v2.3.0 — Meta Muse gateway: share a slice of memory, keep the rest on your device

### New: `cortex-mcp-server gateway` ([guide](docs/muse.md))
- Remote MCP endpoint for **Meta Muse** custom connectors, exposing one read-only tool, `recall_memory`
- **Explicit export only.** Muse can read only what you add with `gateway allow`. The `muse-export` namespace is reserved in core, so ordinary ingest and sync peers cannot write to it
- **Budgets.** Per-day caps on requests and on *distinct* memories disclosed. Every call is charged up front, and once the disclosure budget is spent, new matches are withheld silently
- **Kill switch.** `gateway off` refuses every request immediately; `revoke` takes effect on the next request even while the server is running (no shared cache)
- **Preview + audit.** `gateway preview` shows exactly what Muse would get. The audit log records only metadata, never your query or the shared text
- Bearer auth, Origin check, body, time and concurrency limits, email redaction, single instance per DB
- `-lite` binary (`--no-default-features`) excludes the gateway and keeps zero HTTP/network crates
- Preview status: Muse OAuth-only connectors are not supported yet

## v2.2.1 — Sync hardening, MCP Registry fix

### Security & privacy
- Encrypted sync rejects **plaintext snapshot downgrade** (library-API hardening: `restore_from_snapshot` had no production caller yet) and **un-HMAC'd encrypted oplog ops**
- ⚠️ **Migration:** encrypted ops written before v2.2.0 have no per-op HMAC and are now rejected (logged, skipped). If a device still depends on such old ops, re-export from a device that has the data (`export` → `import`) or re-create the sync group
- `sync_status` / `sync_providers` / `sync_enable` no longer leak the cloud account email: the whole `GoogleDrive-<account>` path segment is redacted, plus any bare email

### Packaging
- `server.json` now validates against the MCP Registry 2025-12-11 schema (#15)
- Docker image builds again (builder `rust:1.94`; 2.2.0 image was never published)
- Clippy clean on Linux + current stable

## v2.2.0 — Security hardening, privacy opt-in, retrieval quality

### Security & crypto
- Versioned **key rotation** with forward secrecy (`ENC2` envelopes, per-version passphrase-derived keys)
- **HMAC integrity** on the sync manifest and on every operation; plaintext lines in an encrypted oplog are rejected (injection defense); a manifest without integrity refuses to load (no key-rollback)
- Encrypted **snapshots** (not just the oplog); corrupt/tampered memory & people rows fail gracefully instead of panicking
- Timing-attack hardening on retrieval (bounded work, constant-time compares)

### Privacy
- **Per-memory privacy opt-in**: Private by default; mark a memory `shared` to sync it; demote it and it's **retracted from other devices** (local copy kept)
- **Persistent sync**: settings survive restarts; passphrase in the OS keychain (never on disk) or `CORTEX_SYNC_PASSPHRASE`; server resumes sync + background pull automatically
- **Deny-by-default MCP capability policy** (`capabilities.json`); ungranted tools are invisible and uncallable; malformed policy fails closed
- **Honest offline mode**: `CORTEX_NO_EMBEDDINGS=1` (or `--no-default-features`) for a zero-network build; one-time notice before the embedding model is ever fetched; CI proves the no-default-features binary is network-free

### Retrieval quality
- **Paraphrase recall 40% → ~90% at 5K memories** by widening the HNSW search beam (`ef_search`); query latency unchanged (`docs/scale-test-2026-06-13.md`, `bench/recall_scale.py`)
- **Bounded query budget** (candidate + wall-clock caps) — DoS guard + timing-channel bound, graceful degradation
- Frecency ranking; employment-change contradiction detection from natural language
- **No silent recall failures**: dimension-mismatched embeddings rejected loudly; `memory_stats` exposes embedding/recall health; `memory_context` `min_confidence` floor keeps low-confidence/superseded facts out of the LLM context
- Opt-in semantic near-duplicate dedup (reinforce-not-lose)

### Tooling & docs
- 30 MCP tools (added `memory_set_privacy`); `RUST_LOG` is now honored (was overridden)
- WASM build; new guides: memory tiers, memory-backends comparison
- One-command device setup (`scripts/setup-device-sync.sh`) incl. Claude Code auto-recall hook

## v2.0.0 — Background Sync, Web Dashboard, Homebrew

### Background Sync
- Filesystem watcher (`notify` crate) triggers instant pull on remote oplog changes
- Polling thread as fallback on configurable interval (default 30s)
- `Cortex::start_background_sync()` / `stop_background_sync()` API
- `Weak<Cortex>` lifecycle — no Arc leak, safe Drop with thread-name self-join guard
- Initial pull on start (no 30s wait for pre-existing data)

### Web Dashboard
- Dark-theme single-page dashboard at `http://localhost:3315/`
- Search, memory list, stats panel, beliefs, people, sync status
- Auto-refresh every 30s, embedded via `include_str!` (no external files)
- New endpoints: `GET /v1/memories/recent`, `GET /v1/stats`, `GET /v1/people`

### Homebrew
- `brew tap gambletan/tap && brew install cortex-mcp-server`

### Integration Docs
- CrewAI, AutoGen, LangGraph, DeerFlow, OpenClaw examples in `docs/integrations.md`

### Codex Review Fixes (12 rounds)
- Oplog: always skip invalid JSON lines (writer guarantees flush)
- Watcher: `Create` + `Modify(Data)` + `Access(Close(Write))` events
- Watcher: device ID filter checks `devices/{id}/` component specifically
- Dashboard: multi-tier recent memories (Episodic + Semantic + Procedural)
- server.json: v1.8.0, pinned OCI tag, MCP entrypoint
- Dockerfile: ENTRYPOINT preserved for backward compat

### Stats
- 489 tests, 0 failures

## v1.8.0 — Cross-Device Memory Sync

### One-Click Sync Setup
- `sync_enable` MCP tool: auto-detects cloud provider, generates device ID, AES-256-GCM passphrase
- `sync_pull` MCP tool: pull and apply remote changes from other devices
- `sync_status` now shows real sync state (device ID, provider, remote devices)
- CLI: `cortex-mcp-server sync enable` / `sync pull` subcommands

### Improved Install
- `--ide claude` now uses `claude mcp add` (correct Claude Code registration)
- New `--ide claude-desktop` for Claude Desktop app
- Post-install next steps guide

### Documentation
- Cross-device sync section in README (EN + CN)
- Japanese (README_JA.md) and Korean (README_KO.md) translations
- Configurable MCP server name via `CORTEX_SERVER_NAME` env var

### Stats
- 29 MCP tools, 485+ tests

## v1.7.0 — Private. Free. Local.

### Cloud Sync
- Changelog-based cross-device sync via iCloud Drive, Google Drive, OneDrive, Dropbox
- Hybrid Logical Clock (HLC) for causally consistent ordering across devices
- Last-Writer-Wins merge with CRDT belief merging and tombstone deletion
- Auto-detects macOS `~/Library/CloudStorage/` paths for all providers

### End-to-End Encryption
- **AES-256-GCM** encrypted sync oplog files (opt-in via passphrase)
- **Argon2id** key derivation (memory-hard, GPU/ASIC resistant)
- Per-line unique 12-byte random nonce — `ENC1:` format
- **SQLCipher** encrypted database at rest (default feature)
- `Cortex::open_encrypted(path, passphrase)` for full DB encryption

### Privacy Enforcement
- `PrivacyLevel::Private` (default) memories **never leave the device**
- Only `Shared` and `Public` memories are written to sync oplog
- Delete operations only synced for previously-synced memories
- `MemContent::zeroize_content()` — secure memory wiping for sensitive text

### Snapshot Bootstrap
- Zstd-compressed full database snapshots for new-device onboarding
- `create_snapshot()` / `restore_from_snapshot()` API
- New devices restore in seconds, then replay only newer oplog files

### Developer Experience
- `SyncConfig::with_encryption()` builder pattern
- `Cortex::enable_sync()` / `sync_pull()` / `sync_status()` API
- `EntityType` enum replaces stringly-typed entity references
- Extracted `bayesian_update()` as reusable function
- `Cortex::build()` refactor eliminates constructor duplication
- MCP tools: `sync_status`, `sync_providers` (27 total)

### Security Documentation
- `SECURITY.md` — threat model, encryption details, zero telemetry proof
- README comparison table: encryption, privacy levels, pricing vs competitors

### Testing
- **420+ tests**, 0 failures
- Full privacy chain e2e test (10 steps: DB encryption → sync → snapshot → zeroize)
- Real Google Drive integration test (auto-skips if not installed)
- 39 sync-specific tests covering all merge paths and edge cases

---

## v1.6.0

Int8 quantization (75% storage reduction), materialized column indexes, FTS5 triggers, LRU caches, rayon parallel decay, 25 MCP tools, batch inference, enhanced Chinese NLP.

## v1.5.0

Docker image (GHCR), batch ingest, dedup, namespace isolation, plugin system, event bus, archival, 351 tests.

## v1.0.0 — v1.4.0

Core memory engine: 4-tier memory model, Bayesian beliefs, people graph, consolidation, multi-signal retrieval, context injection, Chinese NLP, HNSW vector index, conversation compression, relationship inference.
