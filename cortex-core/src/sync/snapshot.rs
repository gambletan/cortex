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
/// `encrypted` selects the snapshot mode of the calling device. In encryption mode only
/// `.json.zst.enc` snapshots are considered; in plaintext mode only bare `.json.zst`. This
/// is a security filter, not just cosmetics: the sync dir is untrusted, and considering
/// wrong-mode files would let an attacker drop a forged future-dated plaintext snapshot that
/// wins the date sort, gets picked, and then fails the downgrade check in
/// [`restore_from_snapshot`] — starving the device of a legitimate older `.enc` snapshot
/// (a bootstrap denial-of-service). Filtering by mode here means a wrong-mode decoy is never
/// selected in the first place.
pub fn find_latest_snapshot(
    snapshots_dir: &Path,
    encrypted: bool,
) -> Result<Option<PathBuf>, CortexError> {
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
        let is_enc = name.ends_with(".json.zst.enc");
        // `.json.zst.enc` ends with `.enc`, not `.json.zst`, so these are mutually exclusive.
        let is_plain = name.ends_with(".json.zst");
        // Only consider snapshots matching the caller's active mode.
        let matches_mode = if encrypted { is_enc } else { is_plain };
        if matches_mode {
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

    // Encrypted snapshots (.enc) hold an ENC1: text envelope; decrypt to the zstd bytes.
    let compressed = if path.to_string_lossy().ends_with(ENC_SUFFIX) {
        let ctx = crypto.ok_or_else(|| {
            CortexError::Storage("Snapshot is encrypted but no key was provided".into())
        })?;
        let line = String::from_utf8(raw)
            .map_err(|e| CortexError::Storage(format!("Encrypted snapshot not UTF-8: {}", e)))?;
        crypto::decrypt_line(ctx, line.trim())?
    } else if crypto.is_some() {
        // SECURITY: encryption is enabled, but this snapshot is not an encrypted (.enc)
        // envelope. The cloud sync directory is untrusted and `find_latest_snapshot` picks
        // by date, so an attacker with write access can drop a forged plaintext `.json.zst`
        // (e.g. a far-future date) that would otherwise be restored with NO key and NO
        // authentication — a key-less injection on new-device bootstrap. Restoring a
        // plaintext snapshot while encryption is active is a downgrade attack; fail closed.
        return Err(CortexError::Storage(
            "Rejecting plaintext snapshot while encryption is enabled — possible downgrade or injection attack".into(),
        ));
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

    /// SECURITY (availability): a forged, far-future-dated *plaintext* snapshot dropped into
    /// the untrusted sync dir must not be able to block bootstrap in encryption mode. If
    /// `find_latest_snapshot` picked purely by date it would select the forged plaintext file
    /// (which restore then rejects), starving the device of a legitimate older `.enc` snapshot.
    /// In encryption mode it must only consider `.enc` snapshots (and vice versa).
    #[test]
    fn test_find_latest_snapshot_is_encryption_mode_aware() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("snapshots");
        fs::create_dir_all(&dir).unwrap();

        // Legit encrypted snapshot (older date) + attacker's future-dated plaintext decoy.
        fs::write(dir.join("snapshot-2020-01-01.json.zst.enc"), b"ENC1:legit").unwrap();
        fs::write(dir.join("snapshot-9999-12-31.json.zst"), b"forged plaintext").unwrap();

        // Encryption mode must skip the plaintext decoy and pick the legit .enc snapshot.
        let enc_pick = find_latest_snapshot(&dir, true).unwrap().unwrap();
        assert!(
            enc_pick.to_string_lossy().ends_with(".json.zst.enc"),
            "encryption mode must ignore plaintext snapshots (got {})",
            enc_pick.display()
        );

        // Plaintext mode must ignore .enc snapshots and pick only plaintext ones.
        let plain_pick = find_latest_snapshot(&dir, false).unwrap().unwrap();
        assert!(
            plain_pick.to_string_lossy().ends_with(".json.zst")
                && !plain_pick.to_string_lossy().ends_with(ENC_SUFFIX),
            "plaintext mode must ignore encrypted snapshots (got {})",
            plain_pick.display()
        );
    }

    /// SECURITY: when encryption is enabled (a crypto context is present), a plaintext
    /// `.json.zst` snapshot must be REJECTED. The cloud sync directory is untrusted, and
    /// `find_latest_snapshot` will happily select a plaintext `.json.zst` by date. An
    /// attacker with write access can therefore drop a forged, key-less plaintext snapshot
    /// (e.g. a far-future date) and have it restored on new-device bootstrap — injecting
    /// arbitrary memories with NO key and NO authentication. Restoring a plaintext snapshot
    /// while encryption is active is a downgrade attack and must fail closed.
    #[test]
    fn test_plaintext_snapshot_rejected_when_encryption_enabled() {
        let cortex = crate::Cortex::in_memory().unwrap();
        let mem = crate::types::MemObjectBuilder::new(
            crate::MemoryTier::Episodic,
            crate::MemContent::Text("forged injected memory".to_string()),
            crate::MemSource::new("attacker"),
        )
        .privacy(crate::PrivacyLevel::Public)
        .build();
        cortex.storage().store_memory(&mem).unwrap();

        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("snapshots");
        // Attacker writes a *plaintext* snapshot (no crypto) into the untrusted dir.
        let plaintext_path = create_snapshot(cortex.storage(), &dir, None).unwrap();
        assert!(plaintext_path.to_string_lossy().ends_with(".json.zst"));
        assert!(!plaintext_path.to_string_lossy().ends_with(ENC_SUFFIX));

        // A device operating in encryption mode must refuse to restore it.
        let ctx = crypto::derive_key("snapshot-test-pass", &crypto::new_encryption_manifest()).unwrap();
        let victim = crate::Cortex::in_memory().unwrap();
        let result =
            restore_from_snapshot(&plaintext_path, victim.storage(), victim.index(), Some(&ctx));
        assert!(
            result.is_err(),
            "plaintext snapshot must be rejected when encryption is enabled (downgrade attack)"
        );
        assert_eq!(
            victim.stats().unwrap().total,
            0,
            "no forged memory may be injected from a plaintext snapshot in encryption mode"
        );
    }
}
