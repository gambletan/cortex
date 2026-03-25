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

    fn my_device_dir(&self) -> PathBuf {
        self.devices_dir().join(&self.device_id)
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

/// Main sync engine.
pub struct SyncEngine {
    config: SyncConfig,
    hlc: HlcClock,
    writer: OpLogWriter,
    crypto: Option<std::sync::Arc<crypto::CryptoContext>>,
}

impl SyncEngine {
    /// Initialize the sync engine. Creates sync folder structure.
    /// Sync tables are initialized in SqliteStorage::init() — no separate connection needed.
    pub fn new(config: SyncConfig, storage: &SqliteStorage) -> Result<Self, CortexError> {
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
        let crypto_ctx = if let Some(ref passphrase) = config.encryption_passphrase {
            // Read or create encryption manifest
            let manifest_path = config.sync_dir.join("manifest.json");
            let manifest_json: serde_json::Value = if manifest_path.exists() {
                let content = fs::read_to_string(&manifest_path)
                    .map_err(|e| CortexError::Storage(format!("Failed to read manifest: {}", e)))?;
                serde_json::from_str(&content)
                    .map_err(|e| CortexError::Serialization(e.to_string()))?
            } else {
                serde_json::json!({})
            };

            let enc_manifest = if let Some(enc) = manifest_json.get("encryption") {
                serde_json::from_value::<crypto::EncryptionManifest>(enc.clone())
                    .map_err(|e| CortexError::Serialization(e.to_string()))?
            } else {
                // First time: generate salt and write to manifest
                let new_manifest = crypto::new_encryption_manifest();
                let mut updated = manifest_json.clone();
                updated["encryption"] = serde_json::to_value(&new_manifest)
                    .map_err(|e| CortexError::Serialization(e.to_string()))?;
                fs::write(&manifest_path, serde_json::to_string_pretty(&updated).unwrap())
                    .map_err(|e| CortexError::Storage(format!("Failed to write manifest: {}", e)))?;
                new_manifest
            };

            let ctx = crypto::derive_key(passphrase, &enc_manifest)?;
            Some(std::sync::Arc::new(ctx))
        } else {
            None
        };

        let hlc = HlcClock::new(&config.device_id);
        let writer = OpLogWriter::new(my_dir, crypto_ctx.clone())?;

        Ok(Self { config, hlc, writer, crypto: crypto_ctx })
    }

    /// Record a local mutation as a SyncOp in the oplog.
    pub fn record_op(&mut self, payload: SyncPayload) -> Result<(), CortexError> {
        let hlc = self.hlc.tick();
        let op = SyncOp {
            op_id: Uuid::new_v4(),
            hlc,
            payload,
        };
        self.writer.append(&op)
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
                // Only sync delete if the memory was previously synced
                let was_synced = storage.with_write_conn(|conn| {
                    state::get_entity_hlc(conn, state::EntityType::Memory, *id)
                })?.is_some();
                if was_synced {
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

        // Periodic tombstone GC
        let _ = storage.with_write_conn(|conn| {
            state::gc_tombstones(conn, self.config.tombstone_ttl_days)
        });

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
    pub fn create_snapshot(&self, storage: &dyn StorageBackend) -> Result<std::path::PathBuf, CortexError> {
        let snapshots_dir = self.config.sync_dir.join("snapshots");
        snapshot::create_snapshot(storage, &snapshots_dir)
    }

    /// Restore from the latest snapshot. Returns None if no snapshot exists.
    pub fn restore_from_snapshot(
        &self,
        storage: &dyn StorageBackend,
        index: &MemoryIndex,
    ) -> Result<Option<crate::export::ImportReport>, CortexError> {
        let snapshots_dir = self.config.sync_dir.join("snapshots");
        match snapshot::find_latest_snapshot(&snapshots_dir)? {
            Some(path) => {
                let (report, _) = snapshot::restore_from_snapshot(&path, storage, index)?;
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
