# Changelog

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
