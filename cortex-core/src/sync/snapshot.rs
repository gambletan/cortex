//! Snapshot — compressed full exports for new-device bootstrap.
//!
//! Creates zstd-compressed JSON snapshots of the entire Cortex database.
//! New devices restore from the latest snapshot then replay only newer oplog files.

use crate::export::{self, ExportData, ImportData, ImportReport};
use crate::storage::memory_index::MemoryIndex;
use crate::storage::traits::StorageBackend;
use crate::sync::crypto::{self, CryptoContext};
use crate::CortexError;
use std::fs;
use std::path::{Path, PathBuf};

/// Suffix marking an encrypted snapshot.
const ENC_SUFFIX: &str = ".enc";

/// Create a compressed snapshot of the entire database.
/// Saved to `{sync_dir}/snapshots/snapshot-{date}.json.zst` (or `.json.zst.enc` when a
/// crypto context is supplied). When sync encryption is on, the snapshot — like the oplog
/// — is AES-256-GCM encrypted, so nothing cloud-bound is ever written in plaintext.
pub fn create_snapshot(
    storage: &dyn StorageBackend,
    snapshots_dir: &Path,
    crypto: Option<&CryptoContext>,
) -> Result<PathBuf, CortexError> {
    fs::create_dir_all(snapshots_dir)
        .map_err(|e| CortexError::Storage(format!("Failed to create snapshots dir: {}", e)))?;

    // SYNC BOUNDARY: snapshots are cloud-bound, so carry only syncable memories and no
    // derived entities. Private memories and people/beliefs/patterns never leave the
    // device — see export::export_for_sync (mirrors the oplog's record_memory_event).
    let data = export::export_for_sync(storage)?;
    let json = serde_json::to_vec(&data)
        .map_err(|e| CortexError::Serialization(e.to_string()))?;
    let compressed = zstd::encode_all(&json[..], 3)
        .map_err(|e| CortexError::Storage(format!("Zstd encode error: {}", e)))?;

    let date = chrono::Utc::now().format("%Y-%m-%d").to_string();
    let (filename, bytes) = match crypto {
        Some(ctx) => {
            // ENC1:<base64(nonce||ciphertext)> — same envelope as encrypted oplog lines.
            let line = crypto::encrypt_line(ctx, &compressed)?;
            (
                format!("snapshot-{}.json.zst{}", date, ENC_SUFFIX),
                line.into_bytes(),
            )
        }
        None => (format!("snapshot-{}.json.zst", date), compressed),
    };

    let path = snapshots_dir.join(&filename);
    fs::write(&path, &bytes)
        .map_err(|e| CortexError::Storage(format!("Snapshot write error: {}", e)))?;

    tracing::info!(path = %path.display(), size_bytes = bytes.len(), encrypted = crypto.is_some(), "Snapshot created");
    Ok(path)
}

/// Find the latest snapshot in the snapshots directory.
///
/// When `require_encrypted` is true (sync encryption is enabled), only `.enc` snapshots are
/// considered — a plaintext `*.json.zst` could only have been written by an attacker with
/// access to the untrusted cloud directory (the writer always encrypts when crypto is on),
/// so it must never win the recency sort and be selected for restore. This is the discovery
/// half of the defense; `restore_from_snapshot` rejects a downgraded path as a second layer.
pub fn find_latest_snapshot(snapshots_dir: &Path, require_encrypted: bool) -> Result<Option<PathBuf>, CortexError> {
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
        if !name.starts_with("snapshot-") {
            continue;
        }
        let is_encrypted = name.ends_with(".json.zst.enc");
        let is_snapshot = is_encrypted || name.ends_with(".json.zst");
        if is_snapshot && (!require_encrypted || is_encrypted) {
            snapshots.push(entry.path());
        }
    }

    // Order by the date key (filename with the .enc suffix stripped) so encryption mode
    // never changes recency ordering; break same-date ties by modification time.
    let date_key = |p: &PathBuf| {
        p.file_name()
            .map(|n| n.to_string_lossy().trim_end_matches(".enc").to_string())
            .unwrap_or_default()
    };
    let mtime = |p: &PathBuf| p.metadata().and_then(|m| m.modified()).ok();
    snapshots.sort_by(|a, b| date_key(a).cmp(&date_key(b)).then_with(|| mtime(a).cmp(&mtime(b))));
    Ok(snapshots.last().cloned())
}

/// Restore from a compressed snapshot.
/// Returns the import report and the snapshot's export timestamp.
pub fn restore_from_snapshot(
    path: &Path,
    storage: &dyn StorageBackend,
    index: &MemoryIndex,
    crypto: Option<&CryptoContext>,
) -> Result<(ImportReport, String), CortexError> {
    let raw = fs::read(path)
        .map_err(|e| CortexError::Storage(format!("Failed to open snapshot: {}", e)))?;

    let is_encrypted = path
        .file_name()
        .map(|n| n.to_string_lossy().ends_with(ENC_SUFFIX))
        .unwrap_or(false);

    // SECURITY: when sync encryption is enabled, legitimate snapshots are always `.enc`.
    // A plaintext snapshot reaching this point is a downgrade/injection attempt — an
    // attacker with write access to the untrusted cloud dir can forge a plaintext snapshot
    // with NO key, which would otherwise be imported wholesale on bootstrap. Reject it
    // rather than trust unauthenticated, unencrypted cloud data.
    if crypto.is_some() && !is_encrypted {
        return Err(CortexError::Storage(
            "Refusing to restore an unencrypted snapshot while sync encryption is enabled — possible downgrade or injection attack".into(),
        ));
    }

    // Encrypted snapshots (.enc) hold an ENC1: text envelope; decrypt to the zstd bytes.
    let compressed = if is_encrypted {
        let ctx = crypto.ok_or_else(|| {
            CortexError::Storage("Snapshot is encrypted but no key was provided".into())
        })?;
        let line = String::from_utf8(raw)
            .map_err(|e| CortexError::Storage(format!("Encrypted snapshot not UTF-8: {}", e)))?;
        crypto::decrypt_line(ctx, line.trim())?
    } else {
        raw
    };

    let json = zstd::decode_all(&compressed[..])
        .map_err(|e| CortexError::Storage(format!("Zstd decode error: {}", e)))?;

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

        // Snapshots carry syncable memories only, so seed Public memories.
        for text in ["I live in Shanghai", "I work at Google"] {
            let mem = crate::types::MemObjectBuilder::new(
                crate::MemoryTier::Episodic,
                crate::MemContent::Text(text.to_string()),
                crate::MemSource::new("test"),
            )
            .privacy(crate::PrivacyLevel::Public)
            .build();
            cortex.storage().store_memory(&mem).unwrap();
        }
        cortex.add_fact("Alice", "works_at", "Stripe", 0.9, "test", None).unwrap();
        cortex.observe_belief("user_is_dev", true, 0.8).unwrap();

        // Create snapshot
        let tmp = TempDir::new().unwrap();
        let snapshots_dir = tmp.path().join("snapshots");
        let path = create_snapshot(cortex.storage(), &snapshots_dir, None).unwrap();
        assert!(path.exists());
        assert!(path.to_string_lossy().ends_with(".json.zst"));

        // Verify file is actually compressed (smaller than raw JSON)
        let compressed_size = fs::metadata(&path).unwrap().len();
        let raw_data = export::export_for_sync(cortex.storage()).unwrap();
        let raw_size = serde_json::to_vec(&raw_data).unwrap().len() as u64;
        assert!(compressed_size < raw_size, "Compressed should be smaller than raw");

        // Restore to a new empty Cortex
        let cortex2 = crate::Cortex::in_memory().unwrap();
        let (report, _exported_at) = restore_from_snapshot(&path, cortex2.storage(), cortex2.index(), None).unwrap();
        assert!(report.memories > 0);
        // SYNC BOUNDARY: snapshots carry syncable memories only. Derived entities
        // (beliefs/people/patterns) have no privacy provenance and must not cross the
        // sync boundary — they are re-derived locally on the receiving device.
        assert_eq!(report.beliefs, 0, "snapshots must not carry beliefs");
        assert_eq!(report.people, 0, "snapshots must not carry people");

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
        assert!(find_latest_snapshot(&snapshots_dir, false).unwrap().is_none());

        // Create fake snapshot files
        fs::write(snapshots_dir.join("snapshot-2026-03-20.json.zst"), b"fake1").unwrap();
        fs::write(snapshots_dir.join("snapshot-2026-03-23.json.zst"), b"fake2").unwrap();
        fs::write(snapshots_dir.join("snapshot-2026-03-21.json.zst"), b"fake3").unwrap();

        let latest = find_latest_snapshot(&snapshots_dir, false).unwrap().unwrap();
        assert!(latest.to_string_lossy().contains("2026-03-23"), "Should find the latest by name sort");
    }

    #[test]
    fn test_find_latest_missing_dir() {
        let result = find_latest_snapshot(Path::new("/nonexistent/path"), false).unwrap();
        assert!(result.is_none());
    }

    /// SECURITY: when sync encryption is enabled (crypto context present), legitimate
    /// snapshots are always `.enc`. An attacker with write access to the untrusted cloud
    /// sync directory must not be able to bootstrap a new device with forged memories by
    /// dropping a plaintext `snapshot-<future-date>.json.zst` (NO key required). Such a
    /// file would otherwise win `find_latest_snapshot`'s recency sort and be imported by
    /// `restore_from_snapshot`'s plaintext branch — keyless injection on bootstrap.
    #[test]
    fn test_plaintext_snapshot_downgrade_rejected_when_encryption_enabled() {
        let ctx = crypto::derive_key("downgrade-test", &crypto::new_encryption_manifest()).unwrap();

        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("snapshots");

        // Legit encrypted snapshot from a real device.
        let cortex = crate::Cortex::in_memory().unwrap();
        let mem = crate::types::MemObjectBuilder::new(
            crate::MemoryTier::Episodic,
            crate::MemContent::Text("legit memory".to_string()),
            crate::MemSource::new("test"),
        )
        .privacy(crate::PrivacyLevel::Public)
        .build();
        cortex.storage().store_memory(&mem).unwrap();
        let enc_path = create_snapshot(cortex.storage(), &dir, Some(&ctx)).unwrap();

        // Attacker forges a plaintext snapshot carrying a memory, dated far in the future
        // so it sorts as the "latest". No key is needed to build this.
        let forged = ExportData {
            version: "1".to_string(),
            exported_at: "9999-12-31T00:00:00Z".to_string(),
            memories: vec![crate::types::MemObjectBuilder::new(
                crate::MemoryTier::Episodic,
                crate::MemContent::Text("ATTACKER-INJECTED".to_string()),
                crate::MemSource::new("attacker"),
            )
            .privacy(crate::PrivacyLevel::Public)
            .build()],
            people: vec![],
            beliefs: vec![],
            patterns: vec![],
        };
        let forged_json = serde_json::to_vec(&forged).unwrap();
        let forged_zst = zstd::encode_all(&forged_json[..], 3).unwrap();
        let forged_path = dir.join("snapshot-9999-12-31.json.zst");
        fs::write(&forged_path, &forged_zst).unwrap();

        // Discovery in encryption mode must ignore the plaintext file and pick the .enc one.
        let latest = find_latest_snapshot(&dir, true).unwrap().unwrap();
        assert_eq!(latest, enc_path, "encryption mode must only discover .enc snapshots");

        // Even if the forged path is fed directly, restore must reject it in encryption mode.
        let victim = crate::Cortex::in_memory().unwrap();
        let res = restore_from_snapshot(&forged_path, victim.storage(), victim.index(), Some(&ctx));
        assert!(
            res.is_err(),
            "plaintext snapshot must be rejected when encryption is enabled (keyless injection)"
        );
        assert_eq!(
            victim.stats().unwrap().total,
            0,
            "no attacker memory may be imported from a downgraded snapshot"
        );
    }

    #[test]
    fn test_encrypted_snapshot_roundtrip_and_not_plaintext() {
        let cortex = crate::Cortex::in_memory().unwrap();
        // Public memory so it is carried in the (syncable) snapshot.
        let mem = crate::types::MemObjectBuilder::new(
            crate::MemoryTier::Episodic,
            crate::MemContent::Text("I live in Shanghai".to_string()),
            crate::MemSource::new("test"),
        )
        .privacy(crate::PrivacyLevel::Public)
        .build();
        cortex.storage().store_memory(&mem).unwrap();
        cortex.observe_belief("user_is_dev", true, 0.8).unwrap();

        let ctx = crypto::derive_key("snapshot-test-pass", &crypto::new_encryption_manifest()).unwrap();

        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("snapshots");
        let path = create_snapshot(cortex.storage(), &dir, Some(&ctx)).unwrap();

        // Named .enc and discoverable by find_latest.
        assert!(path.to_string_lossy().ends_with(".json.zst.enc"));
        assert_eq!(find_latest_snapshot(&dir, true).unwrap().unwrap(), path);

        // On-disk bytes are the encrypted envelope — never plaintext memory content.
        let raw = fs::read(&path).unwrap();
        assert!(raw.starts_with(b"ENC1:"), "encrypted snapshot must use the ENC1: envelope");
        assert!(
            !String::from_utf8_lossy(&raw).contains("Shanghai"),
            "plaintext memory content must never appear in an encrypted snapshot"
        );

        // Restoring without the key fails; with the key it round-trips.
        let c_nokey = crate::Cortex::in_memory().unwrap();
        assert!(restore_from_snapshot(&path, c_nokey.storage(), c_nokey.index(), None).is_err());

        let c2 = crate::Cortex::in_memory().unwrap();
        let (report, _) =
            restore_from_snapshot(&path, c2.storage(), c2.index(), Some(&ctx)).unwrap();
        assert!(report.memories > 0);
        assert!(c2.stats().unwrap().total > 0);
    }
}
