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

/// A demotion to Private is a local-only decision: a peer that has not yet seen the
/// retraction must not be able to flip the local copy back to syncable. Before the fix,
/// the demoting device kept no entity HLC for its own retraction, so the peer's stale
/// upsert (older than the retraction) overwrote the Private memory with `Public` — the
/// user's opt-out was silently undone and every later local edit went back on the wire.
#[test]
fn stale_peer_upsert_cannot_undo_private_demotion() {
    let tmp = TempDir::new().unwrap();
    let sync_dir = tmp.path().join("cortex-sync");

    let a = Cortex::in_memory().unwrap();
    a.enable_sync(make_sync_config(&sync_dir, "dev-a")).unwrap();
    let mem = a
        .ingest_with_options(
            "shared then retracted",
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

    // B touches the memory before it has seen A's retraction.
    b.set_memory_privacy(mem.id, PrivacyLevel::Public).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(5));

    // A then retracts it (strictly later than B's edit).
    a.set_memory_privacy(mem.id, PrivacyLevel::Private).unwrap();

    // A pulls B's stale upsert: the local Private decision must stand.
    a.sync_pull().unwrap();
    let local = a.storage().get_memory(mem.id).unwrap().expect("local copy is kept on demotion");
    assert!(
        !local.privacy.is_syncable(),
        "a peer's stale upsert flipped a locally-Private memory to {:?}",
        local.privacy
    );
}

/// Right to delete: deleting a locally-created Shared memory must retract it from peers.
/// Before the fix, `record_memory_event` decided "was this synced?" from the entity HLC,
/// which is only ever set for *remote* ops — so a delete of the user's own shared memory
/// was never recorded and every other device kept the copy indefinitely.
#[test]
fn deleting_own_shared_memory_propagates_to_peers() {
    let tmp = TempDir::new().unwrap();
    let sync_dir = tmp.path().join("cortex-sync");

    let a = Cortex::in_memory().unwrap();
    a.enable_sync(make_sync_config(&sync_dir, "dev-a")).unwrap();
    let shared = a
        .ingest_with_options(
            "shared then deleted",
            "test",
            None,
            None,
            None,
            None,
            Some(PrivacyLevel::Shared { scope: "all".into() }),
        )
        .unwrap();
    let private = a.ingest("private then deleted", "test", None, None, None).unwrap();

    let b = Cortex::in_memory().unwrap();
    b.enable_sync(make_sync_config(&sync_dir, "dev-b")).unwrap();
    assert!(b.sync_pull().unwrap() >= 1);
    assert!(b.storage().get_memory(shared.id).unwrap().is_some());

    let before = read_ops(&sync_dir, "dev-a").len();
    a.delete_memory(private.id).unwrap();
    assert_eq!(
        read_ops(&sync_dir, "dev-a").len(),
        before,
        "deleting a Private memory must not touch the oplog"
    );

    a.delete_memory(shared.id).unwrap();
    b.sync_pull().unwrap();
    assert!(
        b.storage().get_memory(shared.id).unwrap().is_none(),
        "peer still holds a shared memory its owner deleted"
    );
}

/// A peer's retraction of the shared copy must not destroy the copy this device took
/// back as Private — and must leave no tombstone that would let a later peer upsert
/// recreate it as syncable.
#[test]
fn peer_delete_does_not_remove_locally_private_copy() {
    let tmp = TempDir::new().unwrap();
    let sync_dir = tmp.path().join("cortex-sync");

    let a = Cortex::in_memory().unwrap();
    a.enable_sync(make_sync_config(&sync_dir, "dev-a")).unwrap();
    let b = Cortex::in_memory().unwrap();
    b.enable_sync(make_sync_config(&sync_dir, "dev-b")).unwrap();

    let mem = b
        .ingest_with_options(
            "created on b",
            "test",
            None,
            None,
            None,
            None,
            Some(PrivacyLevel::Shared { scope: "all".into() }),
        )
        .unwrap();
    assert!(a.sync_pull().unwrap() >= 1);

    // A keeps it but takes it off the wire; B then deletes its shared copy.
    a.set_memory_privacy(mem.id, PrivacyLevel::Private).unwrap();
    b.delete_memory(mem.id).unwrap();
    a.sync_pull().unwrap();

    let local = a.storage().get_memory(mem.id).unwrap();
    assert!(
        local.is_some_and(|m| !m.privacy.is_syncable()),
        "peer delete removed (or re-shared) the locally-Private copy"
    );
}
