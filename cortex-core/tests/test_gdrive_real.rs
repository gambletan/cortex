//! Real Google Drive sync integration test.
//! Only runs when Google Drive is available on the system.

use cortex_core::sync::oplog;
use cortex_core::sync::provider;
use cortex_core::sync::{SyncConfig, SyncEngine};
use cortex_core::sync::oplog::SyncPayload;
use cortex_core::types::*;
use cortex_core::Cortex;
use uuid::Uuid;

fn find_gdrive() -> Option<std::path::PathBuf> {
    provider::detect_all_providers()
        .into_iter()
        .find(|p| matches!(p.provider, provider::CloudProvider::GoogleDrive))
        .map(|p| p.sync_dir)
}

#[test]
fn test_real_gdrive_provider_detection() {
    let providers = provider::detect_all_providers();
    println!("Detected {} providers:", providers.len());
    for p in &providers {
        println!("  {} → {}", p.provider.as_str(), p.sync_dir.display());
    }

    if let Some(gdrive) = find_gdrive() {
        println!("✅ Google Drive detected: {}", gdrive.display());
    } else {
        println!("⚠️  Google Drive not installed — skipping real sync test");
    }
}

#[test]
fn test_real_gdrive_encrypted_sync() {
    let gdrive = match find_gdrive() {
        Some(p) => p,
        None => {
            println!("⚠️  Google Drive not available — skipping");
            return;
        }
    };

    // Use a test-specific subfolder to avoid polluting user's Drive
    let sync_dir = gdrive.parent().unwrap().join("cortex-test-sync");

    // Device A: create encrypted sync
    let cortex_a = Cortex::in_memory().unwrap();
    let config_a = SyncConfig::new(
        sync_dir.clone(),
        "test-device-a".into(),
        "Test Device A".into(),
    ).with_encryption("gdrive-test-passphrase");
    cortex_a.enable_sync(config_a).unwrap();
    println!("✅ Device A: sync enabled with encryption");

    // Ingest a Private memory — should NOT sync
    cortex_a.ingest("private secret", "test", None, None, None).unwrap();
    let device_dir_a = sync_dir.join("devices/test-device-a");
    let files = oplog::list_oplog_files(&device_dir_a).unwrap_or_default();
    let ops: usize = files.iter()
        .map(|f| oplog::read_oplog(f, 0, None).unwrap().0.len())
        .sum();
    assert_eq!(ops, 0, "Private memory must NOT appear in oplog");
    println!("✅ Private memory correctly NOT synced");

    // Manually write a Shared memory op to test encryption
    let mem = MemObjectBuilder::new(
        MemoryTier::Episodic,
        MemContent::Text("shared memory for gdrive test".into()),
        MemSource::new("test"),
    )
    .privacy(PrivacyLevel::Shared { scope: "all".into() })
    .build();

    // Use SyncEngine directly to write encrypted op
    let cortex_b = Cortex::in_memory().unwrap();
    let config_b = SyncConfig::new(
        sync_dir.clone(),
        "test-device-b".into(),
        "Test Device B".into(),
    ).with_encryption("gdrive-test-passphrase");
    let mut engine_b = SyncEngine::new(config_b, cortex_b.sqlite_storage()).unwrap();
    engine_b.record_op(SyncPayload::MemoryUpsert { memory: mem.clone() }).unwrap();
    println!("✅ Device B: wrote encrypted memory op");

    // Verify the oplog file contains ENC1: (encrypted)
    let device_dir_b = sync_dir.join("devices/test-device-b");
    let files_b = oplog::list_oplog_files(&device_dir_b).unwrap();
    assert!(!files_b.is_empty(), "Should have oplog files");
    let content = std::fs::read_to_string(&files_b[0]).unwrap();
    assert!(content.starts_with("ENC1:"), "Oplog should be encrypted");
    assert!(!content.contains("shared memory"), "Plaintext should NOT appear in encrypted file");
    println!("✅ Verified: oplog file is encrypted (ENC1:)");
    println!("   File: {}", files_b[0].display());
    println!("   Size: {} bytes", content.len());

    // Device A pulls from Device B
    let applied = cortex_a.sync_pull().unwrap();
    assert!(applied > 0, "Should apply at least 1 remote op");
    println!("✅ Device A pulled {} ops from Device B", applied);

    // Verify the memory arrived on Device A
    let synced = cortex_a.storage().get_memory(mem.id).unwrap();
    assert!(synced.is_some(), "Synced memory should exist on Device A");
    println!("✅ Memory synced from B → A via Google Drive");

    // Check sync status
    let status = cortex_a.sync_status().unwrap();
    println!("\n📊 Sync Status:");
    println!("  Provider: {}", status.provider);
    println!("  Device: {} ({})", status.device_name, status.device_id);
    println!("  Remote devices: {}", status.remote_devices.len());

    // Cleanup
    let _ = std::fs::remove_dir_all(&sync_dir);
    println!("\n🧹 Cleaned up test data from Google Drive");
    println!("✅ ALL REAL GOOGLE DRIVE TESTS PASSED!");
}
