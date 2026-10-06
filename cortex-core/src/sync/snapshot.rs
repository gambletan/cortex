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
const HMAC_PREFIX: &str = "HMAC:";

/// Upper bounds on what restore will read from the (untrusted) sync folder.
#[derive(Debug, Clone, Copy)]
pub(crate) struct SnapshotLimits {
    /// Size of the snapshot file on disk.
    pub max_file_bytes: u64,
    /// Size of the zstd-decompressed JSON (guards against decompression bombs).
    pub max_decompressed_bytes: u64,
}

impl SnapshotLimits {
    pub(crate) const DEFAULT: Self = Self {
        max_file_bytes: 256 * 1024 * 1024,
        max_decompressed_bytes: 1024 * 1024 * 1024,
    };
}

/// HMAC input for an encrypted snapshot: domain-separated from op HMACs and bound to the
/// file name (its date), so a snapshot can't be replayed under another name or forged by
/// someone holding only a (leaked or rotated-out) content key.
pub(crate) fn snapshot_mac_input(file_name: &str, envelope: &str) -> Vec<u8> {
    let mut v = b"cortex-snapshot-v1\0".to_vec();
    v.extend_from_slice(file_name.as_bytes());
    v.push(0);
    v.extend_from_slice(envelope.as_bytes());
    v
}

/// `snapshot-YYYY-MM-DD.json.zst` (plaintext) / `….json.zst.enc` (encrypted) with a real date.
pub(crate) fn snapshot_date(name: &str, encrypted: bool) -> Option<chrono::NaiveDate> {
    let suffix = if encrypted { ".json.zst.enc" } else { ".json.zst" };
    let date = name.strip_prefix("snapshot-")?.strip_suffix(suffix)?;
    if date.len() != 10 {
        return None;
    }
    chrono::NaiveDate::parse_from_str(date, "%Y-%m-%d").ok()
}

/// Create a compressed snapshot of the entire database.
/// Saved to `{sync_dir}/snapshots/snapshot-{date}.json.zst` (or `.json.zst.enc` when a
/// crypto context is supplied). When sync encryption is on, the snapshot — like the oplog
/// — is AES-256-GCM encrypted, so nothing cloud-bound is ever written in plaintext.
pub fn create_snapshot(
    storage: &dyn StorageBackend,
    snapshots_dir: &Path,
    crypto: Option<&CryptoContext>,
) -> Result<PathBuf, CortexError> {
    Ok(create_snapshot_with_mac(storage, snapshots_dir, crypto)?.0)
}

/// [`create_snapshot`], also returning the `HMAC:` line value of an encrypted snapshot so
/// the caller can pin it in the manifest (`None` in plaintext mode).
pub(crate) fn create_snapshot_with_mac(
    storage: &dyn StorageBackend,
    snapshots_dir: &Path,
    crypto: Option<&CryptoContext>,
) -> Result<(PathBuf, Option<String>), CortexError> {
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
    let (filename, bytes, mac) = match crypto {
        Some(ctx) => {
            use base64::Engine;
            // Line 1: the same envelope as encrypted oplog lines (ENC1/ENC2).
            // Line 2: HMAC:<base64> over the name + envelope (see `snapshot_mac_input`).
            let line = crypto::encrypt_line(ctx, &compressed)?;
            let filename = format!("snapshot-{}.json.zst{}", date, ENC_SUFFIX);
            let mac = base64::engine::general_purpose::STANDARD
                .encode(ctx.compute_operation_hmac(&snapshot_mac_input(&filename, &line)));
            let bytes = format!("{line}\n{HMAC_PREFIX}{mac}\n").into_bytes();
            (filename, bytes, Some(mac))
        }
        None => (format!("snapshot-{}.json.zst", date), compressed, None),
    };

    // Write to a temp file and rename, so a reader never sees a half-written snapshot.
    let path = snapshots_dir.join(&filename);
    let tmp = snapshots_dir.join(format!(".{}.tmp-{}", filename, uuid::Uuid::new_v4()));
    fs::write(&tmp, &bytes)
        .and_then(|_| fs::rename(&tmp, &path))
        .map_err(|e| {
            let _ = fs::remove_file(&tmp);
            CortexError::Storage(format!("Snapshot write error: {}", e))
        })?;

    tracing::info!(path = %path.display(), size_bytes = bytes.len(), encrypted = crypto.is_some(), "Snapshot created");
    Ok((path, mac))
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
pub fn list_snapshots(snapshots_dir: &Path, encrypted: bool) -> Result<Vec<PathBuf>, CortexError> {
    if !snapshots_dir.exists() {
        return Ok(Vec::new());
    }
    let entries = fs::read_dir(snapshots_dir)
        .map_err(|e| CortexError::Storage(format!("Failed to read snapshots dir: {}", e)))?;
    let mut snapshots = Vec::new();
    for entry in entries {
        let entry = entry
            .map_err(|e| CortexError::Storage(format!("Failed to read dir entry: {}", e)))?;
        let name = entry.file_name().to_string_lossy().to_string();
        // Strict name check (current mode + a real date): a junk name like `snapshot-zzzz…`
        // can't sort itself to the front. In encryption mode the manifest's snapshot pointer
        // decides what is restored; this ordering only matters for legacy (unpinned) groups.
        if let Some(date) = snapshot_date(&name, encrypted) {
            let mtime = entry.metadata().and_then(|m| m.modified()).ok();
            snapshots.push((date, mtime, entry.path()));
        }
    }
    snapshots.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| b.1.cmp(&a.1)));
    Ok(snapshots.into_iter().map(|(_, _, p)| p).collect())
}

/// Newest snapshot for the given mode (see [`list_snapshots`]).
pub fn find_latest_snapshot(
    snapshots_dir: &Path,
    encrypted: bool,
) -> Result<Option<PathBuf>, CortexError> {
    Ok(list_snapshots(snapshots_dir, encrypted)?.into_iter().next())
}

/// Restore from a compressed snapshot.
/// Returns the import report and the snapshot's export timestamp.
pub fn restore_from_snapshot(
    path: &Path,
    storage: &dyn StorageBackend,
    index: &MemoryIndex,
    crypto: Option<&CryptoContext>,
) -> Result<(ImportReport, String), CortexError> {
    restore_with_limits(path, storage, index, crypto, None, SnapshotLimits::DEFAULT)
}

/// [`restore_from_snapshot`] that additionally requires an encrypted snapshot's `HMAC:` line
/// to equal `expected_mac` (the manifest's snapshot pointer), so a validly-authenticated but
/// stale snapshot replayed under the same name is rejected.
pub(crate) fn restore_pinned(
    path: &Path,
    storage: &dyn StorageBackend,
    index: &MemoryIndex,
    crypto: &CryptoContext,
    expected_mac: &str,
) -> Result<(ImportReport, String), CortexError> {
    restore_with_limits(path, storage, index, Some(crypto), Some(expected_mac), SnapshotLimits::DEFAULT)
}

pub(crate) fn restore_with_limits(
    path: &Path,
    storage: &dyn StorageBackend,
    index: &MemoryIndex,
    crypto: Option<&CryptoContext>,
    expected_mac: Option<&str>,
    limits: SnapshotLimits,
) -> Result<(ImportReport, String), CortexError> {
    use std::io::Read;

    // Check the size before reading, and bound the read itself (the file may grow between
    // the stat and the read).
    let file = fs::File::open(path)
        .map_err(|e| CortexError::Storage(format!("Failed to open snapshot: {}", e)))?;
    let too_large = || {
        CortexError::Storage(format!(
            "Snapshot file too large (limit {} bytes) — rejected",
            limits.max_file_bytes
        ))
    };
    let len = file
        .metadata()
        .map_err(|e| CortexError::Storage(format!("Failed to stat snapshot: {}", e)))?
        .len();
    if len > limits.max_file_bytes {
        return Err(too_large());
    }
    let mut raw = Vec::with_capacity(len as usize);
    file.take(limits.max_file_bytes.saturating_add(1))
        .read_to_end(&mut raw)
        .map_err(|e| CortexError::Storage(format!("Failed to read snapshot: {}", e)))?;
    if raw.len() as u64 > limits.max_file_bytes {
        return Err(too_large());
    }

    // Encrypted snapshots (.enc) hold an ENC1/ENC2 text envelope; decrypt to the zstd bytes.
    let compressed = if path.to_string_lossy().ends_with(ENC_SUFFIX) {
        let ctx = crypto.ok_or_else(|| {
            CortexError::Storage("Snapshot is encrypted but no key was provided".into())
        })?;
        let text = String::from_utf8(raw)
            .map_err(|e| CortexError::Storage(format!("Encrypted snapshot not UTF-8: {}", e)))?;
        let mut lines = text.lines();
        let envelope = lines.next().unwrap_or("").trim();
        let mac = lines
            .next()
            .and_then(|l| l.trim().strip_prefix(HMAC_PREFIX))
            .ok_or_else(|| CortexError::Storage("Encrypted snapshot has no integrity HMAC — rejected".into()))?;
        if let Some(expected) = expected_mac {
            if mac != expected {
                return Err(CortexError::Storage(
                    "Snapshot does not match the manifest's snapshot pointer — stale or replayed, rejected".into(),
                ));
            }
        }
        let file_name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        if !ctx.verify_operation_hmac(&snapshot_mac_input(&file_name, envelope), mac)? {
            return Err(CortexError::Storage("Snapshot HMAC mismatch — tampered or forged".into()));
        }
        // After a key rotation, envelopes under any version older than the active one use a
        // rotated-out key (whoever leaked it could have produced them) — reject them.
        match crypto::envelope_version(envelope) {
            Some(v) if v >= ctx.active_version() => {}
            _ => {
                return Err(CortexError::Storage(
                    "Snapshot uses a rotated-out key version — rejected".into(),
                ))
            }
        }
        crypto::decrypt_line(ctx, envelope)?
    } else if crypto.is_some() {
        // SECURITY: encryption is enabled, but this snapshot is not an encrypted (.enc)
        // envelope. The cloud sync directory is untrusted, so an attacker with write access
        // can drop a forged plaintext `.json.zst` that would otherwise be restored with NO key
        // and NO authentication — a key-less injection on new-device bootstrap. Restoring a
        // plaintext snapshot while encryption is active is a downgrade attack; fail closed.
        return Err(CortexError::Storage(
            "Rejecting plaintext snapshot while encryption is enabled — possible downgrade or injection attack".into(),
        ));
    } else {
        raw
    };

    // Stream-decompress with a cap so a small zstd bomb can't exhaust memory.
    let decoder = zstd::stream::read::Decoder::new(&compressed[..])
        .map_err(|e| CortexError::Storage(format!("Zstd decode error: {}", e)))?;
    let mut json = Vec::new();
    decoder
        .take(limits.max_decompressed_bytes.saturating_add(1))
        .read_to_end(&mut json)
        .map_err(|e| CortexError::Storage(format!("Zstd decode error: {}", e)))?;
    if json.len() as u64 > limits.max_decompressed_bytes {
        return Err(CortexError::Storage(format!(
            "Snapshot decompressed size exceeds limit of {} bytes — rejected",
            limits.max_decompressed_bytes
        )));
    }

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

    fn public_cortex() -> crate::Cortex {
        let cortex = crate::Cortex::in_memory().unwrap();
        let mem = crate::types::MemObjectBuilder::new(
            crate::MemoryTier::Episodic,
            crate::MemContent::Text("I live in Shanghai".to_string()),
            crate::MemSource::new("test"),
        )
        .privacy(crate::PrivacyLevel::Public)
        .build();
        cortex.storage().store_memory(&mem).unwrap();
        cortex
    }

    #[test]
    fn test_encrypted_snapshot_requires_valid_hmac() {
        let cortex = public_cortex();
        let ctx = crypto::derive_key("snapshot-test-pass", &crypto::new_encryption_manifest()).unwrap();
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("snapshots");
        let path = create_snapshot(cortex.storage(), &dir, Some(&ctx)).unwrap();
        let target = crate::Cortex::in_memory().unwrap();

        // Stripped HMAC (envelope only): rejected even though the key decrypts it.
        let raw = fs::read_to_string(&path).unwrap();
        let envelope = raw.lines().next().unwrap().to_string();
        fs::write(&path, format!("{envelope}\n")).unwrap();
        assert!(restore_from_snapshot(&path, target.storage(), target.index(), Some(&ctx)).is_err());

        // Valid HMAC but replayed under another date: rejected (name is bound).
        fs::write(&path, &raw).unwrap();
        let renamed = dir.join("snapshot-2001-01-01.json.zst.enc");
        fs::copy(&path, &renamed).unwrap();
        assert!(restore_from_snapshot(&renamed, target.storage(), target.index(), Some(&ctx)).is_err());

        // Untouched: restores.
        assert!(restore_from_snapshot(&path, target.storage(), target.index(), Some(&ctx)).is_ok());
    }

    #[test]
    fn test_list_snapshots_rejects_junk_names_and_orders_newest_first() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("snapshots");
        fs::create_dir_all(&dir).unwrap();
        for name in [
            "snapshot-2026-03-20.json.zst.enc",
            "snapshot-2026-03-23.json.zst.enc",
            "snapshot-zzzz.json.zst.enc",
            "snapshot-9999-99-99.json.zst.enc",
            "snapshot-2026-03-25.json.zst",
        ] {
            fs::write(dir.join(name), b"x").unwrap();
        }
        let names: Vec<String> = list_snapshots(&dir, true)
            .unwrap()
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().to_string())
            .collect();
        assert_eq!(names, vec!["snapshot-2026-03-23.json.zst.enc", "snapshot-2026-03-20.json.zst.enc"]);
    }

    #[test]
    fn test_snapshot_rejects_envelope_older_than_active_version() {
        let cortex = public_cortex();
        let base = crypto::new_encryption_manifest();
        let at = |v: u32| {
            let mut m = base.clone();
            m.key_version = Some(v);
            crypto::derive_key("snapshot-test-pass", &m).unwrap()
        };
        let (ctx1, ctx2) = (at(1), at(2));
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("snapshots");
        let path = create_snapshot(cortex.storage(), &dir, Some(&ctx1)).unwrap();
        assert!(fs::read_to_string(&path).unwrap().starts_with("ENC2:"));

        // Same HMAC key (version-0 derived), but the envelope is at a retired version.
        let target = crate::Cortex::in_memory().unwrap();
        let err = restore_from_snapshot(&path, target.storage(), target.index(), Some(&ctx2)).unwrap_err();
        assert!(err.to_string().contains("rotated-out"), "{err}");
        assert!(restore_from_snapshot(&path, target.storage(), target.index(), Some(&ctx1)).is_ok());
    }

    #[test]
    fn test_restore_enforces_size_limits() {
        let cortex = public_cortex();
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("snapshots");
        let path = create_snapshot(cortex.storage(), &dir, None).unwrap();
        let file_len = fs::metadata(&path).unwrap().len();
        let target = crate::Cortex::in_memory().unwrap();
        let restore = |limits: SnapshotLimits| {
            restore_with_limits(&path, target.storage(), target.index(), None, None, limits)
        };

        let err = restore(SnapshotLimits { max_file_bytes: file_len - 1, max_decompressed_bytes: u64::MAX })
            .unwrap_err();
        assert!(err.to_string().contains("too large"), "{err}");
        let err = restore(SnapshotLimits { max_file_bytes: u64::MAX, max_decompressed_bytes: 16 }).unwrap_err();
        assert!(err.to_string().contains("decompressed size"), "{err}");
        assert!(restore(SnapshotLimits::DEFAULT).is_ok());
    }

    #[test]
    fn test_restore_requires_expected_mac_when_given() {
        let cortex = public_cortex();
        let ctx = crypto::derive_key("snapshot-test-pass", &crypto::new_encryption_manifest()).unwrap();
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("snapshots");
        let (path, mac) = create_snapshot_with_mac(cortex.storage(), &dir, Some(&ctx)).unwrap();
        let mac = mac.expect("encrypted snapshot has a mac");
        let target = crate::Cortex::in_memory().unwrap();
        let go = |expected: &str| {
            restore_with_limits(&path, target.storage(), target.index(), Some(&ctx), Some(expected), SnapshotLimits::DEFAULT)
        };
        assert!(go("AAAA").is_err());
        assert!(go(&mac).is_ok());
    }
}
