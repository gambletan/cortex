//! Cloud sync — changelog-based synchronization via cloud storage providers.
//!
//! Each device writes operation logs to its own subfolder in a shared cloud directory.
//! Other devices read and replay those logs to stay in sync.
//! Conflict resolution: Last-Writer-Wins (LWW) per entity using Hybrid Logical Clocks.

pub mod hlc;
pub mod merge;
pub mod oplog;
pub mod provider;
pub mod state;

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
        }
    }

    pub fn poll_interval(&self) -> Duration {
        Duration::from_secs(self.poll_interval_secs)
    }

    fn devices_dir(&self) -> PathBuf {
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

        let hlc = HlcClock::new(&config.device_id);
        let writer = OpLogWriter::new(my_dir)?;

        Ok(Self { config, hlc, writer })
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
    pub fn record_memory_event(
        &mut self,
        event: &CortexEvent,
        storage: &dyn StorageBackend,
    ) -> Result<(), CortexError> {
        match event {
            CortexEvent::MemoryCreated { id, .. } | CortexEvent::MemoryUpdated { id } => {
                if let Some(mem) = storage.get_memory(*id)? {
                    self.record_op(SyncPayload::MemoryUpsert { memory: mem })?;
                }
            }
            CortexEvent::MemoryDeleted { id } => {
                self.record_op(SyncPayload::MemoryDelete { id: *id })?;
            }
            CortexEvent::MemoryArchived { id } => {
                if let Some(mem) = storage.get_memory(*id)? {
                    self.record_op(SyncPayload::MemoryUpsert { memory: mem })?;
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
                let (ops, new_offset) = oplog::read_oplog(file_path, cursor)?;

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

    pub fn config(&self) -> &SyncConfig {
        &self.config
    }
}
