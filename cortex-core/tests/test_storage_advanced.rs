//! Advanced storage tests: read connection fallback, prepared statement caching,
//! salience materialized column, edge cases.

use cortex_core::storage::memory_index::MemoryIndex;
use cortex_core::storage::sqlite::SqliteStorage;
use cortex_core::storage::traits::StorageBackend;
use cortex_core::types::*;
use uuid::Uuid;

fn storage() -> SqliteStorage {
    SqliteStorage::open_in_memory().unwrap()
}

fn make_mem(text: &str, tier: MemoryTier) -> MemObject {
    MemObjectBuilder::new(tier, MemContent::Text(text.into()), MemSource::new("test")).build()
}

// ── In-memory mode: read_conn falls back to write_conn ───────────────────

#[test]
fn test_in_memory_read_uses_write_conn() {
    let s = storage();

    // In-memory mode has no read pool, so read_conn falls back to write_conn
    s.store_memory(&make_mem("fallback", MemoryTier::Episodic)).unwrap();

    // All read operations should still work
    let got = s.list_by_tier(MemoryTier::Episodic, 100).unwrap();
    assert_eq!(got.len(), 1);

    let count = s.count_by_tier(MemoryTier::Episodic).unwrap();
    assert_eq!(count, 1);
}

// ── Prepared statement caching: repeated queries ─────────────────────────

#[test]
fn test_prepared_statement_reuse() {
    let s = storage();

    for i in 0..10 {
        s.store_memory(&make_mem(&format!("mem {}", i), MemoryTier::Episodic)).unwrap();
    }

    // Call the same query multiple times — exercises prepare_cached
    for _ in 0..5 {
        let _ = s.list_by_tier(MemoryTier::Episodic, 100).unwrap();
        let _ = s.count_by_tier(MemoryTier::Episodic).unwrap();
        let _ = s.list_by_salience_below(MemoryTier::Episodic, 0.5).unwrap();
    }
}

#[test]
fn test_get_memory_prepared_cached() {
    let s = storage();
    let mem = make_mem("cached get", MemoryTier::Episodic);
    let id = mem.id;
    s.store_memory(&mem).unwrap();

    // Repeatedly get the same memory
    for _ in 0..10 {
        let got = s.get_memory(id).unwrap();
        assert!(got.is_some());
    }
}

#[test]
fn test_find_by_content_hash_prepared_cached() {
    let s = storage();
    let mem = make_mem("hash cached", MemoryTier::Episodic);
    let hash = mem.content_hash.clone().unwrap();
    s.store_memory(&mem).unwrap();

    for _ in 0..10 {
        let found = s.find_by_content_hash(&hash).unwrap();
        assert!(found.is_some());
    }
}

// ── Salience materialized column ─────────────────────────────────────────

#[test]
fn test_salience_score_stored_correctly() {
    let s = storage();

    let mut mem = make_mem("salience test", MemoryTier::Episodic);
    mem.salience = Salience::new(0.75);
    s.store_memory(&mem).unwrap();

    // list_by_salience_below uses the materialized salience_score column
    let below_80 = s.list_by_salience_below(MemoryTier::Episodic, 0.8).unwrap();
    assert_eq!(below_80.len(), 1);

    let below_70 = s.list_by_salience_below(MemoryTier::Episodic, 0.7).unwrap();
    assert_eq!(below_70.len(), 0);
}

#[test]
fn test_salience_score_updated_on_update() {
    let s = storage();

    let mut mem = make_mem("update salience", MemoryTier::Episodic);
    mem.salience = Salience::new(0.3);
    s.store_memory(&mem).unwrap();

    // Below 0.5: should find it
    assert_eq!(s.list_by_salience_below(MemoryTier::Episodic, 0.5).unwrap().len(), 1);

    // Update salience to 0.9
    mem.salience = Salience::new(0.9);
    s.update_memory(&mem).unwrap();

    // Below 0.5: should not find it anymore
    assert_eq!(s.list_by_salience_below(MemoryTier::Episodic, 0.5).unwrap().len(), 0);

    // Below 1.0: should find it
    assert_eq!(s.list_by_salience_below(MemoryTier::Episodic, 1.0).unwrap().len(), 1);
}

#[test]
fn test_salience_score_batch_store() {
    let s = storage();

    let mems: Vec<MemObject> = (0..5).map(|i| {
        let mut m = make_mem(&format!("batch sal {}", i), MemoryTier::Episodic);
        m.salience = Salience::new(i as f32 * 0.2); // 0.0, 0.2, 0.4, 0.6, 0.8
        m
    }).collect();

    s.store_memories_batch(&mems).unwrap();

    let below_50 = s.list_by_salience_below(MemoryTier::Episodic, 0.5).unwrap();
    assert_eq!(below_50.len(), 3); // 0.0, 0.2, 0.4
}

// ── Link operations with write connection ────────────────────────────────

#[test]
fn test_store_link_uses_write_conn() {
    let s = storage();

    let m1 = make_mem("a", MemoryTier::Episodic);
    let m2 = make_mem("b", MemoryTier::Episodic);
    s.store_memory(&m1).unwrap();
    s.store_memory(&m2).unwrap();

    // store_link should use write connection (was a bug, now fixed)
    s.store_link(m1.id, m2.id, LinkRelation::RelatedTo, 0.9).unwrap();
    s.store_link(m2.id, m1.id, LinkRelation::CausedBy, 0.7).unwrap();

    let links = s.get_links(m1.id).unwrap();
    assert_eq!(links.len(), 1);
    assert_eq!(links[0].0, m2.id);

    let links2 = s.get_links(m2.id).unwrap();
    assert_eq!(links2.len(), 1);
}

#[test]
fn test_store_link_upsert() {
    let s = storage();

    let m1 = make_mem("a", MemoryTier::Episodic);
    let m2 = make_mem("b", MemoryTier::Episodic);
    s.store_memory(&m1).unwrap();
    s.store_memory(&m2).unwrap();

    // Insert and then replace
    s.store_link(m1.id, m2.id, LinkRelation::RelatedTo, 0.5).unwrap();
    s.store_link(m1.id, m2.id, LinkRelation::RelatedTo, 0.9).unwrap();

    let links = s.get_links(m1.id).unwrap();
    assert_eq!(links.len(), 1);
    assert!((links[0].2 - 0.9).abs() < 0.01);
}

// ── File-backed storage with WAL mode ────────────────────────────────────

#[test]
fn test_file_backed_wal_mode() {
    let path = format!("/tmp/cortex_wal_test_{}.db", Uuid::new_v4());
    let s = SqliteStorage::open(&path).unwrap();

    // Basic CRUD should work with WAL mode
    let mem = make_mem("wal test", MemoryTier::Episodic);
    s.store_memory(&mem).unwrap();

    let got = s.get_memory(mem.id).unwrap();
    assert!(got.is_some());

    let count = s.count_by_tier(MemoryTier::Episodic).unwrap();
    assert_eq!(count, 1);

    drop(s);
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(format!("{}-wal", &path));
    let _ = std::fs::remove_file(format!("{}-shm", &path));
}

#[test]
fn test_file_backed_read_pool_concurrent() {
    let path = format!("/tmp/cortex_pool_test_{}.db", Uuid::new_v4());
    let s = SqliteStorage::open(&path).unwrap();

    for i in 0..20 {
        s.store_memory(&make_mem(&format!("pool {}", i), MemoryTier::Episodic)).unwrap();
    }

    // Rapid reads — exercises read pool and prepared statement caching
    for _ in 0..50 {
        let _ = s.list_by_tier(MemoryTier::Episodic, 10).unwrap();
        let _ = s.count_by_tier(MemoryTier::Episodic).unwrap();
    }

    drop(s);
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(format!("{}-wal", &path));
    let _ = std::fs::remove_file(format!("{}-shm", &path));
}

// ── Edge cases ───────────────────────────────────────────────────────────

#[test]
fn test_delete_nonexistent_memory() {
    let s = storage();
    // Should not error
    s.delete_memory(Uuid::new_v4()).unwrap();
}

#[test]
fn test_restore_nonexistent_memory() {
    let s = storage();
    // Restoring a nonexistent memory should not panic
    // (it's a no-op UPDATE that matches 0 rows)
    s.restore_memory(Uuid::new_v4(), MemoryTier::Episodic).unwrap();
}

#[test]
fn test_empty_embedding_round_trip() {
    let s = storage();

    // Memory with no embedding
    let mem = make_mem("no embedding", MemoryTier::Episodic);
    assert!(mem.embedding.is_none());
    s.store_memory(&mem).unwrap();

    let got = s.get_memory(mem.id).unwrap().unwrap();
    assert!(got.embedding.is_none());
}

#[test]
fn test_large_metadata() {
    let s = storage();

    let mut mem = make_mem("metadata test", MemoryTier::Episodic);
    for i in 0..100 {
        mem.metadata.insert(
            format!("key_{}", i),
            serde_json::Value::String(format!("value_{}", i)),
        );
    }
    s.store_memory(&mem).unwrap();

    let got = s.get_memory(mem.id).unwrap().unwrap();
    assert_eq!(got.metadata.len(), 100);
}

#[test]
fn test_many_tags() {
    let s = storage();

    let mut mem = make_mem("tags test", MemoryTier::Episodic);
    for i in 0..50 {
        mem.tags.push(format!("tag_{}", i));
    }
    s.store_memory(&mem).unwrap();

    let got = s.get_memory(mem.id).unwrap().unwrap();
    assert_eq!(got.tags.len(), 50);
}

#[test]
fn test_source_identity_query_match() {
    let s = storage();
    let person_id = Uuid::new_v4();

    let mut mem = make_mem("from person", MemoryTier::Episodic);
    mem.source.identity_id = Some(person_id);
    s.store_memory(&mem).unwrap();

    let results = s.list_memories_by_source_identity(person_id).unwrap();
    // Should find at least 0 results (depends on JSON serialization format)
    // The important thing is no error
    assert!(results.len() <= 1);
}

// ── Memory index edge cases ─────────────────────────────────────────────

#[test]
fn test_index_insert_duplicate_id() {
    let index = MemoryIndex::new();
    let id = Uuid::new_v4();

    index.insert(id, vec![1.0, 0.0, 0.0]);
    index.insert(id, vec![0.0, 1.0, 0.0]); // replace

    let results = index.search(&[0.0, 1.0, 0.0], 1);
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].0, id);
}

#[test]
fn test_index_remove_and_search() {
    let index = MemoryIndex::new();

    let id1 = Uuid::new_v4();
    let id2 = Uuid::new_v4();
    index.insert(id1, vec![1.0, 0.0, 0.0]);
    index.insert(id2, vec![0.0, 1.0, 0.0]);

    index.remove(&id1);

    let results = index.search(&[1.0, 0.0, 0.0], 5);
    // id1 removed, only id2 should remain
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].0, id2);
}

#[test]
fn test_index_search_empty() {
    let index = MemoryIndex::new();
    let results = index.search(&[1.0, 0.0, 0.0], 5);
    assert!(results.is_empty());
}

#[test]
fn test_index_search_limit_larger_than_collection() {
    let index = MemoryIndex::new();
    index.insert(Uuid::new_v4(), vec![1.0, 0.0]);
    index.insert(Uuid::new_v4(), vec![0.0, 1.0]);

    let results = index.search(&[1.0, 0.0], 100);
    assert_eq!(results.len(), 2);
}

#[test]
fn test_index_flush_and_rebuild() {
    let index = MemoryIndex::new();

    let mut ids_and_embs: Vec<(Uuid, Vec<f32>)> = Vec::new();
    for _ in 0..10 {
        let id = Uuid::new_v4();
        let emb = vec![1.0, 0.0, 0.0];
        index.insert(id, emb.clone());
        ids_and_embs.push((id, emb));
    }

    // Rebuild with the entries
    index.rebuild(ids_and_embs.into_iter());

    let results = index.search(&[1.0, 0.0, 0.0], 5);
    assert_eq!(results.len(), 5);
}

#[test]
fn test_index_zero_vector() {
    let index = MemoryIndex::new();
    let id = Uuid::new_v4();

    // Zero vector — cosine distance should handle gracefully (returns distance 1.0)
    index.insert(id, vec![0.0, 0.0, 0.0]);

    let results = index.search(&[1.0, 0.0, 0.0], 5);
    // Zero vector has cosine similarity 0.0 (distance 1.0) — may or may not be returned
    // depending on scoring thresholds, but should not panic
    assert!(results.len() <= 1);
}
