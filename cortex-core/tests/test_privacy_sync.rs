//! Iteration 17: per-memory privacy opt-in for sync + persistent sync settings.

use cortex_core::sync::oplog::{self, SyncPayload};
use cortex_core::sync::SyncConfig;
use cortex_core::types::*;
use cortex_core::Cortex;
use tempfile::TempDir;

fn make_sync_config(sync_dir: &std::path::Path, device_id: &str) -> SyncConfig {
    SyncConfig::new(
        sync_dir.to_path_buf(),
        device_id.to_string(),
        format!("Test Device {}", device_id),
    )
}

fn read_ops(sync_dir: &std::path::Path, device_id: &str) -> Vec<oplog::SyncOp> {
    let device_dir = sync_dir.join("devices").join(device_id);
    let mut ops = Vec::new();
    for f in oplog::list_oplog_files(&device_dir).unwrap_or_default() {
        let (file_ops, _) = oplog::read_oplog(&f, 0, None).unwrap();
        ops.extend(file_ops);
    }
    ops
}

#[test]
fn ingest_with_shared_privacy_is_synced() {
    let tmp = TempDir::new().unwrap();
    let sync_dir = tmp.path().join("cortex-sync");
    let cortex = Cortex::in_memory().unwrap();
    cortex.enable_sync(make_sync_config(&sync_dir, "dev-a")).unwrap();

    // Private (default) ingest: no oplog entry
    cortex.ingest("private note", "test", None, None, None).unwrap();
    assert_eq!(read_ops(&sync_dir, "dev-a").len(), 0);

    // Shared ingest: exactly one upsert hits the oplog
    let mem = cortex
        .ingest_with_options(
            "shared note",
            "test",
            None,
            None,
            None,
            None,
            Some(PrivacyLevel::Shared { scope: "all".into() }),
        )
        .unwrap();
    let ops = read_ops(&sync_dir, "dev-a");
    assert_eq!(ops.len(), 1, "shared ingest must record a sync op");
    match &ops[0].payload {
        SyncPayload::MemoryUpsert { memory } => assert_eq!(memory.id, mem.id),
        other => panic!("expected MemoryUpsert, got {:?}", std::mem::discriminant(other)),
    }
}

#[test]
fn set_memory_privacy_promote_then_retract() {
    let tmp = TempDir::new().unwrap();
    let sync_dir = tmp.path().join("cortex-sync");
    let cortex = Cortex::in_memory().unwrap();
    cortex.enable_sync(make_sync_config(&sync_dir, "dev-a")).unwrap();

    let mem = cortex.ingest("starts private", "test", None, None, None).unwrap();
    assert_eq!(read_ops(&sync_dir, "dev-a").len(), 0);

    // Promote: upsert recorded
    let updated = cortex
        .set_memory_privacy(mem.id, PrivacyLevel::Shared { scope: "all".into() })
        .unwrap();
    assert!(updated.privacy.is_syncable());
    let ops = read_ops(&sync_dir, "dev-a");
    assert_eq!(ops.len(), 1);
    assert!(matches!(&ops[0].payload, SyncPayload::MemoryUpsert { memory } if memory.id == mem.id));

    // Demote back to Private: a delete op retracts it from remote devices
    cortex.set_memory_privacy(mem.id, PrivacyLevel::Private).unwrap();
    let ops = read_ops(&sync_dir, "dev-a");
    assert_eq!(ops.len(), 2, "retraction must record a MemoryDelete op");
    assert!(matches!(&ops[1].payload, SyncPayload::MemoryDelete { id } if *id == mem.id));

    // Local copy is kept, now Private
    let local = cortex.storage().get_memory(mem.id).unwrap().unwrap();
    assert!(!local.privacy.is_syncable());

    // Demoting an always-private memory records nothing extra
    let other = cortex.ingest("never shared", "test", None, None, None).unwrap();
    cortex.set_memory_privacy(other.id, PrivacyLevel::Private).unwrap();
    assert_eq!(read_ops(&sync_dir, "dev-a").len(), 2);
}

#[test]
fn shared_memory_actually_reaches_second_device() {
    let tmp = TempDir::new().unwrap();
    let sync_dir = tmp.path().join("cortex-sync");

    let a = Cortex::in_memory().unwrap();
    a.enable_sync(make_sync_config(&sync_dir, "dev-a")).unwrap();
    let mem = a
        .ingest_with_options(
            "the shared fact travels",
            "test",
            None,
            None,
            None,
            None,
            Some(PrivacyLevel::Shared { scope: "all".into() }),
        )
        .unwrap();

    let b = Cortex::in_memory().unwrap();
    b.enable_sync(make_sync_config(&sync_dir, "dev-b")).unwrap();
    let applied = b.sync_pull().unwrap();
    assert!(applied >= 1, "device B must apply device A's op, applied={applied}");
    let got = b.storage().get_memory(mem.id).unwrap();
    assert!(got.is_some(), "shared memory must exist on device B");
}

#[test]
fn sync_settings_persist_and_resume() {
    let tmp = TempDir::new().unwrap();
    let sync_dir = tmp.path().join("cortex-sync");
    let db_path = tmp.path().join("memory.db");
    let db_str = db_path.to_str().unwrap();

    // Session 1: enable (unencrypted — keychain not exercised in tests), write shared
    {
        let cortex = Cortex::open(db_str).unwrap();
        cortex.enable_sync(make_sync_config(&sync_dir, "dev-p")).unwrap();
        assert!(cortex.sync_status().is_some());
    }

    // Session 2: fresh open — sync must resume from persisted settings
    {
        let cortex = Cortex::open(db_str).unwrap();
        assert!(cortex.sync_status().is_none(), "before resume, sync is off");
        let resumed = cortex.resume_sync().unwrap();
        assert!(resumed, "resume_sync must restore persisted config");
        let status = cortex.sync_status().expect("sync active after resume");
        assert_eq!(status.device_id, "dev-p");

        // And recording works after resume
        cortex
            .ingest_with_options(
                "post-restart shared memory",
                "test",
                None,
                None,
                None,
                None,
                Some(PrivacyLevel::Shared { scope: "all".into() }),
            )
            .unwrap();
        assert_eq!(read_ops(&sync_dir, "dev-p").len(), 1);
    }
}

#[test]
fn pull_invalidates_retrieval_cache() {
    let tmp = TempDir::new().unwrap();
    let sync_dir = tmp.path().join("cortex-sync");

    let a = Cortex::in_memory().unwrap();
    a.enable_sync(make_sync_config(&sync_dir, "dev-a")).unwrap();
    let mem = a
        .ingest_with_options(
            "the beacon memory travels",
            "test",
            None,
            None,
            None,
            None,
            Some(PrivacyLevel::Shared { scope: "all".into() }),
        )
        .unwrap();

    let b = Cortex::in_memory().unwrap();
    b.enable_sync(make_sync_config(&sync_dir, "dev-b")).unwrap();
    assert!(b.sync_pull().unwrap() >= 1);

    // Fill B's retrieval cache with a query that finds the memory.
    let r1 = b
        .retrieve_with_namespace("beacon memory", 5, None, None, None, None)
        .unwrap();
    assert!(r1.iter().any(|r| r.memory.id == mem.id), "B must see the shared memory");

    // A retracts (privacy demotion). B pulls the delete, then repeats the SAME query:
    // the cached result must not survive the pull.
    a.set_memory_privacy(mem.id, PrivacyLevel::Private).unwrap();
    assert!(b.sync_pull().unwrap() >= 1, "delete op must be applied");
    let r2 = b
        .retrieve_with_namespace("beacon memory", 5, None, None, None, None)
        .unwrap();
    assert!(
        !r2.iter().any(|r| r.memory.id == mem.id),
        "retracted memory must not be served from a stale cache after pull"
    );
}

#[test]
fn resume_without_settings_is_noop() {
    let cortex = Cortex::in_memory().unwrap();
    assert!(!cortex.resume_sync().unwrap());
    assert!(cortex.sync_status().is_none());
}

#[test]
fn encrypted_settings_without_passphrase_stay_disabled() {
    // Never touch the developer's real login keychain from tests.
    std::env::set_var("CORTEX_NO_KEYCHAIN", "1");
    std::env::remove_var("CORTEX_SYNC_PASSPHRASE");

    let tmp = TempDir::new().unwrap();
    let sync_dir = tmp.path().join("cortex-sync");
    let db_path = tmp.path().join("memory.db");
    let db_str = db_path.to_str().unwrap();

    {
        let cortex = Cortex::open(db_str).unwrap();
        let config = make_sync_config(&sync_dir, "cortex-test-no-keychain-entry-xyz")
            .with_encryption("session-only-pass");
        cortex.enable_sync(config).unwrap();
    }

    // New session: encryption flag is persisted, but no passphrase source exists
    // → resume must fail SAFE: sync off, no error, no plaintext fallback.
    {
        let cortex = Cortex::open(db_str).unwrap();
        let resumed = cortex.resume_sync().unwrap();
        assert!(!resumed, "resume must not succeed without a passphrase");
        assert!(cortex.sync_status().is_none());
    }
    std::env::remove_var("CORTEX_NO_KEYCHAIN");
}
