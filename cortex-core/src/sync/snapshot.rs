//! Snapshot — compressed full exports for new-device bootstrap.
//!
//! Creates zstd-compressed JSON snapshots of the entire Cortex database.
//! New devices restore from the latest snapshot then replay only newer oplog files.

use crate::export::{self, ExportData, ImportData, ImportReport};
use crate::storage::memory_index::MemoryIndex;
use crate::storage::traits::StorageBackend;
use crate::CortexError;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

/// Create a compressed snapshot of the entire database.
/// Saved to `{sync_dir}/snapshots/snapshot-{date}.json.zst`.
pub fn create_snapshot(
    storage: &dyn StorageBackend,
    snapshots_dir: &Path,
) -> Result<PathBuf, CortexError> {
    fs::create_dir_all(snapshots_dir)
        .map_err(|e| CortexError::Storage(format!("Failed to create snapshots dir: {}", e)))?;

    let data = export::export_all(storage)?;
    let json = serde_json::to_vec(&data)
        .map_err(|e| CortexError::Serialization(e.to_string()))?;

    let date = chrono::Utc::now().format("%Y-%m-%d").to_string();
    let filename = format!("snapshot-{}.json.zst", date);
    let path = snapshots_dir.join(&filename);

    let file = fs::File::create(&path)
        .map_err(|e| CortexError::Storage(format!("Failed to create snapshot: {}", e)))?;
    let mut encoder = zstd::Encoder::new(file, 3)
        .map_err(|e| CortexError::Storage(format!("Zstd encoder error: {}", e)))?;
    encoder.write_all(&json)
        .map_err(|e| CortexError::Storage(format!("Snapshot write error: {}", e)))?;
    encoder.finish()
        .map_err(|e| CortexError::Storage(format!("Snapshot finish error: {}", e)))?;

    tracing::info!(path = %path.display(), size_bytes = json.len(), "Snapshot created");
    Ok(path)
}

/// Find the latest snapshot in the snapshots directory.
pub fn find_latest_snapshot(snapshots_dir: &Path) -> Result<Option<PathBuf>, CortexError> {
    if !snapshots_dir.exists() {
        return Ok(None);
    }

    let mut snapshots: Vec<PathBuf> = Vec::new();
    let entries = fs::read_dir(snapshots_dir)
        .map_err(|e| CortexError::Storage(format!("Failed to read snapshots dir: {}", e)))?;

    for entry in entries {
        let entry = entry
            .map_err(|e| CortexError::Storage(format!("Failed to read dir entry: {}", e)))?;
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with("snapshot-") && name.ends_with(".json.zst") {
            snapshots.push(entry.path());
        }
    }

    snapshots.sort();
    Ok(snapshots.last().cloned())
}

/// Restore from a compressed snapshot.
/// Returns the import report and the snapshot's export timestamp.
pub fn restore_from_snapshot(
    path: &Path,
    storage: &dyn StorageBackend,
    index: &MemoryIndex,
) -> Result<(ImportReport, String), CortexError> {
    let file = fs::File::open(path)
        .map_err(|e| CortexError::Storage(format!("Failed to open snapshot: {}", e)))?;
    let mut decoder = zstd::Decoder::new(file)
        .map_err(|e| CortexError::Storage(format!("Zstd decoder error: {}", e)))?;

    let mut json = Vec::new();
    decoder.read_to_end(&mut json)
        .map_err(|e| CortexError::Storage(format!("Snapshot read error: {}", e)))?;

    let data: ExportData = serde_json::from_slice(&json)
        .map_err(|e| CortexError::Serialization(e.to_string()))?;

    let exported_at = data.exported_at.clone();

    let import_data = ImportData {
        version: Some(data.version),
        memories: Some(data.memories),
        people: Some(data.people),
        beliefs: Some(data.beliefs),
    };

    let report = export::import_all(storage, index, import_data)?;

    tracing::info!(
        memories = report.memories,
        people = report.people,
        beliefs = report.beliefs,
        "Snapshot restored"
    );

    Ok((report, exported_at))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_create_and_restore_snapshot() {
        let cortex = crate::Cortex::in_memory().unwrap();

        // Ingest some data
        cortex.ingest("I live in Shanghai", "test", None, None, None).unwrap();
        cortex.ingest("I work at Google", "test", None, None, None).unwrap();
        cortex.add_fact("Alice", "works_at", "Stripe", 0.9, "test", None).unwrap();
        cortex.observe_belief("user_is_dev", true, 0.8).unwrap();

        // Create snapshot
        let tmp = TempDir::new().unwrap();
        let snapshots_dir = tmp.path().join("snapshots");
        let path = create_snapshot(cortex.storage(), &snapshots_dir).unwrap();
        assert!(path.exists());
        assert!(path.to_string_lossy().ends_with(".json.zst"));

        // Verify file is actually compressed (smaller than raw JSON)
        let compressed_size = fs::metadata(&path).unwrap().len();
        let raw_data = export::export_all(cortex.storage()).unwrap();
        let raw_size = serde_json::to_vec(&raw_data).unwrap().len() as u64;
        assert!(compressed_size < raw_size, "Compressed should be smaller than raw");

        // Restore to a new empty Cortex
        let cortex2 = crate::Cortex::in_memory().unwrap();
        let (report, _exported_at) = restore_from_snapshot(&path, cortex2.storage(), cortex2.index()).unwrap();
        assert!(report.memories > 0);
        assert!(report.beliefs > 0);

        // Verify data was restored
        let stats = cortex2.stats().unwrap();
        assert!(stats.total > 0, "Should have restored memories");
    }

    #[test]
    fn test_find_latest_snapshot() {
        let tmp = TempDir::new().unwrap();
        let snapshots_dir = tmp.path().join("snapshots");
        fs::create_dir_all(&snapshots_dir).unwrap();

        // No snapshots
        assert!(find_latest_snapshot(&snapshots_dir).unwrap().is_none());

        // Create fake snapshot files
        fs::write(snapshots_dir.join("snapshot-2026-03-20.json.zst"), b"fake1").unwrap();
        fs::write(snapshots_dir.join("snapshot-2026-03-23.json.zst"), b"fake2").unwrap();
        fs::write(snapshots_dir.join("snapshot-2026-03-21.json.zst"), b"fake3").unwrap();

        let latest = find_latest_snapshot(&snapshots_dir).unwrap().unwrap();
        assert!(latest.to_string_lossy().contains("2026-03-23"), "Should find the latest by name sort");
    }

    #[test]
    fn test_find_latest_missing_dir() {
        let result = find_latest_snapshot(Path::new("/nonexistent/path")).unwrap();
        assert!(result.is_none());
    }
}
