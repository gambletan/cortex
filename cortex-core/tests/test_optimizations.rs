//! Tests for batch ingest, deduplication, archival, metrics, and events.

use cortex_core::types::*;
use cortex_core::Cortex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

#[test]
fn test_batch_ingest_basic() {
    let cortex = Cortex::in_memory().unwrap();
    let items: Vec<BatchIngestItem> = (0..10)
        .map(|i| BatchIngestItem {
            text: format!("batch memory {}", i),
            channel: "test".into(),
            user_id: None,
            salience_hint: Some(0.6),
            embedding: None,
            namespace: None,
        })
        .collect();

    let report = cortex.ingest_batch(items).unwrap();
    assert_eq!(report.total_submitted, 10);
    assert_eq!(report.stored, 10);
    assert_eq!(report.deduplicated, 0);

    let stats = cortex.stats().unwrap();
    assert_eq!(stats.episodic, 10);
}

#[test]
fn test_batch_ingest_dedup() {
    let cortex = Cortex::in_memory().unwrap();

    // First batch
    let items: Vec<BatchIngestItem> = vec![
        BatchIngestItem {
            text: "I love rust".into(),
            channel: "test".into(),
            user_id: None,
            salience_hint: None,
            embedding: None,
            namespace: None,
        },
        BatchIngestItem {
            text: "I love rust".into(), // exact duplicate within batch
            channel: "test".into(),
            user_id: None,
            salience_hint: None,
            embedding: None,
            namespace: None,
        },
    ];

    let report = cortex.ingest_batch(items).unwrap();
    assert_eq!(report.stored, 1);
    assert_eq!(report.deduplicated, 1);

    // Second batch with same content
    let items2 = vec![BatchIngestItem {
        text: "I love rust".into(), // dupe from first batch
        channel: "test".into(),
        user_id: None,
        salience_hint: None,
        embedding: None,
        namespace: None,
    }];

    let report2 = cortex.ingest_batch(items2).unwrap();
    assert_eq!(report2.stored, 0);
    assert_eq!(report2.deduplicated, 1);
}

#[test]
fn test_single_ingest_dedup() {
    let cortex = Cortex::in_memory().unwrap();

    let mem1 = cortex.ingest("hello world", "test", None, None, None).unwrap();
    assert_eq!(mem1.tier.as_str(), "episodic");

    // Same content should be deduped
    let mem2 = cortex.ingest("hello world", "test", None, None, None).unwrap();
    // mem2 is returned but not stored (dedup)
    assert!(mem2.content_hash.is_some());

    let stats = cortex.stats().unwrap();
    // Only 1 episodic memory should exist (semantic facts may also be created from inference)
    assert_eq!(stats.episodic, 1);
}

#[test]
fn test_content_hash_computation() {
    let hash1 = compute_content_hash(&MemContent::Text("hello".into()));
    let hash2 = compute_content_hash(&MemContent::Text("hello".into()));
    let hash3 = compute_content_hash(&MemContent::Text("world".into()));

    assert_eq!(hash1, hash2);
    assert_ne!(hash1, hash3);
    assert_eq!(hash1.len(), 64); // SHA-256 hex
}

#[test]
fn test_archival_and_restore() {
    let cortex = Cortex::in_memory().unwrap();

    let mem = cortex.ingest("archive me", "test", None, Some(0.1), None).unwrap();
    let id = mem.id;

    // Archive
    cortex.archive_memory(id).unwrap();

    let stats = cortex.stats().unwrap();
    assert_eq!(stats.episodic, 0);
    assert_eq!(stats.archived, 1);

    // Restore
    cortex.restore_memory(id, MemoryTier::Episodic).unwrap();

    let stats2 = cortex.stats().unwrap();
    assert_eq!(stats2.episodic, 1);
    assert_eq!(stats2.archived, 0);
}

#[test]
fn test_near_dedup_opt_in_reinforces_and_distinct_kept() {
    // Near-dedup is opt-in; uses explicit embeddings so the test is deterministic and
    // never touches the network/embedder.
    let cortex = Cortex::in_memory().unwrap().with_dedup_config(DeduplicationConfig {
        exact_dedup: true,
        near_dedup: true,
        near_dedup_threshold: 0.95,
    });

    cortex
        .ingest("I prefer dark mode in my editor", "test", None, None, Some(vec![1.0, 0.0, 0.0]))
        .unwrap();
    // Near-identical embedding (cosine 1.0 ≥ 0.95) → reinforced, not stored as a copy.
    cortex
        .ingest("dark mode is my editor preference", "test", None, None, Some(vec![1.0, 0.0, 0.0]))
        .unwrap();
    // Distinct embedding (orthogonal) → kept as a separate memory.
    cortex
        .ingest("I live in Shanghai", "test", None, None, Some(vec![0.0, 1.0, 0.0]))
        .unwrap();

    let stats = cortex.stats().unwrap();
    assert_eq!(stats.episodic, 2, "near-dup merged, distinct kept: {stats:?}");
    assert!(cortex.metrics().dedup_hits >= 1, "near-dup should count a dedup hit");
}

#[test]
fn test_metrics_tracking() {
    let cortex = Cortex::in_memory().unwrap();

    cortex.ingest("metric test 1", "test", None, None, None).unwrap();
    cortex.ingest("metric test 2", "test", None, None, None).unwrap();

    let m = cortex.metrics();
    assert_eq!(m.ingests, 2);

    let _ = cortex.retrieve("test", 5, None, None, None);
    let m2 = cortex.metrics();
    assert_eq!(m2.retrievals, 1);
}

#[test]
fn test_event_bus() {
    let cortex = Cortex::in_memory().unwrap();
    let event_count = Arc::new(AtomicUsize::new(0));
    let count_clone = event_count.clone();

    cortex.on_event(Box::new(move |_event| {
        count_clone.fetch_add(1, Ordering::Relaxed);
    }));

    cortex.ingest("event test", "test", None, None, None).unwrap();

    // At least 1 event should have been emitted (MemoryCreated)
    assert!(event_count.load(Ordering::Relaxed) >= 1);
}

#[test]
fn test_namespace_isolation() {
    let cortex = Cortex::in_memory().unwrap();

    // Ingest with namespace via batch
    let items = vec![
        BatchIngestItem {
            text: "ns-a memory".into(),
            channel: "test".into(),
            user_id: None,
            salience_hint: None,
            embedding: None,
            namespace: Some("team-a".into()),
        },
        BatchIngestItem {
            text: "ns-b memory".into(),
            channel: "test".into(),
            user_id: None,
            salience_hint: None,
            embedding: None,
            namespace: Some("team-b".into()),
        },
    ];

    let report = cortex.ingest_batch(items).unwrap();
    assert_eq!(report.stored, 2);

    // Query with namespace filter
    let all = cortex.storage().list_by_tier_and_namespace(
        MemoryTier::Episodic, None, 100
    ).unwrap();
    assert_eq!(all.len(), 2);

    let team_a = cortex.storage().list_by_tier_and_namespace(
        MemoryTier::Episodic, Some("team-a"), 100
    ).unwrap();
    assert_eq!(team_a.len(), 1);
    assert_eq!(team_a[0].namespace.as_deref(), Some("team-a"));
}

#[test]
fn test_memory_tier_archived() {
    assert_eq!(MemoryTier::Archived.as_str(), "archived");
    assert_eq!(MemoryTier::parse("archived"), Some(MemoryTier::Archived));
}
