//! Tests for LRU retrieval cache, read-write connection separation,
//! Arc<MemObject> in retrieval results, and prepared statement caching.

use cortex_core::types::*;
use cortex_core::Cortex;

// ── LRU Cache: hit / miss / invalidation ─────────────────────────────────

#[test]
fn test_retrieval_cache_hit() {
    let cortex = Cortex::in_memory().unwrap();

    cortex.ingest("cache test data", "cli", None, None, None).unwrap();

    // First call: cache miss, populates cache
    let r1 = cortex.retrieve("cache test", 5, None, None, None).unwrap();
    // Second call: same query → cache hit
    let r2 = cortex.retrieve("cache test", 5, None, None, None).unwrap();

    assert_eq!(r1.len(), r2.len());
    assert_eq!(cortex.metrics().retrievals, 2);
}

#[test]
fn test_retrieval_cache_invalidated_on_ingest() {
    let cortex = Cortex::in_memory().unwrap();

    cortex.ingest("original data", "cli", None, None, None).unwrap();

    let r1 = cortex.retrieve("data", 10, None, None, None).unwrap();
    let count_before = r1.len();

    // Ingest new data → cache should be invalidated
    cortex
        .ingest("new additional data", "cli", None, None, None)
        .unwrap();

    let r2 = cortex.retrieve("data", 10, None, None, None).unwrap();
    // After ingest, results may differ (new memory available)
    assert!(r2.len() >= count_before);
}

#[test]
fn test_retrieval_cache_invalidated_on_batch_ingest() {
    let cortex = Cortex::in_memory().unwrap();

    cortex.ingest("base data", "cli", None, None, None).unwrap();
    let _ = cortex.retrieve("base", 5, None, None, None).unwrap();

    let items = vec![BatchIngestItem {
        text: "batch cache test".into(),
        channel: "cli".into(),
        privacy: None,
        user_id: None,
        salience_hint: None,
        embedding: None,
        namespace: None,
    }];
    cortex.ingest_batch(items).unwrap();

    // Cache was invalidated, this is a fresh query
    let r = cortex.retrieve("base", 5, None, None, None).unwrap();
    assert!(!r.is_empty());
}

#[test]
fn test_retrieval_cache_invalidated_on_consolidation() {
    let cortex = Cortex::in_memory().unwrap();
    cortex.ingest("consolidate me", "cli", None, None, None).unwrap();
    let _ = cortex.retrieve("consolidate", 5, None, None, None).unwrap();

    cortex.run_consolidation().unwrap();

    // Should not return stale results
    let r = cortex.retrieve("consolidate", 5, None, None, None).unwrap();
    assert!(!r.is_empty());
}

#[test]
fn test_retrieval_cache_invalidated_on_decay() {
    let cortex = Cortex::in_memory().unwrap();
    cortex.ingest("decay test", "cli", None, None, None).unwrap();
    let _ = cortex.retrieve("decay", 5, None, None, None).unwrap();

    cortex.run_decay().unwrap();

    let r = cortex.retrieve("decay", 5, None, None, None).unwrap();
    assert!(!r.is_empty());
}

#[test]
fn test_retrieval_cache_invalidated_on_archive() {
    let cortex = Cortex::in_memory().unwrap();

    let mem = cortex.ingest("archive cache test", "cli", None, None, None).unwrap();
    let _ = cortex.retrieve("archive", 5, None, None, None).unwrap();

    cortex.archive_memory(mem.id).unwrap();

    // After archiving, the memory should not appear in results
    let r = cortex.retrieve("archive", 5, None, None, None).unwrap();
    // Archived memories are filtered out
    for result in &r {
        assert_ne!(result.memory.id, mem.id);
    }
}

#[test]
fn test_retrieval_cache_invalidated_on_restore() {
    let cortex = Cortex::in_memory().unwrap();

    let mem = cortex.ingest("restore cache test", "cli", None, None, None).unwrap();
    cortex.archive_memory(mem.id).unwrap();

    let _ = cortex.retrieve("restore", 5, None, None, None).unwrap();

    cortex.restore_memory(mem.id, MemoryTier::Episodic).unwrap();

    let r = cortex.retrieve("restore", 5, None, None, None).unwrap();
    assert!(!r.is_empty());
}

#[test]
fn test_cache_key_differs_by_params() {
    let cortex = Cortex::in_memory().unwrap();
    cortex.ingest("param test", "cli", None, None, None).unwrap();

    // Different limit → different cache key
    let r1 = cortex.retrieve("param", 1, None, None, None).unwrap();
    let r2 = cortex.retrieve("param", 10, None, None, None).unwrap();
    // r1 is limited to 1, r2 to 10
    assert!(r1.len() <= 1);
    assert!(r2.len() <= 10);

    // Different channel → different cache key
    let r3 = cortex.retrieve("param", 5, Some("cli"), None, None).unwrap();
    let r4 = cortex.retrieve("param", 5, Some("other"), None, None).unwrap();
    // r3 filters by channel, r4 uses a different channel
    let _ = (r3, r4); // just verify no crash
}

// ── Arc<MemObject>: verify result cloning and field access ───────────────

#[test]
fn test_retrieval_result_arc_clone() {
    let cortex = Cortex::in_memory().unwrap();
    cortex.ingest("arc clone test", "cli", None, None, None).unwrap();

    let results = cortex.retrieve("arc clone", 5, None, None, None).unwrap();
    assert!(!results.is_empty());

    // Clone the result (uses Arc::clone internally)
    let cloned = results[0].clone();
    assert_eq!(cloned.memory.id, results[0].memory.id);
    assert_eq!(cloned.score, results[0].score);

    // Verify we can access fields through Arc
    if let MemContent::Text(t) = &cloned.memory.content {
        assert!(!t.is_empty());
    }
}

// ── retrieve_with_namespace ──────────────────────────────────────────────

#[test]
fn test_retrieve_with_namespace_filtering() {
    let cortex = Cortex::in_memory().unwrap();

    // Ingest with different namespaces
    let items = vec![
        BatchIngestItem {
            text: "team alpha work".into(),
            channel: "cli".into(),
            privacy: None,
            user_id: None,
            salience_hint: None,
            embedding: None,
            namespace: Some("alpha".into()),
        },
        BatchIngestItem {
            text: "team beta work".into(),
            channel: "cli".into(),
            privacy: None,
            user_id: None,
            salience_hint: None,
            embedding: None,
            namespace: Some("beta".into()),
        },
    ];
    cortex.ingest_batch(items).unwrap();

    // Retrieve with namespace filter
    let alpha = cortex
        .retrieve_with_namespace("work", 10, None, None, None, Some("alpha"))
        .unwrap();
    for r in &alpha {
        assert_eq!(r.memory.namespace.as_deref(), Some("alpha"));
    }

    let beta = cortex
        .retrieve_with_namespace("work", 10, None, None, None, Some("beta"))
        .unwrap();
    for r in &beta {
        assert_eq!(r.memory.namespace.as_deref(), Some("beta"));
    }
}

#[test]
fn test_retrieve_without_namespace_returns_all() {
    let cortex = Cortex::in_memory().unwrap();

    let items = vec![
        BatchIngestItem {
            text: "namespaced content".into(),
            channel: "cli".into(),
            privacy: None,
            user_id: None,
            salience_hint: None,
            embedding: None,
            namespace: Some("ns1".into()),
        },
        BatchIngestItem {
            text: "global content".into(),
            channel: "cli".into(),
            privacy: None,
            user_id: None,
            salience_hint: None,
            embedding: None,
            namespace: None,
        },
    ];
    cortex.ingest_batch(items).unwrap();

    // Without namespace, should return both
    let all = cortex.retrieve("content", 10, None, None, None).unwrap();
    assert!(all.len() >= 2);
}

// ── File-backed storage: read pool and WAL ───────────────────────────────

#[test]
fn test_file_backed_storage_read_pool() {
    let path = format!("/tmp/cortex_read_pool_{}.db", uuid::Uuid::new_v4());
    let cortex = Cortex::open(&path).unwrap();

    // Ingest and retrieve — exercises read pool
    cortex.ingest("file backed test", "cli", None, None, None).unwrap();
    let results = cortex.retrieve("file", 5, None, None, None).unwrap();
    assert!(!results.is_empty());

    // Stats uses read connection
    let stats = cortex.stats().unwrap();
    assert_eq!(stats.episodic, 1);

    drop(cortex);
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(format!("{}-wal", &path));
    let _ = std::fs::remove_file(format!("{}-shm", &path));
}

#[test]
fn test_file_backed_concurrent_reads() {
    let path = format!("/tmp/cortex_concurrent_{}.db", uuid::Uuid::new_v4());
    let cortex = Cortex::open(&path).unwrap();

    for i in 0..20 {
        cortex
            .ingest(&format!("concurrent memory {}", i), "cli", None, None, None)
            .unwrap();
    }

    // Multiple reads in sequence — exercises read pool rotation
    for _ in 0..10 {
        let _ = cortex.retrieve("concurrent", 5, None, None, None).unwrap();
        let _ = cortex.stats().unwrap();
    }

    drop(cortex);
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(format!("{}-wal", &path));
    let _ = std::fs::remove_file(format!("{}-shm", &path));
}
