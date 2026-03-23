//! Local sync state — persisted in the Cortex SQLite database.
//!
//! Tracks: device identity, HLC per entity, read cursors for remote oplogs, tombstones.

use crate::sync::hlc::HlcTimestamp;
use crate::CortexError;
use rusqlite::{params, Connection, OptionalExtension};
use uuid::Uuid;

// ── HLC serde helpers ────────────────────────────────────────────────────────

fn hlc_to_json(hlc: &HlcTimestamp) -> Result<String, CortexError> {
    serde_json::to_string(hlc).map_err(|e| CortexError::Serialization(e.to_string()))
}

fn hlc_from_json(json: &str) -> Result<HlcTimestamp, CortexError> {
    serde_json::from_str(json).map_err(|e| CortexError::Serialization(e.to_string()))
}

// ── Table initialization ─────────────────────────────────────────────────────

/// Initialize sync tables in the Cortex database. Idempotent.
pub fn init_sync_tables(conn: &Connection) -> Result<(), CortexError> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS sync_device (
            device_id TEXT PRIMARY KEY,
            device_name TEXT NOT NULL,
            created_at TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS sync_hlc (
            entity_type TEXT NOT NULL,
            entity_id TEXT NOT NULL,
            last_hlc TEXT NOT NULL,
            PRIMARY KEY (entity_type, entity_id)
        );

        CREATE TABLE IF NOT EXISTS sync_cursors (
            device_id TEXT NOT NULL,
            file_name TEXT NOT NULL,
            byte_offset INTEGER NOT NULL DEFAULT 0,
            PRIMARY KEY (device_id, file_name)
        );

        CREATE TABLE IF NOT EXISTS sync_tombstones (
            entity_type TEXT NOT NULL,
            entity_id TEXT NOT NULL,
            deleted_hlc TEXT NOT NULL,
            created_at TEXT NOT NULL,
            PRIMARY KEY (entity_type, entity_id)
        );",
    )
    .map_err(|e| CortexError::Storage(format!("Failed to init sync tables: {}", e)))
}

// ── Device identity ──────────────────────────────────────────────────────────

/// Get or create this device's identity.
pub fn get_or_create_device(
    conn: &Connection,
    device_id: &str,
    device_name: &str,
) -> Result<(), CortexError> {
    conn.execute(
        "INSERT OR IGNORE INTO sync_device (device_id, device_name, created_at) VALUES (?1, ?2, ?3)",
        params![device_id, device_name, chrono::Utc::now().to_rfc3339()],
    )
    .map_err(|e| CortexError::Storage(e.to_string()))?;
    Ok(())
}

// ── HLC per entity ───────────────────────────────────────────────────────────

/// Get the HLC for an entity, if tracked.
pub fn get_entity_hlc(
    conn: &Connection,
    entity_type: &str,
    entity_id: Uuid,
) -> Result<Option<HlcTimestamp>, CortexError> {
    let result = conn
        .query_row(
            "SELECT last_hlc FROM sync_hlc WHERE entity_type = ?1 AND entity_id = ?2",
            params![entity_type, entity_id.to_string()],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(|e| CortexError::Storage(e.to_string()))?;

    result.map(|json| hlc_from_json(&json)).transpose()
}

/// Set the HLC for an entity.
pub fn set_entity_hlc(
    conn: &Connection,
    entity_type: &str,
    entity_id: Uuid,
    hlc: &HlcTimestamp,
) -> Result<(), CortexError> {
    conn.execute(
        "INSERT OR REPLACE INTO sync_hlc (entity_type, entity_id, last_hlc) VALUES (?1, ?2, ?3)",
        params![entity_type, entity_id.to_string(), hlc_to_json(hlc)?],
    )
    .map_err(|e| CortexError::Storage(e.to_string()))?;
    Ok(())
}

// ── Read cursors ─────────────────────────────────────────────────────────────

/// Get the read cursor (byte offset) for a remote device's oplog file.
pub fn get_cursor(
    conn: &Connection,
    device_id: &str,
    file_name: &str,
) -> Result<u64, CortexError> {
    let result = conn
        .query_row(
            "SELECT byte_offset FROM sync_cursors WHERE device_id = ?1 AND file_name = ?2",
            params![device_id, file_name],
            |row| row.get::<_, i64>(0),
        )
        .optional()
        .map_err(|e| CortexError::Storage(e.to_string()))?;
    Ok(result.unwrap_or(0) as u64)
}

/// Update the read cursor for a remote device's oplog file.
pub fn set_cursor(
    conn: &Connection,
    device_id: &str,
    file_name: &str,
    byte_offset: u64,
) -> Result<(), CortexError> {
    conn.execute(
        "INSERT OR REPLACE INTO sync_cursors (device_id, file_name, byte_offset) VALUES (?1, ?2, ?3)",
        params![device_id, file_name, byte_offset as i64],
    )
    .map_err(|e| CortexError::Storage(e.to_string()))?;
    Ok(())
}

// ── Tombstones ───────────────────────────────────────────────────────────────

/// Check if an entity has a tombstone (was deleted).
pub fn is_tombstoned(
    conn: &Connection,
    entity_type: &str,
    entity_id: Uuid,
) -> Result<Option<HlcTimestamp>, CortexError> {
    let result = conn
        .query_row(
            "SELECT deleted_hlc FROM sync_tombstones WHERE entity_type = ?1 AND entity_id = ?2",
            params![entity_type, entity_id.to_string()],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(|e| CortexError::Storage(e.to_string()))?;

    result.map(|json| hlc_from_json(&json)).transpose()
}

/// Record a tombstone for a deleted entity.
pub fn set_tombstone(
    conn: &Connection,
    entity_type: &str,
    entity_id: Uuid,
    hlc: &HlcTimestamp,
) -> Result<(), CortexError> {
    conn.execute(
        "INSERT OR REPLACE INTO sync_tombstones (entity_type, entity_id, deleted_hlc, created_at) VALUES (?1, ?2, ?3, ?4)",
        params![entity_type, entity_id.to_string(), hlc_to_json(hlc)?, chrono::Utc::now().to_rfc3339()],
    )
    .map_err(|e| CortexError::Storage(e.to_string()))?;
    Ok(())
}

/// Remove old tombstones (garbage collection).
pub fn gc_tombstones(conn: &Connection, max_age_days: i64) -> Result<usize, CortexError> {
    let cutoff = (chrono::Utc::now() - chrono::Duration::days(max_age_days)).to_rfc3339();
    let count = conn
        .execute(
            "DELETE FROM sync_tombstones WHERE created_at < ?1",
            params![cutoff],
        )
        .map_err(|e| CortexError::Storage(e.to_string()))?;
    Ok(count)
}
