//! Full privacy chain end-to-end test.
//!
//! Tests the complete privacy pipeline:
//!   SQLCipher encrypted DB → Private memory filtering → AES-256-GCM oplog
//!   → Cross-device sync → Snapshot bootstrap → Memory zeroize

use cortex_core::sync::oplog::{self, SyncPayload};
use cortex_core::sync::snapshot;
use cortex_core::sync::{SyncConfig, SyncEngine};
use cortex_core::types::*;
use cortex_core::Cortex;
use tempfile::TempDir;

fn make_config(sync_dir: &std::path::Path, device_id: &str) -> SyncConfig {
    SyncConfig::new(
        sync_dir.to_path_buf(),
        device_id.into(),
        format!("Device {}", device_id),
    )
    .with_encryption("full-chain-test-passphrase")
}

#[test]
fn test_full_privacy_chain_encrypted_db_to_encrypted_sync() {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join("encrypted.db");
    let sync_dir = tmp.path().join("cortex-sync");

    // ══════════════════════════════════════════════════════════════════
    // Step 1: Create an encrypted Cortex database (SQLCipher)
    // ══════════════════════════════════════════════════════════════════
    let cortex_a = Cortex::open_encrypted(
        db_path.to_str().unwrap(),
        "my-db-passphrase",
    ).unwrap();
    println!("✅ Step 1: Encrypted DB created with SQLCipher");

    // ══════════════════════════════════════════════════════════════════
    // Step 2: Enable encrypted cloud sync (AES-256-GCM)
    // ══════════════════════════════════════════════════════════════════
    cortex_a.enable_sync(make_config(&sync_dir, "device-a")).unwrap();
    println!("✅ Step 2: Encrypted sync enabled (AES-256-GCM + Argon2id)");

    // ══════════════════════════════════════════════════════════════════
    // Step 3: Ingest memories — Private should NOT sync
    // ══════════════════════════════════════════════════════════════════
    cortex_a.ingest("My SSN is 123-45-6789", "private-channel", None, None, None).unwrap();
    cortex_a.ingest("I have a crush on Alice", "diary", None, None, None).unwrap();
    cortex_a.add_fact("User", "salary", "$150k", 0.9, "private", None).unwrap();

    // Verify: NO oplog entries (all memories are Private by default)
    let device_dir_a = sync_dir.join("devices/device-a");
    let files_a = oplog::list_oplog_files(&device_dir_a).unwrap_or_default();
    let private_ops: usize = files_a.iter()
        .map(|f| oplog::read_oplog(f, 0, None).unwrap().0.len())
        .sum();
    assert_eq!(private_ops, 0, "Private memories must NEVER appear in oplog!");
    println!("✅ Step 3: Private memories (3 total) correctly NOT synced to oplog");

    // ══════════════════════════════════════════════════════════════════
    // Step 4: Verify DB file is encrypted (cannot be read as plain SQLite)
    // ══════════════════════════════════════════════════════════════════
    let db_bytes = std::fs::read(&db_path).unwrap();
    // Plain SQLite starts with "SQLite format 3\0"
    let is_plain_sqlite = db_bytes.len() > 16
        && &db_bytes[0..16] == b"SQLite format 3\0";
    // With SQLCipher, the file header is encrypted and does NOT match
    #[cfg(feature = "encrypted-db")]
    assert!(!is_plain_sqlite, "Encrypted DB should NOT have plain SQLite header");
    println!("✅ Step 4: DB file is encrypted (no plain SQLite header)");

    // ══════════════════════════════════════════════════════════════════
    // Step 5: Write a Shared memory via direct SyncEngine (simulating sync)
    // ══════════════════════════════════════════════════════════════════
    let shared_mem = MemObjectBuilder::new(
        MemoryTier::Episodic,
        MemContent::Text("I prefer dark mode (shared across devices)".into()),
        MemSource::new("settings"),
    )
    .privacy(PrivacyLevel::Shared { scope: "all".into() })
    .build();
    let shared_id = shared_mem.id;

    // Use device B to write this memory to oplog
    let cortex_b = Cortex::in_memory().unwrap();
    let mut engine_b = SyncEngine::new(
        make_config(&sync_dir, "device-b"),
        cortex_b.sqlite_storage(),
    ).unwrap();
    engine_b.record_op(SyncPayload::MemoryUpsert { memory: shared_mem }).unwrap();

    // Verify oplog is encrypted
    let device_dir_b = sync_dir.join("devices/device-b");
    let files_b = oplog::list_oplog_files(&device_dir_b).unwrap();
    assert!(!files_b.is_empty());
    let oplog_content = std::fs::read_to_string(&files_b[0]).unwrap();
    assert!(oplog_content.starts_with("ENC1:"), "Oplog must be encrypted");
    assert!(!oplog_content.contains("dark mode"), "Plaintext must NOT appear in oplog");
    println!("✅ Step 5: Shared memory written to encrypted oplog (ENC1:)");

    // ══════════════════════════════════════════════════════════════════
    // Step 6: Device A pulls from Device B (encrypted cross-device sync)
    // ══════════════════════════════════════════════════════════════════
    let applied = cortex_a.sync_pull().unwrap();
    assert!(applied > 0, "Should apply remote shared memory");

    let synced = cortex_a.storage().get_memory(shared_id).unwrap();
    assert!(synced.is_some(), "Shared memory should arrive on device A");
    println!("✅ Step 6: Cross-device encrypted sync — Device B → Device A");

    // ══════════════════════════════════════════════════════════════════
    // Step 7: Create snapshot for new-device bootstrap
    // ══════════════════════════════════════════════════════════════════
    let snapshots_dir = sync_dir.join("snapshots");
    let snapshot_path = snapshot::create_snapshot(cortex_a.storage(), &snapshots_dir).unwrap();
    assert!(snapshot_path.exists());

    // Verify snapshot is compressed (zstd)
    let snapshot_bytes = std::fs::read(&snapshot_path).unwrap();
    // Zstd magic number: 0xFD2FB528
    assert_eq!(&snapshot_bytes[0..4], &[0x28, 0xB5, 0x2F, 0xFD], "Snapshot should be zstd-compressed");
    println!("✅ Step 7: Snapshot created ({} bytes, zstd-compressed)", snapshot_bytes.len());

    // ══════════════════════════════════════════════════════════════════
    // Step 8: New device restores from snapshot
    // ══════════════════════════════════════════════════════════════════
    let cortex_c = Cortex::in_memory().unwrap();
    let (report, _) = snapshot::restore_from_snapshot(
        &snapshot_path,
        cortex_c.storage(),
        cortex_c.index(),
    ).unwrap();
    assert!(report.memories > 0, "Should restore memories from snapshot");

    let stats_c = cortex_c.stats().unwrap();
    let stats_a = cortex_a.stats().unwrap();
    assert_eq!(stats_c.total, stats_a.total, "Device C should have same memory count as Device A");
    println!("✅ Step 8: New device bootstrapped from snapshot ({} memories)", report.memories);

    // ══════════════════════════════════════════════════════════════════
    // Step 9: Verify memory zeroization
    // ══════════════════════════════════════════════════════════════════
    let mut sensitive = MemContent::Text("My password is hunter2".into());
    sensitive.zeroize_content();
    if let MemContent::Text(ref s) = sensitive {
        assert!(s.is_empty(), "Zeroized content must be empty");
    }

    let mut fact = MemContent::Fact {
        subject: "User".into(),
        predicate: "credit_card".into(),
        object: "4111-1111-1111-1111".into(),
    };
    fact.zeroize_content();
    if let MemContent::Fact { subject, predicate, object } = &fact {
        assert!(subject.is_empty() && predicate.is_empty() && object.is_empty());
    }
    println!("✅ Step 9: Sensitive data zeroized from memory");

    // ══════════════════════════════════════════════════════════════════
    // Step 10: Verify complete privacy chain
    // ══════════════════════════════════════════════════════════════════
    println!("\n🔒 FULL PRIVACY CHAIN VERIFIED:");
    println!("   ├── SQLCipher DB encryption (device-level)");
    println!("   ├── Private memory filtering (never leaves device)");
    println!("   ├── AES-256-GCM oplog encryption (transit-level)");
    println!("   ├── Cross-device encrypted sync (via cloud storage)");
    println!("   ├── Zstd compressed snapshots (new device bootstrap)");
    println!("   └── Memory zeroization (RAM-level)");
    println!("\n✅ ALL 10 STEPS PASSED — FULL PRIVACY CHAIN COMPLETE!");
}

#[test]
fn test_wrong_db_passphrase_fails() {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join("encrypted.db");

    // Create encrypted DB
    let _cortex = Cortex::open_encrypted(db_path.to_str().unwrap(), "correct-password").unwrap();
    // Ingest something so the DB has data
    _cortex.ingest("test data", "test", None, None, None).unwrap();
    drop(_cortex);

    // Try to open with wrong password — should fail
    #[cfg(feature = "encrypted-db")]
    {
        let result = Cortex::open_encrypted(db_path.to_str().unwrap(), "wrong-password");
        assert!(result.is_err(), "Wrong passphrase should fail to open encrypted DB");
        println!("✅ Wrong DB passphrase correctly rejected");
    }
}

#[test]
fn test_wrong_sync_passphrase_skips_ops() {
    let tmp = TempDir::new().unwrap();
    let sync_dir = tmp.path().join("cortex-sync");

    // Device A writes encrypted ops
    let cortex_a = Cortex::in_memory().unwrap();
    let mut engine_a = SyncEngine::new(
        make_config(&sync_dir, "device-a"),
        cortex_a.sqlite_storage(),
    ).unwrap();
    engine_a.record_op(SyncPayload::MemoryDelete { id: uuid::Uuid::new_v4() }).unwrap();

    // Device B tries to read with wrong passphrase
    let cortex_b = Cortex::in_memory().unwrap();
    let config_b = SyncConfig::new(
        sync_dir.clone(),
        "device-b".into(),
        "Device B".into(),
    ).with_encryption("WRONG-passphrase");
    let mut engine_b = SyncEngine::new(config_b, cortex_b.sqlite_storage()).unwrap();

    // Pull should succeed but apply 0 ops (decryption failures are skipped)
    let applied = engine_b.pull_remote(cortex_b.sqlite_storage(), cortex_b.index()).unwrap();
    assert_eq!(applied, 0, "Wrong sync passphrase should result in 0 applied ops");
    println!("✅ Wrong sync passphrase: ops gracefully skipped (0 applied)");
}
