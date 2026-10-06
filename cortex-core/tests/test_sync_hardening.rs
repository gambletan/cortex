//! Sync hardening regressions (2026-10-06 review): snapshot freshness pinned by the manifest,
//! and no plaintext participation in an encrypted sync group.

use cortex_core::sync::oplog::SyncPayload;
use cortex_core::sync::{SyncConfig, SyncEngine};
use cortex_core::types::*;
use cortex_core::Cortex;
use tempfile::TempDir;

fn config(sync_dir: &std::path::Path, device: &str, pass: Option<&str>) -> SyncConfig {
    let c = SyncConfig::new(sync_dir.to_path_buf(), device.into(), device.into());
    match pass {
        Some(p) => c.with_encryption(p),
        None => c,
    }
}

fn public_mem(text: &str) -> MemObject {
    MemObjectBuilder::new(MemoryTier::Episodic, MemContent::Text(text.into()), MemSource::new("t"))
        .privacy(PrivacyLevel::Public)
        .build()
}

fn manifest_json(sync_dir: &std::path::Path) -> serde_json::Value {
    serde_json::from_str(&std::fs::read_to_string(sync_dir.join("manifest.json")).unwrap()).unwrap()
}

#[test]
fn bootstrap_ignores_a_planted_newer_snapshot_because_the_manifest_pins_the_real_one() {
    let tmp = TempDir::new().unwrap();
    let sync_dir = tmp.path().join("sync");

    let a = Cortex::in_memory().unwrap();
    a.storage().store_memory(&public_mem("zebrafalcon fact")).unwrap();
    let mut engine_a = SyncEngine::new(config(&sync_dir, "a", Some("pass-123")), a.sqlite_storage()).unwrap();
    engine_a.create_snapshot(a.sqlite_storage()).unwrap();

    // Attacker (no key) drops a valid-looking, newer-dated junk snapshot.
    std::fs::write(sync_dir.join("snapshots/snapshot-2099-01-01.json.zst.enc"), "ENC1:garbage\nHMAC:AAAA\n").unwrap();

    let b = Cortex::in_memory().unwrap();
    let mut engine_b = SyncEngine::new(config(&sync_dir, "b", Some("pass-123")), b.sqlite_storage()).unwrap();
    let report = engine_b.restore_from_snapshot(b.sqlite_storage(), b.index()).unwrap().expect("restored");
    assert_eq!(report.memories, 1);
}

#[test]
fn create_snapshot_pins_it_in_the_manifest_and_rotation_keeps_the_pin() {
    let tmp = TempDir::new().unwrap();
    let sync_dir = tmp.path().join("sync");
    let a = Cortex::in_memory().unwrap();
    a.storage().store_memory(&public_mem("pinned")).unwrap();
    let mut engine_a = SyncEngine::new(config(&sync_dir, "a", Some("pass-123")), a.sqlite_storage()).unwrap();
    let path = engine_a.create_snapshot(a.sqlite_storage()).unwrap();

    let raw = std::fs::read_to_string(&path).unwrap();
    let mac = raw.lines().nth(1).unwrap().strip_prefix("HMAC:").unwrap().to_string();
    let ptr = manifest_json(&sync_dir)["encryption"]["latest_snapshot"].clone();
    assert_eq!(ptr["file"], path.file_name().unwrap().to_string_lossy().as_ref());
    assert_eq!(ptr["mac"], mac.as_str());

    engine_a.rotate_key(a.sqlite_storage()).unwrap();
    assert_eq!(manifest_json(&sync_dir)["encryption"]["latest_snapshot"], ptr, "rotation must keep the pointer");

    // The manifest (now carrying a pointer) still verifies for a fresh device.
    let b = Cortex::in_memory().unwrap();
    SyncEngine::new(config(&sync_dir, "b", Some("pass-123")), b.sqlite_storage()).unwrap();
}

#[test]
fn replaying_an_older_snapshot_under_the_same_name_is_rejected() {
    let tmp = TempDir::new().unwrap();
    let sync_dir = tmp.path().join("sync");
    let a = Cortex::in_memory().unwrap();
    let doomed = public_mem("deleted-later secret");
    a.storage().store_memory(&doomed).unwrap();
    let mut engine_a = SyncEngine::new(config(&sync_dir, "a", Some("pass-123")), a.sqlite_storage()).unwrap();
    let path = engine_a.create_snapshot(a.sqlite_storage()).unwrap();
    let old_bytes = std::fs::read(&path).unwrap();

    // Later the same day: the memory is deleted and a new snapshot is pinned.
    a.storage().delete_memory(doomed.id).unwrap();
    let path2 = engine_a.create_snapshot(a.sqlite_storage()).unwrap();
    assert_ne!(path, path2, "snapshot files are immutable and uniquely named");

    // Attacker replays the older (validly HMAC'd) snapshot under the pinned name.
    std::fs::write(&path2, &old_bytes).unwrap();

    let b = Cortex::in_memory().unwrap();
    let mut engine_b = SyncEngine::new(config(&sync_dir, "b", Some("pass-123")), b.sqlite_storage()).unwrap();
    assert!(engine_b.restore_from_snapshot(b.sqlite_storage(), b.index()).is_err());
    assert_eq!(b.stats().unwrap().total, 0, "deleted data must not resurrect");
}

#[test]
fn corrupted_pointed_snapshot_fails_closed() {
    let tmp = TempDir::new().unwrap();
    let sync_dir = tmp.path().join("sync");
    let a = Cortex::in_memory().unwrap();
    a.storage().store_memory(&public_mem("x")).unwrap();
    let mut engine_a = SyncEngine::new(config(&sync_dir, "a", Some("pass-123")), a.sqlite_storage()).unwrap();
    let path = engine_a.create_snapshot(a.sqlite_storage()).unwrap();
    std::fs::write(&path, "ENC1:garbage\nHMAC:AAAA\n").unwrap();

    let b = Cortex::in_memory().unwrap();
    let mut engine_b = SyncEngine::new(config(&sync_dir, "b", Some("pass-123")), b.sqlite_storage()).unwrap();
    assert!(engine_b.restore_from_snapshot(b.sqlite_storage(), b.index()).is_err());

    // Deleting it is also an error, not "no snapshot".
    std::fs::remove_file(&path).unwrap();
    assert!(engine_b.restore_from_snapshot(b.sqlite_storage(), b.index()).is_err());
}

#[test]
fn joining_an_encrypted_group_without_a_passphrase_is_refused() {
    let tmp = TempDir::new().unwrap();
    let sync_dir = tmp.path().join("sync");
    let a = Cortex::in_memory().unwrap();
    SyncEngine::new(config(&sync_dir, "a", Some("pass-123")), a.sqlite_storage()).unwrap();

    let b = Cortex::in_memory().unwrap();
    assert!(SyncEngine::new(config(&sync_dir, "b", None), b.sqlite_storage()).is_err());
}

#[test]
fn unreadable_manifest_refuses_plaintext_start() {
    let tmp = TempDir::new().unwrap();
    let sync_dir = tmp.path().join("sync");
    std::fs::create_dir_all(&sync_dir).unwrap();
    std::fs::write(sync_dir.join("manifest.json"), "{ not json").unwrap();

    let b = Cortex::in_memory().unwrap();
    assert!(SyncEngine::new(config(&sync_dir, "b", None), b.sqlite_storage()).is_err());
}

#[test]
fn deleted_manifest_with_encrypted_peer_oplog_refuses_plaintext_start() {
    let tmp = TempDir::new().unwrap();
    let sync_dir = tmp.path().join("sync");
    let a = Cortex::in_memory().unwrap();
    let mut engine_a = SyncEngine::new(config(&sync_dir, "a", Some("pass-123")), a.sqlite_storage()).unwrap();
    engine_a.record_op(SyncPayload::MemoryUpsert { memory: public_mem("enc") }).unwrap();
    drop(engine_a);

    // Attacker deletes the manifest to make the folder look unencrypted.
    std::fs::remove_file(sync_dir.join("manifest.json")).unwrap();

    let b = Cortex::in_memory().unwrap();
    assert!(SyncEngine::new(config(&sync_dir, "b", None), b.sqlite_storage()).is_err());
    assert!(
        std::fs::read_dir(sync_dir.join("devices")).unwrap().all(|e| e.unwrap().file_name() != "b"),
        "a refused device must not leave artifacts in the shared folder"
    );
}

#[test]
fn plaintext_group_still_starts_without_a_passphrase() {
    let tmp = TempDir::new().unwrap();
    let sync_dir = tmp.path().join("sync");
    let a = Cortex::in_memory().unwrap();
    let mut engine_a = SyncEngine::new(config(&sync_dir, "a", None), a.sqlite_storage()).unwrap();
    engine_a.record_op(SyncPayload::MemoryUpsert { memory: public_mem("plain") }).unwrap();

    let b = Cortex::in_memory().unwrap();
    SyncEngine::new(config(&sync_dir, "b", None), b.sqlite_storage()).unwrap();
}

fn snapshot_path(sync_dir: &std::path::Path) -> std::path::PathBuf {
    let name = manifest_json(sync_dir)["encryption"]["latest_snapshot"]["file"].as_str().unwrap().to_string();
    sync_dir.join("snapshots").join(name)
}

#[test]
fn every_manifest_write_bumps_the_generation() {
    let tmp = TempDir::new().unwrap();
    let sync_dir = tmp.path().join("sync");
    let a = Cortex::in_memory().unwrap();
    let mut engine_a = SyncEngine::new(config(&sync_dir, "a", Some("pass-123")), a.sqlite_storage()).unwrap();
    let generation = || manifest_json(&sync_dir)["encryption"]["generation"].as_u64();
    assert_eq!(generation(), Some(1), "first creation is generation 1");
    engine_a.create_snapshot(a.sqlite_storage()).unwrap();
    assert_eq!(generation(), Some(2));
    engine_a.rotate_key(a.sqlite_storage()).unwrap();
    assert_eq!(generation(), Some(3));
}

#[test]
fn a_device_that_saw_a_newer_manifest_rejects_a_replayed_older_one() {
    let tmp = TempDir::new().unwrap();
    let sync_dir = tmp.path().join("sync");
    let a = Cortex::in_memory().unwrap();
    let doomed = public_mem("deleted-later secret");
    a.storage().store_memory(&doomed).unwrap();
    let mut engine_a = SyncEngine::new(config(&sync_dir, "a", Some("pass-123")), a.sqlite_storage()).unwrap();
    engine_a.create_snapshot(a.sqlite_storage()).unwrap();
    // Attacker records generation N-1: the manifest and the snapshot it pins.
    let old_manifest = std::fs::read(sync_dir.join("manifest.json")).unwrap();
    let old_snapshot = std::fs::read(snapshot_path(&sync_dir)).unwrap();

    // Generation N: the memory is deleted and re-snapshotted.
    a.storage().delete_memory(doomed.id).unwrap();
    engine_a.create_snapshot(a.sqlite_storage()).unwrap();

    // Device b syncs and sees generation N.
    let b = Cortex::in_memory().unwrap();
    let mut engine_b = SyncEngine::new(config(&sync_dir, "b", Some("pass-123")), b.sqlite_storage()).unwrap();
    assert_eq!(engine_b.restore_from_snapshot(b.sqlite_storage(), b.index()).unwrap().unwrap().memories, 0);

    // Whole-manifest rollback: both files validly signed, just old.
    std::fs::write(sync_dir.join("manifest.json"), &old_manifest).unwrap();
    std::fs::write(snapshot_path(&sync_dir), &old_snapshot).unwrap();

    let err = engine_b.restore_from_snapshot(b.sqlite_storage(), b.index()).unwrap_err();
    assert!(err.to_string().contains("rollback"), "{err}");
    assert_eq!(b.stats().unwrap().total, 0, "deleted data must not resurrect");

    // The high-water mark is persisted locally: a restarted engine also refuses.
    drop(engine_b);
    let err = SyncEngine::new(config(&sync_dir, "b", Some("pass-123")), b.sqlite_storage()).err().expect("rollback refused");
    assert!(err.to_string().contains("rollback"), "{err}");

    // Documented residual: a brand-new device has no anchor and accepts it.
    let c = Cortex::in_memory().unwrap();
    let mut engine_c = SyncEngine::new(config(&sync_dir, "c", Some("pass-123")), c.sqlite_storage()).unwrap();
    assert_eq!(engine_c.restore_from_snapshot(c.sqlite_storage(), c.index()).unwrap().unwrap().memories, 1);
}

#[test]
fn legacy_manifest_without_generation_still_loads_until_a_newer_one_is_seen() {
    use cortex_core::sync::crypto;
    let tmp = TempDir::new().unwrap();
    let sync_dir = tmp.path().join("sync");
    std::fs::create_dir_all(&sync_dir).unwrap();

    // A manifest written before `generation` existed (validly signed).
    let mut enc = crypto::new_encryption_manifest();
    let (hmac, salt) = crypto::compute_manifest_hmac(&serde_json::to_vec(&enc).unwrap(), "pass-123").unwrap();
    enc.hmac = Some(hmac);
    enc.hmac_salt = Some(salt);
    let legacy = serde_json::json!({ "version": "cortex-sync-v1", "encryption": enc });
    assert!(legacy["encryption"].get("generation").is_none());
    let legacy_text = serde_json::to_string_pretty(&legacy).unwrap();
    std::fs::write(sync_dir.join("manifest.json"), &legacy_text).unwrap();

    let a = Cortex::in_memory().unwrap();
    let mut engine_a = SyncEngine::new(config(&sync_dir, "a", Some("pass-123")), a.sqlite_storage()).unwrap();
    engine_a.create_snapshot(a.sqlite_storage()).unwrap();
    assert_eq!(manifest_json(&sync_dir)["encryption"]["generation"].as_u64(), Some(1));

    // Once a generation has been seen, replaying the legacy (generation-less) manifest is a rollback.
    std::fs::write(sync_dir.join("manifest.json"), &legacy_text).unwrap();
    assert!(SyncEngine::new(config(&sync_dir, "a", Some("pass-123")), a.sqlite_storage()).is_err());
}

#[test]
fn rotation_persists_the_generation_immediately() {
    let tmp = TempDir::new().unwrap();
    let sync_dir = tmp.path().join("sync");
    let a = Cortex::in_memory().unwrap();
    let mut engine_a = SyncEngine::new(config(&sync_dir, "a", Some("pass-123")), a.sqlite_storage()).unwrap();
    let before = std::fs::read(sync_dir.join("manifest.json")).unwrap();
    engine_a.rotate_key(a.sqlite_storage()).unwrap();
    drop(engine_a); // "crash": no pull/snapshot after rotating

    std::fs::write(sync_dir.join("manifest.json"), before).unwrap(); // replay pre-rotation manifest
    let err = SyncEngine::new(config(&sync_dir, "a", Some("pass-123")), a.sqlite_storage()).err();
    assert!(err.is_some_and(|e| e.to_string().contains("rollback")));
}

#[test]
fn two_same_day_snapshots_both_exist_and_the_pointer_names_the_second() {
    let tmp = TempDir::new().unwrap();
    let sync_dir = tmp.path().join("sync");
    let a = Cortex::in_memory().unwrap();
    a.storage().store_memory(&public_mem("one")).unwrap();
    let mut engine_a = SyncEngine::new(config(&sync_dir, "a", Some("pass-123")), a.sqlite_storage()).unwrap();
    let first = engine_a.create_snapshot(a.sqlite_storage()).unwrap();
    a.storage().store_memory(&public_mem("two")).unwrap();
    let second = engine_a.create_snapshot(a.sqlite_storage()).unwrap();

    assert_ne!(first, second);
    assert!(first.exists() && second.exists());
    assert_eq!(snapshot_path(&sync_dir), second);

    let b = Cortex::in_memory().unwrap();
    let mut engine_b = SyncEngine::new(config(&sync_dir, "b", Some("pass-123")), b.sqlite_storage()).unwrap();
    assert_eq!(engine_b.restore_from_snapshot(b.sqlite_storage(), b.index()).unwrap().unwrap().memories, 2);
}

#[test]
fn crash_between_snapshot_write_and_pointer_update_keeps_the_old_pin_restorable() {
    use cortex_core::sync::{crypto, snapshot};
    let tmp = TempDir::new().unwrap();
    let sync_dir = tmp.path().join("sync");
    let a = Cortex::in_memory().unwrap();
    a.storage().store_memory(&public_mem("pinned")).unwrap();
    let mut engine_a = SyncEngine::new(config(&sync_dir, "a", Some("pass-123")), a.sqlite_storage()).unwrap();
    let pinned = engine_a.create_snapshot(a.sqlite_storage()).unwrap();

    // Simulate the crash: a new snapshot file is fully written, but the pointer is never
    // published (this is exactly the first half of `SyncEngine::create_snapshot`).
    let enc: crypto::EncryptionManifest =
        serde_json::from_value(manifest_json(&sync_dir)["encryption"].clone()).unwrap();
    let ctx = crypto::derive_key("pass-123", &enc).unwrap();
    a.storage().store_memory(&public_mem("unpublished")).unwrap();
    let orphan = snapshot::create_snapshot(a.storage(), &sync_dir.join("snapshots"), Some(&ctx)).unwrap();

    assert_ne!(orphan, pinned);
    assert!(pinned.exists(), "the pinned snapshot must never be overwritten");
    assert_eq!(snapshot_path(&sync_dir), pinned);
    let b = Cortex::in_memory().unwrap();
    let mut engine_b = SyncEngine::new(config(&sync_dir, "b", Some("pass-123")), b.sqlite_storage()).unwrap();
    assert_eq!(engine_b.restore_from_snapshot(b.sqlite_storage(), b.index()).unwrap().unwrap().memories, 1);
}

#[test]
fn snapshot_by_a_device_with_a_stale_key_after_another_rotated_is_still_restorable() {
    let tmp = TempDir::new().unwrap();
    let sync_dir = tmp.path().join("sync");
    let a = Cortex::in_memory().unwrap();
    let mut engine_a = SyncEngine::new(config(&sync_dir, "a", Some("pass-123")), a.sqlite_storage()).unwrap();
    let b = Cortex::in_memory().unwrap();
    b.storage().store_memory(&public_mem("from b")).unwrap();
    let mut engine_b = SyncEngine::new(config(&sync_dir, "b", Some("pass-123")), b.sqlite_storage()).unwrap();

    // A rotates while B keeps running with its version-0 context.
    engine_a.rotate_key(a.sqlite_storage()).unwrap();
    let path = engine_b.create_snapshot(b.sqlite_storage()).unwrap();
    assert!(
        std::fs::read_to_string(&path).unwrap().starts_with("ENC2:"),
        "B must encrypt under the current (rotated) key version"
    );

    let c = Cortex::in_memory().unwrap();
    let mut engine_c = SyncEngine::new(config(&sync_dir, "c", Some("pass-123")), c.sqlite_storage()).unwrap();
    assert_eq!(engine_c.restore_from_snapshot(c.sqlite_storage(), c.index()).unwrap().unwrap().memories, 1);
}

#[test]
fn old_unpinned_snapshots_are_pruned_but_the_pinned_one_is_kept() {
    let tmp = TempDir::new().unwrap();
    let sync_dir = tmp.path().join("sync");
    let a = Cortex::in_memory().unwrap();
    a.storage().store_memory(&public_mem("x")).unwrap();
    let mut engine_a = SyncEngine::new(config(&sync_dir, "a", Some("pass-123")), a.sqlite_storage()).unwrap();
    let mut last = None;
    for _ in 0..6 {
        last = Some(engine_a.create_snapshot(a.sqlite_storage()).unwrap());
    }
    let count = std::fs::read_dir(sync_dir.join("snapshots")).unwrap().count();
    assert!(count <= 3, "old snapshots pruned, found {count}");
    assert!(last.as_ref().unwrap().exists());
    assert_eq!(snapshot_path(&sync_dir), last.unwrap());
}

#[test]
fn a_running_device_follows_another_devices_key_rotation_on_pull() {
    use cortex_core::sync::oplog::SyncPayload;
    let tmp = TempDir::new().unwrap();
    let sync_dir = tmp.path().join("sync");
    let a = Cortex::in_memory().unwrap();
    let b = Cortex::in_memory().unwrap();
    let mut engine_a = SyncEngine::new(config(&sync_dir, "a", Some("pass-123")), a.sqlite_storage()).unwrap();
    let mut engine_b = SyncEngine::new(config(&sync_dir, "b", Some("pass-123")), b.sqlite_storage()).unwrap();

    engine_a.rotate_key(a.sqlite_storage()).unwrap();
    engine_b.pull_remote(b.sqlite_storage(), b.index()).unwrap();

    let mem = MemObjectBuilder::new(MemoryTier::Episodic, MemContent::Text("after rotation".into()), MemSource::new("t"))
        .privacy(PrivacyLevel::Public)
        .build();
    engine_b.record_op(SyncPayload::MemoryUpsert { memory: mem }).unwrap();

    let dir_b = sync_dir.join("devices/b");
    let files = cortex_core::sync::oplog::list_oplog_files(&dir_b).unwrap();
    let last = files.iter().flat_map(|f| std::fs::read_to_string(f).unwrap().lines().map(String::from).collect::<Vec<_>>()).last().unwrap();
    assert!(last.starts_with("ENC2:"), "B must write under the rotated key: {}", &last[..8]);
}

#[test]
fn pinned_snapshot_stays_restorable_after_key_rotation() {
    let tmp = TempDir::new().unwrap();
    let sync_dir = tmp.path().join("sync");
    let a = Cortex::in_memory().unwrap();
    let mem = MemObjectBuilder::new(MemoryTier::Episodic, MemContent::Text("pre-rotation fact".into()), MemSource::new("t"))
        .privacy(PrivacyLevel::Public)
        .build();
    a.storage().store_memory(&mem).unwrap();
    let mut engine_a = SyncEngine::new(config(&sync_dir, "a", Some("pass-123")), a.sqlite_storage()).unwrap();
    engine_a.create_snapshot(a.sqlite_storage()).unwrap();
    engine_a.rotate_key(a.sqlite_storage()).unwrap();

    let c = Cortex::in_memory().unwrap();
    let mut engine_c = SyncEngine::new(config(&sync_dir, "c", Some("pass-123")), c.sqlite_storage()).unwrap();
    let report = engine_c.restore_from_snapshot(c.sqlite_storage(), c.index()).unwrap().expect("restored");
    assert_eq!(report.memories, 1);
}

fn last_oplog_line(sync_dir: &std::path::Path, device: &str) -> String {
    let files = cortex_core::sync::oplog::list_oplog_files(&sync_dir.join("devices").join(device)).unwrap();
    let text = std::fs::read_to_string(files.last().expect("an oplog file")).unwrap();
    text.lines().rev().find(|l| !l.trim().is_empty()).expect("an oplog line").to_string()
}

/// Codex round 4, P1: A rotates (gen N+1, v1) while B, which hasn't seen it, snapshots
/// (gen N+1, v0) and B's manifest lands last. A must not adopt the lower key version on
/// restart; it repairs the manifest back to v1 and keeps writing ENC2 at v1.
#[test]
fn concurrent_writer_cannot_roll_back_the_key_version() {
    use cortex_core::sync::crypto;
    let tmp = TempDir::new().unwrap();
    let sync_dir = tmp.path().join("sync");
    let manifest_path = sync_dir.join("manifest.json");
    let a = Cortex::in_memory().unwrap();
    let mut engine_a = SyncEngine::new(config(&sync_dir, "a", Some("pass-123")), a.sqlite_storage()).unwrap();
    let b = Cortex::in_memory().unwrap();
    b.storage().store_memory(&public_mem("from b")).unwrap();
    let mut engine_b = SyncEngine::new(config(&sync_dir, "b", Some("pass-123")), b.sqlite_storage()).unwrap();
    let before = std::fs::read(&manifest_path).unwrap(); // generation N, v0

    // A rotates: generation N+1, v1.
    engine_a.rotate_key(a.sqlite_storage()).unwrap();
    // B never saw that write (cloud sync lag): it snapshots from generation N and its
    // manifest (generation N+1, v0) lands last.
    std::fs::write(&manifest_path, &before).unwrap();
    engine_b.create_snapshot(b.sqlite_storage()).unwrap();
    assert_eq!(manifest_json(&sync_dir)["encryption"]["key_version"], 0);

    // A restarts and must not adopt v0.
    drop(engine_a);
    let mut engine_a = SyncEngine::new(config(&sync_dir, "a", Some("pass-123")), a.sqlite_storage()).unwrap();
    assert_eq!(manifest_json(&sync_dir)["encryption"]["key_version"], 1, "manifest repaired to v1");
    engine_a.record_op(SyncPayload::MemoryUpsert { memory: public_mem("after restart") }).unwrap();
    assert_eq!(crypto::envelope_version(&last_oplog_line(&sync_dir, "a")), Some(1));

    // The repaired manifest kept B's pointer, and B's snapshot is still restorable.
    let c = Cortex::in_memory().unwrap();
    let mut engine_c = SyncEngine::new(config(&sync_dir, "c", Some("pass-123")), c.sqlite_storage()).unwrap();
    assert_eq!(engine_c.restore_from_snapshot(c.sqlite_storage(), c.index()).unwrap().unwrap().memories, 1);

    // B, still running, also follows the repaired manifest instead of reverting it.
    engine_b.pull_remote(b.sqlite_storage(), b.index()).unwrap();
    engine_b.record_op(SyncPayload::MemoryUpsert { memory: public_mem("b later") }).unwrap();
    assert_eq!(crypto::envelope_version(&last_oplog_line(&sync_dir, "b")), Some(1));
}

/// Codex round 4, P2: B's context predates A's rotation; restoring A's pinned v1 snapshot
/// must adopt the manifest's key version instead of rejecting it.
#[test]
fn restore_after_another_device_rotated_and_snapshotted_succeeds() {
    let tmp = TempDir::new().unwrap();
    let sync_dir = tmp.path().join("sync");
    let a = Cortex::in_memory().unwrap();
    a.storage().store_memory(&public_mem("from a")).unwrap();
    let mut engine_a = SyncEngine::new(config(&sync_dir, "a", Some("pass-123")), a.sqlite_storage()).unwrap();
    let b = Cortex::in_memory().unwrap();
    let mut engine_b = SyncEngine::new(config(&sync_dir, "b", Some("pass-123")), b.sqlite_storage()).unwrap();

    engine_a.rotate_key(a.sqlite_storage()).unwrap();
    engine_a.create_snapshot(a.sqlite_storage()).unwrap();

    assert_eq!(engine_b.restore_from_snapshot(b.sqlite_storage(), b.index()).unwrap().unwrap().memories, 1);
}
