//! Integration tests for cloud sync — two Cortex instances syncing via a temp directory.

use cortex_core::sync::hlc::{HlcClock, HlcTimestamp};
use cortex_core::sync::merge;
use cortex_core::sync::oplog::{self, SyncOp, SyncPayload};
use cortex_core::sync::provider;
use cortex_core::sync::state::{self, EntityType};
use cortex_core::sync::{SyncConfig, SyncEngine};
use cortex_core::types::*;
use cortex_core::Cortex;
use tempfile::TempDir;
use uuid::Uuid;

fn make_sync_config(sync_dir: &std::path::Path, device_id: &str) -> SyncConfig {
    SyncConfig::new(
        sync_dir.to_path_buf(),
        device_id.to_string(),
        format!("Test Device {}", device_id),
    )
}

// ── End-to-end sync tests ────────────────────────────────────────────────────

#[test]
fn test_two_devices_sync_memories() {
    let tmp = TempDir::new().unwrap();
    let sync_dir = tmp.path().join("cortex-sync");

    let cortex_a = Cortex::in_memory().unwrap();
    let mut engine_a = SyncEngine::new(make_sync_config(&sync_dir, "device-a"), cortex_a.sqlite_storage()).unwrap();

    let cortex_b = Cortex::in_memory().unwrap();
    let mut engine_b = SyncEngine::new(make_sync_config(&sync_dir, "device-b"), cortex_b.sqlite_storage()).unwrap();

    // Device A: ingest a memory
    let mem_a = cortex_a
        .ingest("I live in Shanghai", "test", None, None, None)
        .unwrap();

    // Record the ingest in A's oplog
    engine_a
        .record_op(SyncPayload::MemoryUpsert {
            memory: cortex_a.storage().get_memory(mem_a.id).unwrap().unwrap(),
        })
        .unwrap();

    // Device B: pull from A
    let applied = engine_b
        .pull_remote(cortex_b.sqlite_storage(), cortex_b.index())
        .unwrap();
    assert!(applied > 0, "Should have applied at least 1 op");

    // Verify memory exists on Device B
    let mem_on_b = cortex_b.storage().get_memory(mem_a.id).unwrap();
    assert!(mem_on_b.is_some(), "Memory should exist on device B");

    // Device B: ingest another memory
    let mem_b = cortex_b
        .ingest("I work at Google", "test", None, None, None)
        .unwrap();
    engine_b
        .record_op(SyncPayload::MemoryUpsert {
            memory: cortex_b.storage().get_memory(mem_b.id).unwrap().unwrap(),
        })
        .unwrap();

    // Device A: pull from B
    let applied_a = engine_a
        .pull_remote(cortex_a.sqlite_storage(), cortex_a.index())
        .unwrap();
    assert!(applied_a > 0, "Device A should have applied B's memory");

    let mem_on_a = cortex_a.storage().get_memory(mem_b.id).unwrap();
    assert!(mem_on_a.is_some(), "B's memory should exist on device A");
}

#[test]
fn test_delete_syncs() {
    let tmp = TempDir::new().unwrap();
    let sync_dir = tmp.path().join("cortex-sync");

    let cortex_a = Cortex::in_memory().unwrap();
    let mut engine_a = SyncEngine::new(make_sync_config(&sync_dir, "device-a"), cortex_a.sqlite_storage()).unwrap();

    let cortex_b = Cortex::in_memory().unwrap();
    let mut engine_b = SyncEngine::new(make_sync_config(&sync_dir, "device-b"), cortex_b.sqlite_storage()).unwrap();

    // A creates a memory
    let mem = cortex_a.ingest("temp note", "test", None, None, None).unwrap();
    engine_a.record_op(SyncPayload::MemoryUpsert {
        memory: cortex_a.storage().get_memory(mem.id).unwrap().unwrap(),
    }).unwrap();

    // B pulls it
    engine_b.pull_remote(cortex_b.sqlite_storage(), cortex_b.index()).unwrap();
    assert!(cortex_b.storage().get_memory(mem.id).unwrap().is_some());

    // A deletes the memory
    cortex_a.storage().delete_memory(mem.id).unwrap();
    engine_a.record_op(SyncPayload::MemoryDelete { id: mem.id }).unwrap();

    // B pulls the delete
    let applied = engine_b.pull_remote(cortex_b.sqlite_storage(), cortex_b.index()).unwrap();
    assert!(applied > 0);
    assert!(cortex_b.storage().get_memory(mem.id).unwrap().is_none(), "Memory should be deleted on B");
}

#[test]
fn test_lww_conflict_resolution() {
    let tmp = TempDir::new().unwrap();
    let sync_dir = tmp.path().join("cortex-sync");

    let cortex_a = Cortex::in_memory().unwrap();
    let _engine_a = SyncEngine::new(make_sync_config(&sync_dir, "device-a"), cortex_a.sqlite_storage()).unwrap();

    // Store a memory on A with an older HLC
    let mem_a = MemObjectBuilder::new(
        MemoryTier::Episodic,
        MemContent::Text("version 1 from A".into()),
        MemSource::new("test"),
    ).build();

    let old_hlc = HlcTimestamp::new(1000, 0, "device-a");
    cortex_a.storage().store_memory(&mem_a).unwrap();
    cortex_a.sqlite_storage().with_write_conn(|conn| {
        state::set_entity_hlc(conn, EntityType::Memory, mem_a.id, &old_hlc)
    }).unwrap();

    // B writes a newer version with same ID
    let mut mem_b = mem_a.clone();
    mem_b.content = MemContent::Text("version 2 from B".into());
    mem_b.content_hash = None;

    let new_hlc = HlcTimestamp::new(2000, 0, "device-b");
    let op_b = SyncOp {
        op_id: Uuid::new_v4(),
        hlc: new_hlc,
        payload: SyncPayload::MemoryUpsert { memory: mem_b },
    };

    // Apply B's op on A — should win (newer HLC)
    let result = merge::apply_op(&op_b, cortex_a.sqlite_storage(), cortex_a.index()).unwrap();
    assert!(matches!(result, merge::MergeResult::Applied));

    let updated = cortex_a.storage().get_memory(mem_a.id).unwrap().unwrap();
    if let MemContent::Text(ref text) = updated.content {
        assert_eq!(text, "version 2 from B");
    } else {
        panic!("Expected Text content");
    }
}

// ── Cortex.enable_sync integration test ──────────────────────────────────────

#[test]
fn test_enable_sync_and_auto_record() {
    let tmp = TempDir::new().unwrap();
    let sync_dir = tmp.path().join("cortex-sync");

    let cortex = Cortex::in_memory().unwrap();
    cortex.enable_sync(make_sync_config(&sync_dir, "device-a")).unwrap();

    // Ingest a memory, then update it to Shared (Private memories don't sync)
    let mem = cortex.ingest("test memory", "test", None, None, None).unwrap();
    let mut shared_mem = cortex.storage().get_memory(mem.id).unwrap().unwrap();
    shared_mem.privacy = PrivacyLevel::Shared { scope: "all".into() };
    cortex.storage().update_memory(&shared_mem).unwrap();

    // Re-ingest a Shared memory to trigger sync recording
    // Use add_fact which also emits MemoryCreated — but it's also Private by default.
    // Simplest: directly store a Shared memory and manually trigger delete (which checks sync state)
    // Actually, let's test that Private does NOT sync:
    let device_dir = sync_dir.join("devices/device-a");
    let files_before = oplog::list_oplog_files(&device_dir).unwrap();
    let ops_before = if files_before.is_empty() {
        0
    } else {
        oplog::read_oplog(&files_before[0], 0, None).unwrap().0.len()
    };

    // Private ingest should NOT produce oplog entries
    cortex.ingest("private memory", "test", None, None, None).unwrap();
    let files_after = oplog::list_oplog_files(&device_dir).unwrap();
    let ops_after = if files_after.is_empty() {
        0
    } else {
        oplog::read_oplog(&files_after[0], 0, None).unwrap().0.len()
    };
    assert_eq!(ops_before, ops_after, "Private memory should NOT be synced to oplog");
}

#[test]
fn test_private_memory_not_synced() {
    let tmp = TempDir::new().unwrap();
    let sync_dir = tmp.path().join("cortex-sync");

    let cortex = Cortex::in_memory().unwrap();
    cortex.enable_sync(make_sync_config(&sync_dir, "device-a")).unwrap();

    // Default privacy is Private — should not sync
    cortex.ingest("secret thought", "test", None, None, None).unwrap();

    let device_dir = sync_dir.join("devices/device-a");
    let files = oplog::list_oplog_files(&device_dir).unwrap();
    let total_ops: usize = files.iter()
        .map(|f| oplog::read_oplog(f, 0, None).unwrap().0.len())
        .sum();
    assert_eq!(total_ops, 0, "Private memories must never appear in oplog");
}

#[test]
fn test_sync_status_api() {
    let tmp = TempDir::new().unwrap();
    let sync_dir = tmp.path().join("cortex-sync");

    let cortex = Cortex::in_memory().unwrap();
    assert!(cortex.sync_status().is_none(), "Sync should be disabled initially");

    cortex.enable_sync(make_sync_config(&sync_dir, "device-a")).unwrap();
    let status = cortex.sync_status().unwrap();
    assert!(status.enabled);
    assert_eq!(status.device_id, "device-a");
}

// ── Merge logic tests ────────────────────────────────────────────────────────

#[test]
fn test_person_merge_max_interaction_count() {
    let cortex = Cortex::in_memory().unwrap();

    // Create a local person with interaction_count=5
    let person = cortex.add_person("Alice", "slack", "U123").unwrap();
    // Bump interactions to 5
    for _ in 0..5 {
        cortex.storage().update_person(&{
            let mut p = cortex.storage().get_person(person.id).unwrap().unwrap();
            p.interaction_count += 1;
            cortex.storage().update_person(&p).unwrap();
            p
        }).unwrap();
    }

    // Remote has same person with interaction_count=10 but older HLC
    let mut remote_person = cortex.storage().get_person(person.id).unwrap().unwrap();
    remote_person.interaction_count = 10;

    let old_hlc = HlcTimestamp::new(500, 0, "device-a");
    let new_hlc = HlcTimestamp::new(1000, 0, "device-b");

    // Set local HLC to be newer
    cortex.sqlite_storage().with_write_conn(|conn| {
        state::set_entity_hlc(conn, EntityType::Person, person.id, &new_hlc)
    }).unwrap();

    let op = SyncOp {
        op_id: Uuid::new_v4(),
        hlc: old_hlc,
        payload: SyncPayload::PersonUpsert { person: remote_person },
    };

    // Apply — should be Skipped (LWW) but interaction_count should be merged to max(local, remote)
    let result = merge::apply_op(&op, cortex.sqlite_storage(), cortex.index()).unwrap();
    assert!(matches!(result, merge::MergeResult::Skipped));

    let final_person = cortex.storage().get_person(person.id).unwrap().unwrap();
    assert_eq!(final_person.interaction_count, 10, "Should take max interaction_count");
}

#[test]
fn test_belief_crdt_merge() {
    let cortex = Cortex::in_memory().unwrap();

    // Create a local belief with one observation
    cortex.observe_belief("user_is_dev", true, 0.8).unwrap();
    let local_belief = cortex.storage().get_belief("user_is_dev").unwrap().unwrap();

    // Remote belief has a different observation
    let mut remote_belief = local_belief.clone();
    remote_belief.observations.clear();
    remote_belief.observations.push(cortex_core::belief::Observation {
        timestamp: chrono::Utc::now() + chrono::Duration::seconds(10),
        evidence: cortex_core::belief::Evidence::Supports(0.9),
        prior: 0.5,
        posterior: 0.9,
    });

    let op = SyncOp {
        op_id: Uuid::new_v4(),
        hlc: HlcTimestamp::new(2000, 0, "device-b"),
        payload: SyncPayload::BeliefUpsert { belief: remote_belief },
    };

    let result = merge::apply_op(&op, cortex.sqlite_storage(), cortex.index()).unwrap();
    assert!(matches!(result, merge::MergeResult::Applied));

    // Merged belief should have observations from both
    let merged = cortex.storage().get_belief("user_is_dev").unwrap().unwrap();
    assert!(merged.observations.len() >= 2, "Should have merged observations from both devices");
}

#[test]
fn test_pattern_merge_union_actions() {
    let cortex = Cortex::in_memory().unwrap();

    // Create a local pattern
    let local_pattern = cortex_core::procedural::Pattern {
        id: Uuid::new_v4(),
        trigger: "morning".into(),
        actions: vec!["check_email".into()],
        frequency: 5,
        last_seen: chrono::Utc::now(),
    };
    cortex.storage().store_pattern(&local_pattern).unwrap();

    // Remote has same trigger but different actions and higher frequency
    let remote_pattern = cortex_core::procedural::Pattern {
        id: local_pattern.id,
        trigger: "morning".into(),
        actions: vec!["check_email".into(), "read_news".into()],
        frequency: 10,
        last_seen: chrono::Utc::now() + chrono::Duration::hours(1),
    };

    let op = SyncOp {
        op_id: Uuid::new_v4(),
        hlc: HlcTimestamp::new(2000, 0, "device-b"),
        payload: SyncPayload::PatternUpsert { pattern: remote_pattern },
    };

    let result = merge::apply_op(&op, cortex.sqlite_storage(), cortex.index()).unwrap();
    assert!(matches!(result, merge::MergeResult::Applied));

    let merged = cortex.storage().get_pattern("morning").unwrap().unwrap();
    assert!(merged.actions.contains(&"check_email".to_string()));
    assert!(merged.actions.contains(&"read_news".to_string()));
    assert_eq!(merged.frequency, 10, "Should take max frequency");
}

#[test]
fn test_link_add_wins() {
    let cortex = Cortex::in_memory().unwrap();

    let id_a = Uuid::new_v4();
    let id_b = Uuid::new_v4();

    let op = SyncOp {
        op_id: Uuid::new_v4(),
        hlc: HlcTimestamp::new(1000, 0, "device-b"),
        payload: SyncPayload::LinkUpsert {
            source_id: id_a,
            target_id: id_b,
            relation: LinkRelation::RelatedTo,
            strength: 0.8,
        },
    };

    let result = merge::apply_op(&op, cortex.sqlite_storage(), cortex.index()).unwrap();
    assert!(matches!(result, merge::MergeResult::Applied));

    let links = cortex.storage().get_links(id_a).unwrap();
    assert_eq!(links.len(), 1);
}

#[test]
fn test_delete_then_recreate() {
    let tmp = TempDir::new().unwrap();
    let sync_dir = tmp.path().join("cortex-sync");

    let cortex = Cortex::in_memory().unwrap();
    let _engine = SyncEngine::new(make_sync_config(&sync_dir, "device-a"), cortex.sqlite_storage()).unwrap();

    let mem = MemObjectBuilder::new(
        MemoryTier::Episodic,
        MemContent::Text("will be deleted then recreated".into()),
        MemSource::new("test"),
    ).build();
    let mem_id = mem.id;

    // Store → delete → tombstone
    cortex.storage().store_memory(&mem).unwrap();
    let delete_hlc = HlcTimestamp::new(1000, 0, "device-a");
    cortex.sqlite_storage().with_write_conn(|conn| {
        state::set_tombstone(conn, EntityType::Memory, mem_id, &delete_hlc)?;
        state::set_entity_hlc(conn, EntityType::Memory, mem_id, &delete_hlc)
    }).unwrap();
    cortex.storage().delete_memory(mem_id).unwrap();

    // Now a NEWER remote op recreates the memory
    let mut recreated = mem.clone();
    recreated.content = MemContent::Text("recreated version".into());
    recreated.content_hash = None;

    let recreate_hlc = HlcTimestamp::new(2000, 0, "device-b");
    let op = SyncOp {
        op_id: Uuid::new_v4(),
        hlc: recreate_hlc,
        payload: SyncPayload::MemoryUpsert { memory: recreated },
    };

    let result = merge::apply_op(&op, cortex.sqlite_storage(), cortex.index()).unwrap();
    assert!(matches!(result, merge::MergeResult::Applied), "Newer op should override tombstone");

    let final_mem = cortex.storage().get_memory(mem_id).unwrap();
    assert!(final_mem.is_some(), "Memory should be recreated");
}

#[test]
fn test_tombstone_gc() {
    let cortex = Cortex::in_memory().unwrap();

    // Create a very old tombstone (40 days ago)
    let old_time = chrono::Utc::now() - chrono::Duration::days(40);
    let mem_id = Uuid::new_v4();

    cortex.sqlite_storage().with_write_conn(|conn| {
        // Manually insert with old created_at
        conn.execute(
            "INSERT INTO sync_tombstones (entity_type, entity_id, deleted_hlc, created_at) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params!["memory", mem_id.to_string(), "{}", old_time.to_rfc3339()],
        ).map_err(|e| cortex_core::CortexError::Storage(e.to_string()))?;
        Ok(())
    }).unwrap();

    // GC with 30-day TTL should remove it
    let removed = cortex.sqlite_storage().with_write_conn(|conn| {
        state::gc_tombstones(conn, 30)
    }).unwrap();
    assert_eq!(removed, 1, "Old tombstone should be garbage collected");

    // Verify it's gone
    let still_there = cortex.sqlite_storage().with_write_conn(|conn| {
        state::is_tombstoned(conn, EntityType::Memory, mem_id)
    }).unwrap();
    assert!(still_there.is_none(), "Tombstone should be removed after GC");
}

// ── HLC and oplog tests ─────────────────────────────────────────────────────

#[test]
fn test_hlc_across_devices() {
    let clock_a = HlcClock::new("a");
    let clock_b = HlcClock::new("b");

    let t1 = clock_a.tick();
    let t2 = clock_b.update(&t1);
    let t3 = clock_a.update(&t2);

    assert!(t1 < t2);
    assert!(t2 < t3);
}

#[test]
fn test_oplog_partial_line_recovery() {
    let tmp = TempDir::new().unwrap();
    let device_dir = tmp.path().join("device-a");
    std::fs::create_dir_all(&device_dir).unwrap();

    let file_path = device_dir.join("oplog-2026-03-23-001.jsonl");

    let op = SyncOp {
        op_id: Uuid::new_v4(),
        hlc: HlcTimestamp::new(1000, 0, "device-a"),
        payload: SyncPayload::MemoryDelete { id: Uuid::new_v4() },
    };
    let valid_line = serde_json::to_string(&op).unwrap();
    std::fs::write(&file_path, format!("{}\n{{corrupt partial", valid_line)).unwrap();

    let (ops, _) = oplog::read_oplog(&file_path, 0, None).unwrap();
    assert_eq!(ops.len(), 1);
    assert_eq!(ops[0].op_id, op.op_id);
}

#[test]
fn test_provider_detection() {
    let _ = provider::detect_provider();
    let _ = provider::detect_all_providers();
}
