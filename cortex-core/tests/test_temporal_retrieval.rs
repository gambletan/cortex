//! Tests for enhanced temporal retrieval (Phase 5).

use cortex_core::retrieval::{RetrievalEngine, RetrievalQuery};
use cortex_core::storage::memory_index::MemoryIndex;
use cortex_core::storage::sqlite::SqliteStorage;
use cortex_core::storage::traits::StorageBackend;
use cortex_core::types::*;
use chrono::{Duration, Utc};

fn setup() -> (SqliteStorage, MemoryIndex) {
    let storage = SqliteStorage::open_in_memory().unwrap();
    let index = MemoryIndex::new();
    (storage, index)
}

fn insert_at_age(storage: &SqliteStorage, index: &MemoryIndex, text: &str, hours_ago: i64) {
    let emb = vec![0.5, 0.5, 0.0]; // same embedding for all
    let mem = MemObjectBuilder::new(
        MemoryTier::Episodic,
        MemContent::Text(text.to_string()),
        MemSource::new("test"),
    )
    .embedding(emb.clone())
    .build();

    storage.store_memory(&mem).unwrap();
    index.insert(mem.id, emb);

    // Update timestamp
    let mut updated = storage.get_memory(mem.id).unwrap().unwrap();
    let old_time = Utc::now() - Duration::hours(hours_ago);
    updated.temporal.ingestion_time = old_time;
    updated.temporal.last_accessed = old_time;
    storage.update_memory(&updated).unwrap();
}

#[test]
fn test_recent_query_prefers_new() {
    let (storage, index) = setup();

    insert_at_age(&storage, &index, "old event", 720); // 30 days ago
    insert_at_age(&storage, &index, "recent event", 2);  // 2 hours ago

    let engine = RetrievalEngine::new(&storage, &index);
    let query = RetrievalQuery::new("What happened recently?", 2)
        .with_embedding(vec![0.5, 0.5, 0.0]);

    let results = engine.retrieve(&query).unwrap();
    assert_eq!(results.len(), 2);

    // Recent event should rank higher due to temporal intent
    let first_text = match &results[0].memory.content {
        MemContent::Text(t) => t.as_str(),
        _ => "",
    };
    assert_eq!(first_text, "recent event");
}

#[test]
fn test_first_query_prefers_old() {
    let (storage, index) = setup();

    insert_at_age(&storage, &index, "old event from start", 720);
    insert_at_age(&storage, &index, "new event", 2);

    let engine = RetrievalEngine::new(&storage, &index);
    let query = RetrievalQuery::new("When did I first mention this?", 2)
        .with_embedding(vec![0.5, 0.5, 0.0]);

    let results = engine.retrieve(&query).unwrap();
    assert_eq!(results.len(), 2);

    // Older event should rank higher for "first" queries
    let first_text = match &results[0].memory.content {
        MemContent::Text(t) => t.as_str(),
        _ => "",
    };
    assert_eq!(first_text, "old event from start");
}

#[test]
fn test_non_temporal_query_balanced() {
    let (storage, index) = setup();

    insert_at_age(&storage, &index, "old event", 720);
    insert_at_age(&storage, &index, "new event", 2);

    let engine = RetrievalEngine::new(&storage, &index);
    let query = RetrievalQuery::new("Tell me about events", 2)
        .with_embedding(vec![0.5, 0.5, 0.0]);

    let results = engine.retrieve(&query).unwrap();
    assert_eq!(results.len(), 2);
    // Both should have non-zero scores (balanced retrieval)
    assert!(results[0].score > 0.0);
    assert!(results[1].score > 0.0);
}

#[test]
fn test_chinese_temporal_recent() {
    let (storage, index) = setup();

    insert_at_age(&storage, &index, "old meeting", 720);
    insert_at_age(&storage, &index, "new meeting", 2);

    let engine = RetrievalEngine::new(&storage, &index);
    let query = RetrievalQuery::new("最近聊了什么", 2)
        .with_embedding(vec![0.5, 0.5, 0.0]);

    let results = engine.retrieve(&query).unwrap();
    assert_eq!(results.len(), 2);

    let first_text = match &results[0].memory.content {
        MemContent::Text(t) => t.as_str(),
        _ => "",
    };
    assert_eq!(first_text, "new meeting");
}

#[test]
fn test_chinese_temporal_first() {
    let (storage, index) = setup();

    insert_at_age(&storage, &index, "earliest discussion", 720);
    insert_at_age(&storage, &index, "latest discussion", 2);

    let engine = RetrievalEngine::new(&storage, &index);
    let query = RetrievalQuery::new("第一次讨论是什么时候", 2)
        .with_embedding(vec![0.5, 0.5, 0.0]);

    let results = engine.retrieve(&query).unwrap();
    assert_eq!(results.len(), 2);

    let first_text = match &results[0].memory.content {
        MemContent::Text(t) => t.as_str(),
        _ => "",
    };
    assert_eq!(first_text, "earliest discussion");
}
