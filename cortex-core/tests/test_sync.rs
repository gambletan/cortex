//! Integration tests for cloud sync — two Cortex instances syncing via a temp directory.

use cortex_core::sync::hlc::{HlcClock, HlcTimestamp};
use cortex_core::sync::merge;
use cortex_core::sync::oplog::{self, OpLogWriter, SyncOp, SyncPayload};
use cortex_core::sync::provider;
use cortex_core::sync::state;
use cortex_core::sync::{SyncConfig, SyncEngine};
use cortex_core::types::*;
use cortex_core::Cortex;
use rusqlite::Connection;
use std::path::PathBuf;
use tempfile::TempDir;
use uuid::Uuid;

fn make_sync_config(sync_dir: &std::path::Path, device_id: &str) -> SyncConfig {
    SyncConfig::new(
        sync_dir.to_path_buf(),
        device_id.to_string(),
        format!("Test Device {}", device_id),
    )
}

#[test]
fn test_two_devices_sync_memories() {
    let tmp = TempDir::new().unwrap();
    let sync_dir = tmp.path().join("cortex-sync");

    // Device A: create Cortex + enable sync
    let cortex_a = Cortex::in_memory().unwrap();
    let conn_a = Connection::open_in_memory().unwrap();
    let config_a = make_sync_config(&sync_dir, "device-a");
    let mut engine_a = SyncEngine::new(config_a, &conn_a).unwrap();

    // Device B: create Cortex + enable sync
    let cortex_b = Cortex::in_memory().unwrap();
    let conn_b = Connection::open_in_memory().unwrap();
    let config_b = make_sync_config(&sync_dir, "device-b");
    let mut engine_b = SyncEngine::new(config_b, &conn_b).unwrap();

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
        .pull_remote(cortex_b.storage(), cortex_b.index(), &conn_b)
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
        .pull_remote(cortex_a.storage(), cortex_a.index(), &conn_a)
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
    let conn_a = Connection::open_in_memory().unwrap();
    let mut engine_a = SyncEngine::new(make_sync_config(&sync_dir, "device-a"), &conn_a).unwrap();

    let cortex_b = Cortex::in_memory().unwrap();
    let conn_b = Connection::open_in_memory().unwrap();
    let mut engine_b = SyncEngine::new(make_sync_config(&sync_dir, "device-b"), &conn_b).unwrap();

    // A creates a memory
    let mem = cortex_a.ingest("temp note", "test", None, None, None).unwrap();
    engine_a.record_op(SyncPayload::MemoryUpsert {
        memory: cortex_a.storage().get_memory(mem.id).unwrap().unwrap(),
    }).unwrap();

    // B pulls it
    engine_b.pull_remote(cortex_b.storage(), cortex_b.index(), &conn_b).unwrap();
    assert!(cortex_b.storage().get_memory(mem.id).unwrap().is_some());

    // A deletes the memory
    cortex_a.storage().delete_memory(mem.id).unwrap();
    engine_a.record_op(SyncPayload::MemoryDelete { id: mem.id }).unwrap();

    // B pulls the delete
    let applied = engine_b.pull_remote(cortex_b.storage(), cortex_b.index(), &conn_b).unwrap();
    assert!(applied > 0);
    assert!(cortex_b.storage().get_memory(mem.id).unwrap().is_none(), "Memory should be deleted on B");
}

#[test]
fn test_lww_conflict_resolution() {
    let tmp = TempDir::new().unwrap();
    let sync_dir = tmp.path().join("cortex-sync");

    let cortex_a = Cortex::in_memory().unwrap();
    let conn_a = Connection::open_in_memory().unwrap();
    let mut engine_a = SyncEngine::new(make_sync_config(&sync_dir, "device-a"), &conn_a).unwrap();

    let cortex_b = Cortex::in_memory().unwrap();
    let conn_b = Connection::open_in_memory().unwrap();
    let mut engine_b = SyncEngine::new(make_sync_config(&sync_dir, "device-b"), &conn_b).unwrap();

    // Both devices create a memory with the same ID (simulating concurrent edit)
    let shared_id = Uuid::new_v4();
    let mem_v1 = MemObjectBuilder::new(
        MemoryTier::Episodic,
        MemContent::Text("version 1 from A".into()),
        MemSource::new("test"),
    ).build();

    // Manually set same ID
    let mut mem_a = mem_v1;
    // Store on A with an older timestamp oplog entry
    let old_hlc = HlcTimestamp::new(1000, 0, "device-a");
    cortex_a.storage().store_memory(&mem_a).unwrap();
    state::set_entity_hlc(&conn_a, "memory", mem_a.id, &old_hlc).unwrap();

    // B writes a newer version
    let mut mem_b = mem_a.clone();
    mem_b.content = MemContent::Text("version 2 from B".into());
    mem_b.content_hash = None; // Different content → different hash
    cortex_b.storage().store_memory(&mem_b).unwrap();

    // B's op has a newer HLC
    let new_hlc = HlcTimestamp::new(2000, 0, "device-b");
    let op_b = SyncOp {
        op_id: Uuid::new_v4(),
        hlc: new_hlc,
        device_id: "device-b".into(),
        payload: SyncPayload::MemoryUpsert { memory: mem_b.clone() },
    };

    // Apply B's op on A — should win (newer HLC)
    let result = merge::apply_op(&op_b, cortex_a.storage(), cortex_a.index(), &conn_a).unwrap();
    assert!(matches!(result, merge::MergeResult::Applied));

    let updated = cortex_a.storage().get_memory(mem_a.id).unwrap().unwrap();
    if let MemContent::Text(ref text) = updated.content {
        assert_eq!(text, "version 2 from B");
    } else {
        panic!("Expected Text content");
    }
}

#[test]
fn test_sync_status() {
    let tmp = TempDir::new().unwrap();
    let sync_dir = tmp.path().join("cortex-sync");

    let conn = Connection::open_in_memory().unwrap();
    let engine = SyncEngine::new(make_sync_config(&sync_dir, "device-a"), &conn).unwrap();

    let status = engine.status().unwrap();
    assert!(status.enabled);
    assert_eq!(status.device_id, "device-a");
    assert!(status.remote_devices.is_empty());
}

#[test]
fn test_provider_detection() {
    // Just verify it doesn't panic
    let _ = provider::detect_provider();
    let all = provider::detect_all_providers();
    // On CI there may be no cloud providers, that's fine
    let _ = all;
}

#[test]
fn test_hlc_across_devices() {
    let clock_a = HlcClock::new("a");
    let clock_b = HlcClock::new("b");

    let t1 = clock_a.tick();
    let t2 = clock_b.update(&t1);
    let t3 = clock_a.update(&t2);

    // Causal ordering must hold
    assert!(t1 < t2);
    assert!(t2 < t3);
}

#[test]
fn test_oplog_partial_line_recovery() {
    let tmp = TempDir::new().unwrap();
    let device_dir = tmp.path().join("device-a");
    std::fs::create_dir_all(&device_dir).unwrap();

    let file_path = device_dir.join("oplog-2026-03-23-001.jsonl");

    // Write a valid line + a partial/corrupt line
    let op = SyncOp {
        op_id: Uuid::new_v4(),
        hlc: HlcTimestamp::new(1000, 0, "device-a"),
        device_id: "device-a".into(),
        payload: SyncPayload::MemoryDelete { id: Uuid::new_v4() },
    };
    let valid_line = serde_json::to_string(&op).unwrap();
    std::fs::write(&file_path, format!("{}\n{{corrupt partial", valid_line)).unwrap();

    // Should read 1 valid op and skip the corrupt line
    let (ops, _) = oplog::read_oplog(&file_path, 0).unwrap();
    assert_eq!(ops.len(), 1);
    assert_eq!(ops[0].op_id, op.op_id);
}
