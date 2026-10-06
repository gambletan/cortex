//! Cloud sync — changelog-based synchronization via cloud storage providers.
//!
//! Each device writes operation logs to its own subfolder in a shared cloud directory.
//! Other devices read and replay those logs to stay in sync.
//! Conflict resolution: Last-Writer-Wins (LWW) per entity using Hybrid Logical Clocks.

pub mod crypto;
pub mod hlc;
pub mod merge;
pub mod oplog;
pub mod provider;
pub mod secret;
pub mod snapshot;
pub mod state;
pub mod watcher;

use crate::storage::memory_index::MemoryIndex;
use crate::storage::sqlite::SqliteStorage;
use crate::storage::traits::StorageBackend;
use crate::sync::hlc::HlcClock;
use crate::sync::oplog::{OpLogWriter, SyncOp, SyncPayload};
use crate::types::*;
use crate::CortexError;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use uuid::Uuid;
use zeroize::Zeroize;

/// Configuration for cloud sync.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncConfig {
    /// Path to the sync folder (cloud-synced directory).
    pub sync_dir: PathBuf,
    /// This device's unique ID.
    pub device_id: String,
    /// This device's human-readable name.
    pub device_name: String,
    /// How often to poll for remote changes (default: 30s).
    #[serde(default = "default_poll_interval_secs")]
    pub poll_interval_secs: u64,
    /// Tombstone retention in days (default: 30).
    #[serde(default = "default_tombstone_ttl_days")]
    pub tombstone_ttl_days: i64,
    /// Optional passphrase for encrypting oplog files (AES-256-GCM).
    /// Never serialized to disk.
    #[serde(default, skip_serializing)]
    pub encryption_passphrase: Option<String>,
}

fn default_poll_interval_secs() -> u64 { 30 }
fn default_tombstone_ttl_days() -> i64 { 30 }

impl SyncConfig {
    pub fn new(sync_dir: PathBuf, device_id: String, device_name: String) -> Self {
        Self {
            sync_dir,
            device_id,
            device_name,
            poll_interval_secs: default_poll_interval_secs(),
            tombstone_ttl_days: default_tombstone_ttl_days(),
            encryption_passphrase: None,
        }
    }

    /// Set encryption passphrase for oplog files.
    pub fn with_encryption(mut self, passphrase: impl Into<String>) -> Self {
        self.encryption_passphrase = Some(passphrase.into());
        self
    }

    pub fn poll_interval(&self) -> Duration {
        Duration::from_secs(self.poll_interval_secs)
    }

    pub fn devices_dir(&self) -> PathBuf {
        self.sync_dir.join("devices")
    }

    pub fn my_device_dir(&self) -> PathBuf {
        self.devices_dir().join(&self.device_id)
    }
}

impl Drop for SyncConfig {
    fn drop(&mut self) {
        // Zeroize passphrase from memory when config is dropped
        if let Some(ref mut passphrase) = self.encryption_passphrase {
            passphrase.zeroize();
        }
    }
}

/// Sync status report.
#[derive(Debug, Clone, Serialize)]
pub struct SyncStatus {
    pub enabled: bool,
    pub device_id: String,
    pub device_name: String,
    pub sync_dir: String,
    pub provider: String,
    pub remote_devices: Vec<RemoteDevice>,
    pub pending_ops: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct RemoteDevice {
    pub device_id: String,
    pub oplog_files: usize,
}

/// How many snapshots of the current mode to keep (the pinned one is always kept too).
const SNAPSHOTS_TO_KEEP: usize = 3;

/// Main sync engine.
pub struct SyncEngine {
    config: SyncConfig,
    hlc: HlcClock,
    writer: OpLogWriter,
    crypto: Option<std::sync::Arc<crypto::CryptoContext>>,
    /// Encryption mode only: the sync group's KDF salt (keys the local generation mark).
    group_salt: Option<String>,
    /// Anti-rollback marks for the manifest (in memory; at least the persisted marks).
    marks: parking_lot::Mutex<ManifestMarks>,
}

/// What this device has accepted from the (untrusted) manifest so far, per sync group.
#[derive(Debug, Clone, Default)]
struct ManifestMarks {
    /// Highest `generation` accepted; a lower one is a rollback.
    generation: u64,
    /// Highest `key_version` accepted or written; a lower one is repaired, never adopted.
    key_version: u32,
    /// Identity + pointer of the manifest accepted at `generation` (None if unknown, e.g. a
    /// mark recorded before identities were tracked). A different manifest at the same
    /// generation is a fork, resolved in favour of this one.
    accepted: Option<(String, Option<crypto::SnapshotPointer>)>,
}

impl ManifestMarks {
    fn load(storage: &SqliteStorage, salt: &str) -> Result<Self, CortexError> {
        storage.with_write_conn(|conn| {
            let generation = state::get_manifest_generation(conn, salt)?;
            let key_version = state::get_manifest_key_version(conn, salt)?;
            let accepted = state::get_accepted_manifest(conn, salt)?
                .filter(|a| a.generation == generation)
                .map(|a| (a.identity, a.pointer));
            Ok(Self { generation, key_version, accepted })
        })
    }

    fn persist(&self, storage: &SqliteStorage, salt: &str) -> Result<(), CortexError> {
        storage.with_write_conn(|conn| {
            state::raise_manifest_generation(conn, salt, self.generation)?;
            state::raise_manifest_key_version(conn, salt, self.key_version)?;
            if let Some((identity, pointer)) = &self.accepted {
                state::record_accepted_manifest(
                    conn,
                    salt,
                    &state::AcceptedManifest {
                        generation: self.generation,
                        identity: identity.clone(),
                        pointer: pointer.clone(),
                    },
                )?;
            }
            Ok(())
        })
    }

    /// Record a manifest that passed [`enforce_manifest_marks`] (or that we just wrote).
    fn accept(&mut self, m: &crypto::EncryptionManifest) -> Result<(), CortexError> {
        let generation = m.generation.unwrap_or(0);
        if generation >= self.generation {
            self.generation = generation;
            self.accepted = Some((manifest_identity(m)?, m.latest_snapshot.clone()));
        }
        self.key_version = self.key_version.max(m.key_version.unwrap_or(0));
        Ok(())
    }
}

/// Content identity of a manifest: SHA-256 over its hmac-free serialization (the bytes the
/// manifest HMAC signs), so it is stable across re-signing with a fresh HMAC salt.
fn manifest_identity(m: &crypto::EncryptionManifest) -> Result<String, CortexError> {
    use sha2::{Digest, Sha256};
    let mut m = m.clone();
    m.hmac = None;
    m.hmac_salt = None;
    let bytes = serde_json::to_vec(&m).map_err(|e| CortexError::Serialization(e.to_string()))?;
    Ok(Sha256::digest(&bytes).iter().map(|b| format!("{b:02x}")).collect())
}

impl SyncEngine {
    /// Initialize the sync engine. Creates sync folder structure.
    /// Sync tables are initialized in SqliteStorage::init() — no separate connection needed.
    pub fn new(config: SyncConfig, storage: &SqliteStorage) -> Result<Self, CortexError> {
        // Without a key, refuse to join a group that is (or looks) encrypted — before writing
        // anything into the shared folder.
        if config.encryption_passphrase.is_none() {
            ensure_plaintext_group(&config.sync_dir)?;
        }

        // Create sync directory structure
        let my_dir = config.my_device_dir();
        fs::create_dir_all(&my_dir)
            .map_err(|e| CortexError::Storage(format!("Failed to create sync dir: {}", e)))?;

        // Write manifest if it doesn't exist
        let manifest_path = config.sync_dir.join("manifest.json");
        if !manifest_path.exists() {
            let manifest = serde_json::json!({
                "version": "cortex-sync-v1",
                "schema_version": 1,
                "created_at": chrono::Utc::now().to_rfc3339(),
            });
            fs::write(&manifest_path, serde_json::to_string_pretty(&manifest).unwrap())
                .map_err(|e| CortexError::Storage(format!("Failed to write manifest: {}", e)))?;
        }

        // Write device.json
        let device_json = serde_json::json!({
            "device_id": config.device_id,
            "device_name": config.device_name,
            "os": std::env::consts::OS,
            "cortex_version": env!("CARGO_PKG_VERSION"),
            "last_active": chrono::Utc::now().to_rfc3339(),
        });
        fs::write(
            my_dir.join("device.json"),
            serde_json::to_string_pretty(&device_json).unwrap(),
        )
        .map_err(|e| CortexError::Storage(format!("Failed to write device.json: {}", e)))?;

        // Register device in sync tables (tables created during SqliteStorage::init)
        storage.with_write_conn(|conn| {
            state::get_or_create_device(conn, &config.device_id, &config.device_name)
        })?;

        // Set up encryption if passphrase is provided
        let (crypto_ctx, group_salt, marks) = if let Some(ref passphrase) = config.encryption_passphrase {
            let (enc_manifest, mut marks) = match load_verified_manifest(&manifest_path, passphrase)? {
                (manifest_json, Some(manifest)) => {
                    // Anti-rollback against what this device accepted before (persisted
                    // locally, per sync group): see `enforce_manifest_marks`.
                    let marks = ManifestMarks::load(storage, &manifest.salt)?;
                    let m = enforce_manifest_marks(&manifest_path, passphrase, manifest_json, manifest, &marks)?.1;
                    (m, marks)
                }
                // First time: generate salt and write an HMAC-protected encryption block.
                (manifest_json, None) => (
                    write_encryption_manifest(
                        &manifest_path,
                        manifest_json,
                        crypto::new_encryption_manifest(),
                        passphrase,
                    )?,
                    ManifestMarks::default(),
                ),
            };
            marks.accept(&enc_manifest)?;
            marks.persist(storage, &enc_manifest.salt)?;
            let ctx = crypto::derive_key(passphrase, &enc_manifest)?;
            (Some(std::sync::Arc::new(ctx)), Some(enc_manifest.salt.clone()), marks)
        } else {
            (None, None, ManifestMarks::default())
        };

        let hlc = HlcClock::new(&config.device_id);
        let writer = OpLogWriter::new(my_dir, crypto_ctx.clone())?;

        Ok(Self {
            config,
            hlc,
            writer,
            crypto: crypto_ctx,
            group_salt,
            marks: parking_lot::Mutex::new(marks),
        })
    }

    /// Load + verify the manifest and enforce the anti-rollback marks (raising the in-memory
    /// marks on success): a lower generation is an error; a lower key version is a writer
    /// conflict that gets repaired (see [`enforce_manifest_marks`]). See
    /// [`load_verified_manifest`].
    fn load_manifest_checked(
        &self,
        passphrase: &str,
    ) -> Result<(serde_json::Value, Option<crypto::EncryptionManifest>), CortexError> {
        let manifest_path = self.config.sync_dir.join("manifest.json");
        let (json, manifest) = load_verified_manifest(&manifest_path, passphrase)?;
        if let Some(m) = &manifest {
            if self.group_salt.as_deref() != Some(m.salt.as_str()) {
                return Err(CortexError::Storage(
                    "Encryption manifest now belongs to a different sync group (salt changed) — refusing; \
                     re-enable sync to join it"
                        .into(),
                ));
            }
        }
        let Some(m) = manifest else { return Ok((json, None)) };
        let mut marks = self.marks.lock();
        let (json, m) = enforce_manifest_marks(&manifest_path, passphrase, json, m, &marks)?;
        marks.accept(&m)?;
        Ok((json, Some(m)))
    }

    /// Sign and write the manifest (bumping its generation) and raise the in-memory mark.
    fn write_manifest(
        &self,
        manifest_json: serde_json::Value,
        manifest: crypto::EncryptionManifest,
        passphrase: &str,
    ) -> Result<crypto::EncryptionManifest, CortexError> {
        let manifest_path = self.config.sync_dir.join("manifest.json");
        let manifest = write_encryption_manifest(&manifest_path, manifest_json, manifest, passphrase)?;
        self.marks.lock().accept(&manifest)?;
        Ok(manifest)
    }

    /// Persist the in-memory anti-rollback marks (generation, key version, accepted manifest
    /// identity + pointer) to the local database (never lowers them).
    fn persist_manifest_generation(&self, storage: &SqliteStorage) -> Result<(), CortexError> {
        if let Some(salt) = &self.group_salt {
            let marks = self.marks.lock().clone();
            marks.persist(storage, salt)?;
        }
        Ok(())
    }

    /// Rotate the sync encryption key forward by one version.
    ///
    /// Existing oplog/snapshot data stays readable under its original version; new writes use
    /// the new version's key, which is derived from the passphrase independently of prior
    /// versions (forward secrecy against AES-key exfiltration). Bumps `key_version` in the
    /// manifest, recomputes the manifest HMAC, re-derives the crypto context, and rebuilds the
    /// writer to encrypt under the new version. Returns the new active version. Requires sync
    /// encryption to be enabled.
    ///
    /// Takes the local storage so the new manifest generation is persisted immediately: a
    /// crash right after rotating must not leave a window where the pre-rotation manifest
    /// could be replayed.
    pub fn rotate_key(&mut self, storage: &SqliteStorage) -> Result<u32, CortexError> {
        let passphrase = self.config.encryption_passphrase.clone().ok_or_else(|| {
            CortexError::Storage("Cannot rotate key: sync encryption is not enabled".into())
        })?;
        let my_dir = self.config.my_device_dir();

        // Verify before bumping: re-signing an unverified manifest would launder tampering.
        let (manifest_json, manifest) = self.load_manifest_checked(&passphrase)?;
        let mut manifest = manifest.ok_or_else(|| {
            CortexError::Storage("Cannot rotate key: manifest has no encryption block".into())
        })?;

        // Bump the active version and re-sign. Every other field — including the snapshot
        // pointer — is carried over unchanged.
        let new_version = manifest.key_version.unwrap_or(0) + 1;
        manifest.key_version = Some(new_version);
        let manifest = self.write_manifest(manifest_json, manifest, &passphrase)?;
        self.persist_manifest_generation(storage)?;

        // Re-derive at the new version and rebuild the writer so new lines use the new key.
        let ctx = std::sync::Arc::new(crypto::derive_key(&passphrase, &manifest)?);
        self.writer = OpLogWriter::new(my_dir, Some(ctx.clone()))?;
        self.crypto = Some(ctx);
        Ok(new_version)
    }

    /// Record a local mutation as a SyncOp in the oplog.
    pub fn record_op(&mut self, payload: SyncPayload) -> Result<(), CortexError> {
        let hlc = self.hlc.tick();
        let op = SyncOp {
            op_id: Uuid::new_v4(),
            hlc,
            payload,
            hmac: None, // HMAC computed by writer
        };
        self.writer.append(op)
    }

    /// Record a memory event from the EventBus.
    /// Only Shared/Public memories are synced — Private (the default) never leaves the device.
    pub fn record_memory_event(
        &mut self,
        event: &CortexEvent,
        storage: &SqliteStorage,
    ) -> Result<(), CortexError> {
        match event {
            CortexEvent::MemoryCreated { id, .. } | CortexEvent::MemoryUpdated { id } => {
                if let Some(mem) = storage.get_memory(*id)? {
                    if mem.privacy.is_syncable() {
                        self.record_op(SyncPayload::MemoryUpsert { memory: mem })?;
                    }
                }
            }
            CortexEvent::MemoryDeleted { id } => {
                // Only sync delete if the memory was previously synced AND is/was syncable
                // Prevent leaking Private memory deletion via observable HLC timestamps
                let was_syncable = storage.with_write_conn(|conn| {
                    // Check if memory was synced (had an HLC entry) before deletion
                    if state::get_entity_hlc(conn, state::EntityType::Memory, *id)?.is_some() {
                        // Memory was previously tracked, check if it was syncable
                        // For deleted memories, we can't check current privacy, so assume Private
                        // was NOT syncable if it's being deleted. Only sync deletes of memories
                        // that we know were explicitly synced (Shared/Public)
                        Ok::<bool, CortexError>(true)
                    } else {
                        Ok(false)
                    }
                })?;

                // For safety, only sync the delete if we're confident it was Public/Shared
                // Check if there's any sync history indicating this was a shared memory
                if was_syncable {
                    // Double-check: if we still have the memory, verify it's syncable
                    if let Ok(Some(mem)) = storage.get_memory(*id) {
                        if !mem.privacy.is_syncable() {
                            // Memory exists and is Private — don't sync the delete
                            return Ok(());
                        }
                    }
                    self.record_op(SyncPayload::MemoryDelete { id: *id })?;
                }
            }
            CortexEvent::MemoryArchived { id } => {
                if let Some(mem) = storage.get_memory(*id)? {
                    if mem.privacy.is_syncable() {
                        self.record_op(SyncPayload::MemoryUpsert { memory: mem })?;
                    }
                }
            }
            // Consolidation and decay are local-only operations
            CortexEvent::ConsolidationCompleted { .. }
            | CortexEvent::DecayCompleted { .. } => {}
        }
        Ok(())
    }

    /// Pull and merge remote changes from all other devices.
    /// Returns the number of operations applied.
    pub fn pull_remote(
        &mut self,
        storage: &SqliteStorage,
        index: &MemoryIndex,
    ) -> Result<usize, CortexError> {
        // Flush any in-memory manifest generation (e.g. from `rotate_key`) to the local mark.
        self.persist_manifest_generation(storage)?;

        // Follow key rotations made by other devices, so this device stops writing under a
        // retired key even if it never creates a snapshot. Also re-checks for rollback.
        if let Some(passphrase) = self.config.encryption_passphrase.clone() {
            if let (_, Some(manifest)) = self.load_manifest_checked(&passphrase)? {
                self.adopt_key_version(&manifest, &passphrase)?;
                self.persist_manifest_generation(storage)?;
            }
        }

        let devices_dir = self.config.devices_dir();
        if !devices_dir.exists() {
            return Ok(0);
        }

        let mut total_applied = 0;

        // Scan for other device directories
        let entries = fs::read_dir(&devices_dir)
            .map_err(|e| CortexError::Storage(format!("Failed to read devices dir: {}", e)))?;

        for entry in entries {
            let entry = entry
                .map_err(|e| CortexError::Storage(format!("Failed to read dir entry: {}", e)))?;

            let dir_name = entry.file_name().to_string_lossy().to_string();
            if dir_name == self.config.device_id {
                continue; // Skip our own device
            }

            if !entry.path().is_dir() {
                continue;
            }

            // Read oplog files for this remote device
            let oplog_files = oplog::list_oplog_files(&entry.path())?;
            for file_path in &oplog_files {
                let file_name = file_path
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_string();

                // Get cursor for this file
                let cursor = storage.with_write_conn(|conn| {
                    state::get_cursor(conn, &dir_name, &file_name)
                })?;

                // Read new operations from cursor
                let (ops, new_offset) = oplog::read_oplog(
                    file_path,
                    cursor,
                    self.crypto.as_deref(),
                )?;

                if ops.is_empty() {
                    continue;
                }

                // Apply each operation
                for op in &ops {
                    // Validate that the operation's device_id matches the directory it came from
                    // Prevents device spoofing where attacker creates sync/devices/legitimate-device/
                    // and fills it with operations claiming a different device_id
                    if op.hlc.device_id != dir_name {
                        tracing::warn!(
                            "Rejecting operation from device '{}' in directory '{}' — device ID mismatch",
                            op.hlc.device_id, dir_name
                        );
                        continue;
                    }

                    // Advance local HLC past remote
                    self.hlc.update(&op.hlc);

                    let result = merge::apply_op(op, storage, index);

                    match result {
                        Ok(merge::MergeResult::Applied) => {
                            total_applied += 1;
                            tracing::debug!(
                                op_id = %op.op_id,
                                device = %op.hlc.device_id,
                                "Sync: applied remote op"
                            );
                        }
                        Ok(result) => {
                            tracing::debug!(
                                op_id = %op.op_id,
                                result = ?result,
                                "Sync: skipped remote op"
                            );
                        }
                        Err(e) => {
                            tracing::warn!(
                                op_id = %op.op_id,
                                error = %e,
                                "Sync: failed to apply remote op"
                            );
                        }
                    }
                }

                // Update cursor
                storage.with_write_conn(|conn| {
                    state::set_cursor(conn, &dir_name, &file_name, new_offset)
                })?;
            }
        }

        // Periodic tombstone GC — don't fail the pull if it errors, but never swallow it
        // silently: a recurring failure would otherwise let tombstones grow unbounded with
        // zero observability.
        if let Err(e) = storage.with_write_conn(|conn| {
            state::gc_tombstones(conn, self.config.tombstone_ttl_days)
        }) {
            tracing::warn!(error = %e, "tombstone GC failed");
        }

        Ok(total_applied)
    }

    /// Get sync status.
    pub fn status(&self) -> Result<SyncStatus, CortexError> {
        let devices_dir = self.config.devices_dir();
        let mut remote_devices = Vec::new();

        if devices_dir.exists() {
            if let Ok(entries) = fs::read_dir(&devices_dir) {
                for entry in entries.flatten() {
                    let dir_name = entry.file_name().to_string_lossy().to_string();
                    if dir_name == self.config.device_id || !entry.path().is_dir() {
                        continue;
                    }
                    let files = oplog::list_oplog_files(&entry.path()).unwrap_or_default();
                    remote_devices.push(RemoteDevice {
                        device_id: dir_name,
                        oplog_files: files.len(),
                    });
                }
            }
        }

        // Detect provider
        let provider = provider::detect_all_providers()
            .into_iter()
            .find(|p| self.config.sync_dir.starts_with(p.sync_dir.parent().unwrap_or(&p.sync_dir)))
            .map(|p| p.provider.as_str().to_string())
            .unwrap_or_else(|| "Custom".to_string());

        Ok(SyncStatus {
            enabled: true,
            device_id: self.config.device_id.clone(),
            device_name: self.config.device_name.clone(),
            sync_dir: self.config.sync_dir.display().to_string(),
            provider,
            remote_devices,
            pending_ops: 0,
        })
    }

    /// Create a compressed snapshot for new-device bootstrap.
    ///
    /// In encryption mode the snapshot is also pinned in the manifest (`latest_snapshot`,
    /// HMAC-protected), which is what [`Self::restore_from_snapshot`] restores — so a stale
    /// snapshot can't be replayed and a newer one can't be suppressed into a fallback.
    ///
    /// Ordering: (1) load + verify the manifest and, if another device rotated the key, adopt
    /// the current key version first, so the snapshot is never written under a retired key;
    /// (2) write the snapshot under a fresh unique name (never overwriting the pinned one);
    /// (3) only then publish the pointer; (4) prune old unpinned snapshots. A crash at any
    /// point leaves the previous pin intact and restorable.
    pub fn create_snapshot(&mut self, storage: &SqliteStorage) -> Result<std::path::PathBuf, CortexError> {
        let snapshots_dir = self.config.sync_dir.join("snapshots");
        let encrypted = self.crypto.is_some();
        let Some(passphrase) = self.config.encryption_passphrase.clone().filter(|_| encrypted) else {
            let path = snapshot::create_snapshot(storage, &snapshots_dir, None)?;
            snapshot::prune_snapshots(&snapshots_dir, false, SNAPSHOTS_TO_KEEP, &path);
            return Ok(path);
        };

        let (_, manifest) = self.load_manifest_checked(&passphrase)?;
        let manifest = manifest.ok_or_else(|| {
            CortexError::Storage("Encryption manifest disappeared — cannot create snapshot".into())
        })?;
        self.adopt_key_version(&manifest, &passphrase)?;
        let key_version = manifest.key_version;

        let (path, mac) =
            snapshot::create_snapshot_with_mac(storage, &snapshots_dir, self.crypto.as_deref())?;
        let mac = mac.ok_or_else(|| CortexError::Storage("Encrypted snapshot has no MAC".into()))?;

        // Re-read right before publishing to shrink the race with other writers. If the key
        // was rotated meanwhile, don't pin a snapshot written under the now-retired key.
        let (manifest_json, manifest) = self.load_manifest_checked(&passphrase)?;
        let mut manifest = manifest.ok_or_else(|| {
            CortexError::Storage("Encryption manifest disappeared — cannot pin snapshot".into())
        })?;
        if manifest.key_version != key_version {
            let _ = fs::remove_file(&path);
            return Err(CortexError::Storage(
                "Sync key was rotated while the snapshot was being written — retry".into(),
            ));
        }
        // Never publish a pointer to a file that isn't there (e.g. removed by another device).
        if !path.is_file() {
            return Err(CortexError::Storage(
                "Snapshot disappeared before it could be published — retry".into(),
            ));
        }
        let file = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        manifest.latest_snapshot = Some(crypto::SnapshotPointer { file, mac });
        self.write_manifest(manifest_json, manifest, &passphrase)?;
        self.persist_manifest_generation(storage)?;

        snapshot::prune_snapshots(&snapshots_dir, true, SNAPSHOTS_TO_KEEP, &path);
        Ok(path)
    }

    /// If the (verified) manifest's key version differs from ours — another device rotated —
    /// re-derive the crypto context and rebuild the oplog writer, as `rotate_key` does, so
    /// new data is written under the current key. An older manifest version is refused.
    fn adopt_key_version(
        &mut self,
        manifest: &crypto::EncryptionManifest,
        passphrase: &str,
    ) -> Result<(), CortexError> {
        let current = self.crypto.as_ref().map(|c| c.active_version()).unwrap_or(0);
        let wanted = manifest.key_version.unwrap_or(0);
        if wanted == current {
            return Ok(());
        }
        if wanted < current {
            return Err(CortexError::Storage(format!(
                "Manifest key version {wanted} is older than this device's active version {current} — \
                 refusing (possible key-version rollback)"
            )));
        }
        let ctx = std::sync::Arc::new(crypto::derive_key(passphrase, manifest)?);
        self.writer = OpLogWriter::new(self.config.my_device_dir(), Some(ctx.clone()))?;
        self.crypto = Some(ctx);
        tracing::info!(from = current, to = wanted, "Adopted rotated sync key version");
        Ok(())
    }

    /// Restore from the authoritative snapshot. Returns None if no snapshot exists.
    ///
    /// Encryption mode: the manifest is re-read, verified, and checked against this device's
    /// generation high-water mark (anti-rollback); if it pins a snapshot, exactly
    /// that file is restored and its HMAC line must equal the pinned one. Any failure is an
    /// error — there is deliberately no fallback to an older snapshot (that would let an
    /// attacker resurrect deleted data by corrupting the newest one). Legacy groups without a
    /// pointer, and plaintext mode, use the newest validly-named snapshot, also without
    /// fallback.
    pub fn restore_from_snapshot(
        &mut self,
        storage: &SqliteStorage,
        index: &MemoryIndex,
    ) -> Result<Option<crate::export::ImportReport>, CortexError> {
        let snapshots_dir = self.config.sync_dir.join("snapshots");
        if let (true, Some(passphrase)) =
            (self.crypto.is_some(), self.config.encryption_passphrase.clone())
        {
            // Verified, and not older than any manifest this device has accepted.
            let (_, manifest) = self.load_manifest_checked(&passphrase)?;
            self.persist_manifest_generation(storage)?;
            let manifest = manifest.ok_or_else(|| {
                CortexError::Storage(
                    "Encryption manifest is missing — refusing to restore (possible downgrade)".into(),
                )
            })?;
            // Another device may have rotated since this engine started: adopt the current key
            // version, or a pinned snapshot under it would look like a future version.
            self.adopt_key_version(&manifest, &passphrase)?;
            let ctx = self.crypto.as_deref().ok_or_else(|| CortexError::Storage("no crypto context".into()))?;
            if let Some(ptr) = manifest.latest_snapshot {
                // The pointer is authenticated, but still only accept a plain snapshot name.
                if snapshot::snapshot_date(&ptr.file, true).is_none() {
                    return Err(CortexError::Storage("Manifest snapshot pointer has an invalid file name".into()));
                }
                let (report, _) =
                    snapshot::restore_pinned(&snapshots_dir.join(&ptr.file), storage, index, ctx, &ptr.mac)?;
                return Ok(Some(report));
            }
        }
        match snapshot::find_latest_snapshot(&snapshots_dir, self.crypto.is_some())? {
            Some(path) => {
                let (report, _) =
                    snapshot::restore_from_snapshot(&path, storage, index, self.crypto.as_deref())?;
                Ok(Some(report))
            }
            None => Ok(None),
        }
    }

    pub fn config(&self) -> &SyncConfig {
        &self.config
    }

    /// Start background sync: a polling thread that calls `pull_remote()` on the
    /// configured interval, plus a filesystem watcher that triggers immediate pulls
    /// when remote .jsonl files change (with 2-second debounce).
    ///
    /// The returned `BackgroundSyncHandle` stops both when `stop()` is called or on drop.
    pub fn start_background_sync(
        config: SyncConfig,
        storage: Arc<SqliteStorage>,
        index: Arc<MemoryIndex>,
        engine: Arc<parking_lot::Mutex<SyncEngine>>,
    ) -> Result<BackgroundSyncHandle, CortexError> {
        let stop_flag = Arc::new(AtomicBool::new(false));

        // --- Polling thread ---
        let poll_stop = stop_flag.clone();
        let poll_storage = storage.clone();
        let poll_index = index.clone();
        let poll_engine = engine.clone();
        let poll_interval = config.poll_interval();

        let poll_thread = std::thread::Builder::new()
            .name("cortex-sync-poll".into())
            .spawn(move || {
                tracing::info!(interval = ?poll_interval, "Background sync polling started");
                while !poll_stop.load(Ordering::Acquire) {
                    // Sleep in small increments so we can check the stop flag
                    let start = std::time::Instant::now();
                    while start.elapsed() < poll_interval {
                        if poll_stop.load(Ordering::Acquire) {
                            return;
                        }
                        std::thread::sleep(Duration::from_millis(200));
                    }

                    if poll_stop.load(Ordering::Acquire) {
                        break;
                    }

                    let mut guard = poll_engine.lock();
                    match guard.pull_remote(&poll_storage, &poll_index) {
                        Ok(0) => {}
                        Ok(n) => tracing::info!(applied = n, "Background sync: pulled remote changes"),
                        Err(e) => tracing::warn!(error = %e, "Background sync: pull failed"),
                    }
                }
                tracing::debug!("Background sync polling thread exiting");
            })
            .map_err(|e| CortexError::Storage(format!("Failed to spawn poll thread: {}", e)))?;

        // --- Filesystem watcher ---
        let watch_stop = stop_flag.clone();
        let watch_storage = storage;
        let watch_index = index;
        let watch_engine = engine;

        let watcher_handle = watcher::start_watcher(
            watcher::WatcherConfig {
                watch_dir: config.devices_dir(),
                device_id: config.device_id.clone(),
                debounce: Duration::from_secs(2),
            },
            Box::new(move || {
                if watch_stop.load(Ordering::Acquire) {
                    return;
                }
                let mut guard = watch_engine.lock();
                match guard.pull_remote(&watch_storage, &watch_index) {
                    Ok(0) => {}
                    Ok(n) => tracing::info!(applied = n, "Watcher sync: pulled remote changes"),
                    Err(e) => tracing::warn!(error = %e, "Watcher sync: pull failed"),
                }
            }),
        )?;

        Ok(BackgroundSyncHandle {
            stop_flag,
            poll_thread: Some(poll_thread),
            watcher_handle: Some(watcher_handle),
        })
    }
}

/// Read `manifest.json` and, if it has an `encryption` block, verify that block's HMAC.
///
/// Returns the whole manifest JSON (so callers can rewrite it preserving other fields) and
/// the verified encryption manifest with `hmac`/`hmac_salt` stripped. A missing file is an
/// empty manifest; an unreadable or unparsable one is an error. The HMAC is mandatory: the
/// manifest sits in plaintext on untrusted storage and the HMAC is the only thing binding
/// `salt`, `kdf_params`, `key_version` and the snapshot pointer (a stripped HMAC would
/// otherwise allow key-version rollback, defeating forward secrecy).
fn load_verified_manifest(
    manifest_path: &std::path::Path,
    passphrase: &str,
) -> Result<(serde_json::Value, Option<crypto::EncryptionManifest>), CortexError> {
    let manifest_json: serde_json::Value = match fs::read_to_string(manifest_path) {
        Ok(content) => serde_json::from_str(&content).map_err(|e| CortexError::Serialization(e.to_string()))?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => serde_json::json!({}),
        Err(e) => return Err(CortexError::Storage(format!("Failed to read manifest: {}", e))),
    };
    let Some(enc) = manifest_json.get("encryption") else {
        return Ok((manifest_json, None));
    };
    let mut manifest = serde_json::from_value::<crypto::EncryptionManifest>(enc.clone())
        .map_err(|e| CortexError::Serialization(e.to_string()))?;
    let stored_hmac = manifest.hmac.take().ok_or_else(|| {
        CortexError::Storage(
            "Encryption manifest is missing its integrity HMAC — refusing to load. \
             This indicates tampering or a downgrade attack on the sync directory."
                .into(),
        )
    })?;
    let stored_salt = manifest.hmac_salt.take();
    let manifest_without_hmac =
        serde_json::to_vec(&manifest).map_err(|e| CortexError::Serialization(e.to_string()))?;
    if !crypto::verify_manifest_integrity(&manifest_without_hmac, &stored_hmac, passphrase, stored_salt.as_deref())? {
        return Err(CortexError::Storage(
            "Manifest integrity check failed: HMAC mismatch. Data may be corrupted or tampered.".into(),
        ));
    }
    Ok((manifest_json, Some(manifest)))
}

/// Apply this device's anti-rollback marks to a verified manifest.
///
/// - `generation < marks.generation` → a replayed older manifest: `Err` ("manifest rollback").
/// - `generation == marks.generation` but different content than the manifest this device
///   accepted at that generation → a fork (two devices published from the same parent, or a
///   replay of the losing branch). Resolved deterministically in favour of what this device
///   accepted: re-publish at `generation + 1` with the accepted snapshot pointer (or the
///   fork's, if this device accepted none). The other branch is then strictly older and
///   rejected as a rollback from now on.
/// - `key_version < marks.key_version` → a writer that hadn't seen the latest rotation.
///   Never adopt the lower version; re-publish with `key_version = marks.key_version`.
///
/// Re-publishing re-signs the manifest (generation bumped) and keeps all other fields.
/// Returns the (possibly repaired) manifest JSON and encryption manifest (hmac stripped).
fn enforce_manifest_marks(
    manifest_path: &std::path::Path,
    passphrase: &str,
    manifest_json: serde_json::Value,
    manifest: crypto::EncryptionManifest,
    marks: &ManifestMarks,
) -> Result<(serde_json::Value, crypto::EncryptionManifest), CortexError> {
    check_generation(&manifest, marks.generation)?;
    let mut repaired = manifest.clone();
    let mut reasons = Vec::new();
    if manifest.generation.unwrap_or(0) == marks.generation {
        if let Some((identity, pointer)) = &marks.accepted {
            if *identity != manifest_identity(&manifest)? {
                // Keep the snapshot this device accepted. If it had none (e.g. its accepted
                // manifest was a rotation from an unpinned parent), the fork's pointer is a
                // sibling made from the same parent, not an older one — keep it rather than
                // unpinning every snapshot.
                if pointer.is_some() {
                    repaired.latest_snapshot = pointer.clone();
                }
                reasons.push("same-generation fork");
            }
        }
    }
    if manifest.key_version.unwrap_or(0) < marks.key_version {
        repaired.key_version = Some(marks.key_version);
        reasons.push("older key version");
    }
    if reasons.is_empty() {
        return Ok((manifest_json, manifest));
    }
    tracing::warn!(
        reasons = ?reasons,
        generation = manifest.generation.unwrap_or(0),
        "Sync manifest conflicts with what this device accepted (concurrent writer or replay); \
         re-publishing a resolved manifest at the next generation"
    );
    let mut repaired = write_encryption_manifest(manifest_path, manifest_json.clone(), repaired, passphrase)?;
    let mut json = manifest_json;
    json["encryption"] =
        serde_json::to_value(&repaired).map_err(|e| CortexError::Serialization(e.to_string()))?;
    repaired.hmac = None;
    repaired.hmac_salt = None;
    Ok((json, repaired))
}

/// Reject a manifest older than the newest generation this device has accepted (`seen`).
/// Legacy manifests without a generation count as 0, so once a device has seen a
/// generation, a replayed pre-generation manifest is also rejected.
fn check_generation(manifest: &crypto::EncryptionManifest, seen: u64) -> Result<(), CortexError> {
    let found = manifest.generation.unwrap_or(0);
    if found < seen {
        return Err(CortexError::Storage(format!(
            "Manifest rollback detected: sync manifest generation {found} is older than generation \
             {seen} already accepted by this device — refusing to use a replayed manifest"
        )));
    }
    Ok(())
}

/// Sign `manifest` (HMAC over its hmac-free serialization, generation bumped), store it as the `encryption`
/// block of `manifest_json`, and atomically replace `manifest.json` (temp file + rename).
/// Returns the signed manifest.
fn write_encryption_manifest(
    manifest_path: &std::path::Path,
    mut manifest_json: serde_json::Value,
    mut manifest: crypto::EncryptionManifest,
    passphrase: &str,
) -> Result<crypto::EncryptionManifest, CortexError> {
    manifest.hmac = None;
    manifest.hmac_salt = None;
    // Every write is a new generation (first creation: None → 1).
    manifest.generation = Some(manifest.generation.unwrap_or(0) + 1);
    let bytes = serde_json::to_vec(&manifest).map_err(|e| CortexError::Serialization(e.to_string()))?;
    let (hmac_value, hmac_salt) = crypto::compute_manifest_hmac(&bytes, passphrase)?;
    manifest.hmac = Some(hmac_value);
    manifest.hmac_salt = Some(hmac_salt);
    manifest_json["encryption"] =
        serde_json::to_value(&manifest).map_err(|e| CortexError::Serialization(e.to_string()))?;
    let text = serde_json::to_string_pretty(&manifest_json)
        .map_err(|e| CortexError::Serialization(e.to_string()))?;

    let dir = manifest_path.parent().unwrap_or(std::path::Path::new("."));
    let tmp = dir.join(format!(".manifest.json.tmp-{}", Uuid::new_v4()));
    fs::write(&tmp, text)
        .and_then(|_| fs::rename(&tmp, manifest_path))
        .map_err(|e| {
            let _ = fs::remove_file(&tmp);
            CortexError::Storage(format!("Failed to write manifest: {}", e))
        })?;
    Ok(manifest)
}

/// Refuse to run in plaintext mode in a sync folder that belongs to an encrypted group.
///
/// Signals, any of which is enough: the manifest has an `encryption` block; the manifest
/// exists but can't be read or parsed (fail closed — can't tell); any device's oplog starts
/// with an encrypted line; or an encrypted snapshot exists. The last two catch an attacker
/// who deletes `manifest.json` to trick a key-less device into writing plaintext into the
/// group.
fn ensure_plaintext_group(sync_dir: &std::path::Path) -> Result<(), CortexError> {
    let refuse = |why: &str| {
        Err(CortexError::InvalidInput(format!(
            "This sync folder appears to be encrypted ({why}); a passphrase is required to join it."
        )))
    };

    let manifest_path = sync_dir.join("manifest.json");
    match fs::read_to_string(&manifest_path) {
        Ok(content) => match serde_json::from_str::<serde_json::Value>(&content) {
            Ok(m) if m.get("encryption").is_some() => return refuse("manifest has an encryption block"),
            Ok(_) => {}
            Err(_) => return refuse("manifest.json is unparsable"),
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => return refuse("manifest.json is unreadable"),
    }

    let devices_dir = sync_dir.join("devices");
    if let Ok(entries) = fs::read_dir(&devices_dir) {
        for entry in entries.flatten() {
            if !entry.path().is_dir() {
                continue;
            }
            for file in oplog::list_oplog_files(&entry.path())? {
                if oplog_starts_encrypted(&file) {
                    return refuse("a device oplog contains encrypted lines");
                }
            }
        }
    }

    let snapshots_dir = sync_dir.join("snapshots");
    if !snapshot::list_snapshots(&snapshots_dir, true)?.is_empty() {
        return refuse("an encrypted snapshot is present");
    }
    Ok(())
}

/// Whether the first non-empty line of an oplog file is an encrypted envelope. Reads at most
/// a few KiB. Unreadable files count as not encrypted (they can't be replayed either).
fn oplog_starts_encrypted(path: &std::path::Path) -> bool {
    use std::io::Read;
    let mut head = Vec::new();
    let Ok(file) = fs::File::open(path) else { return false };
    if file.take(4096).read_to_end(&mut head).is_err() {
        return false;
    }
    let text = String::from_utf8_lossy(&head);
    crypto::is_encrypted_line(text.trim_start())
}

/// Handle for stopping background sync (polling + filesystem watcher).
pub struct BackgroundSyncHandle {
    stop_flag: Arc<AtomicBool>,
    poll_thread: Option<std::thread::JoinHandle<()>>,
    watcher_handle: Option<watcher::WatcherHandle>,
}

impl BackgroundSyncHandle {
    /// Create a new handle from pre-built components.
    pub fn new(
        stop_flag: Arc<AtomicBool>,
        poll_thread: std::thread::JoinHandle<()>,
        watcher_handle: watcher::WatcherHandle,
    ) -> Self {
        Self {
            stop_flag,
            poll_thread: Some(poll_thread),
            watcher_handle: Some(watcher_handle),
        }
    }

    /// Stop both the polling thread and the filesystem watcher.
    pub fn stop(mut self) {
        self.shutdown();
    }

    /// Check if background sync is still running.
    pub fn is_running(&self) -> bool {
        !self.stop_flag.load(Ordering::Acquire)
    }

    /// Signal stop without joining the thread. Safe to call from any thread
    /// including the background sync thread itself (avoids self-join deadlock).
    pub fn signal_stop(&mut self) {
        self.stop_flag.store(true, Ordering::Release);
        if let Some(wh) = self.watcher_handle.take() {
            wh.stop();
        }
        // Don't join — thread will exit on its own when it checks stop_flag
        // or when Weak::upgrade fails.
    }

    fn shutdown(&mut self) {
        self.stop_flag.store(true, Ordering::Release);
        if let Some(wh) = self.watcher_handle.take() {
            wh.stop();
        }
        if let Some(th) = self.poll_thread.take() {
            let _ = th.join();
        }
    }
}

impl Drop for BackgroundSyncHandle {
    fn drop(&mut self) {
        // Check if we're on the bg sync thread — if so, only signal (no join).
        let is_bg_thread = std::thread::current().name() == Some("cortex-bg-sync");
        if is_bg_thread {
            self.signal_stop();
        } else {
            self.shutdown();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// Write a correctly-encrypted, correctly-HMAC'd snapshot under an arbitrary (older) name —
    /// i.e. a genuine older snapshot an attacker kept and can put back.
    fn plant_valid_snapshot(engine: &SyncEngine, source: &crate::Cortex, name: &str) {
        use base64::Engine;
        let ctx = engine.crypto.as_deref().unwrap();
        let json = serde_json::to_vec(&crate::export::export_for_sync(source.storage()).unwrap()).unwrap();
        let line = crypto::encrypt_line(ctx, &zstd::encode_all(&json[..], 3).unwrap()).unwrap();
        let mac = base64::engine::general_purpose::STANDARD
            .encode(ctx.compute_operation_hmac(&snapshot::snapshot_mac_input(name, &line)));
        let dir = engine.config.sync_dir.join("snapshots");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(name), format!("{line}\nHMAC:{mac}\n")).unwrap();
    }

    fn cortex_with(text: &str) -> crate::Cortex {
        let c = crate::Cortex::in_memory().unwrap();
        let mem = MemObjectBuilder::new(MemoryTier::Episodic, MemContent::Text(text.into()), MemSource::new("t"))
            .privacy(PrivacyLevel::Public)
            .build();
        c.storage().store_memory(&mem).unwrap();
        c
    }

    #[test]
    fn corrupted_pinned_snapshot_does_not_fall_back_to_an_older_valid_one() {
        let tmp = TempDir::new().unwrap();
        let sync_dir = tmp.path().join("sync");
        let cfg = |dev: &str| SyncConfig::new(sync_dir.clone(), dev.into(), dev.into()).with_encryption("pass-123");

        let a = cortex_with("current");
        let mut engine_a = SyncEngine::new(cfg("a"), a.sqlite_storage()).unwrap();
        let pinned = engine_a.create_snapshot(a.sqlite_storage()).unwrap();
        plant_valid_snapshot(&engine_a, &cortex_with("deleted long ago"), "snapshot-2001-01-01.json.zst.enc");

        let b = crate::Cortex::in_memory().unwrap();
        let mut engine_b = SyncEngine::new(cfg("b"), b.sqlite_storage()).unwrap();

        // Intact pin: exactly the pinned snapshot is restored.
        let report = engine_b.restore_from_snapshot(b.sqlite_storage(), b.index()).unwrap().unwrap();
        assert_eq!(report.memories, 1);

        // Corrupted pin: error, and the older (genuine) snapshot is NOT used.
        fs::write(&pinned, "ENC1:garbage\nHMAC:AAAA\n").unwrap();
        let c = crate::Cortex::in_memory().unwrap();
        assert!(engine_b.restore_from_snapshot(c.sqlite_storage(), c.index()).is_err());
        assert_eq!(c.stats().unwrap().total, 0);
    }

    #[test]
    fn legacy_unpinned_group_uses_newest_snapshot_without_fallback() {
        let tmp = TempDir::new().unwrap();
        let sync_dir = tmp.path().join("sync");
        let cfg = |dev: &str| SyncConfig::new(sync_dir.clone(), dev.into(), dev.into()).with_encryption("pass-123");
        let a = cortex_with("old");
        let engine_a = SyncEngine::new(cfg("a"), a.sqlite_storage()).unwrap();
        plant_valid_snapshot(&engine_a, &a, "snapshot-2001-01-01.json.zst.enc");
        // Newest-named snapshot is junk: no fallback to the valid older one.
        fs::write(sync_dir.join("snapshots/snapshot-2002-01-01.json.zst.enc"), "ENC1:x\nHMAC:AAAA\n").unwrap();

        let b = crate::Cortex::in_memory().unwrap();
        let mut engine_b = SyncEngine::new(cfg("b"), b.sqlite_storage()).unwrap();
        assert!(engine_b.restore_from_snapshot(b.sqlite_storage(), b.index()).is_err());
        assert_eq!(b.stats().unwrap().total, 0);
    }
}
