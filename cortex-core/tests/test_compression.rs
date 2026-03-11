use chrono::{Duration, Utc};
use cortex_core::compression::CompressionEngine;
use cortex_core::storage::memory_index::MemoryIndex;
use cortex_core::storage::sqlite::SqliteStorage;
use cortex_core::storage::traits::StorageBackend;
use cortex_core::types::*;

fn setup() -> (SqliteStorage, MemoryIndex) {
    let storage = SqliteStorage::open_in_memory().unwrap();
    let index = MemoryIndex::new();
    (storage, index)
}

fn insert_old_memory(storage: &SqliteStorage, text: &str, channel: &str, hours_ago: i64) {
    let mem = MemObjectBuilder::new(
        MemoryTier::Episodic,
        MemContent::Text(text.to_string()),
        MemSource::new(channel),
    )
    .build();

    // Store first, then update timestamp to simulate old memory
    storage.store_memory(&mem).unwrap();
    let mut updated = storage.get_memory(mem.id).unwrap().unwrap();
    let old_time = Utc::now() - Duration::hours(hours_ago);
    updated.temporal.ingestion_time = old_time;
    updated.temporal.last_accessed = old_time;
    storage.update_memory(&updated).unwrap();
}

#[test]
fn test_find_compressible_sessions() {
    let (storage, index) = setup();
    let engine = CompressionEngine::new(&storage, &index);

    // Insert 5 old messages (same channel, same day)
    for i in 0..5 {
        insert_old_memory(&storage, &format!("message {}", i), "telegram", 48 + i);
    }

    let sessions = engine
        .find_compressible_sessions(3, Duration::hours(24))
        .unwrap();
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].memories.len(), 5);
    assert_eq!(sessions[0].channel, "telegram");
}

#[test]
fn test_no_compression_for_recent() {
    let (storage, index) = setup();
    let engine = CompressionEngine::new(&storage, &index);

    // Insert recent messages (only 2 hours old)
    for i in 0..5 {
        insert_old_memory(&storage, &format!("message {}", i), "test", 2);
    }

    // max_age = 24h, so 2h-old messages should NOT be compressed
    let sessions = engine
        .find_compressible_sessions(3, Duration::hours(24))
        .unwrap();
    assert!(sessions.is_empty());
}

#[test]
fn test_no_compression_below_threshold() {
    let (storage, index) = setup();
    let engine = CompressionEngine::new(&storage, &index);

    // Only 2 messages (below min_messages=3)
    for i in 0..2 {
        insert_old_memory(&storage, &format!("message {}", i), "test", 48);
    }

    let sessions = engine
        .find_compressible_sessions(3, Duration::hours(24))
        .unwrap();
    assert!(sessions.is_empty());
}

#[test]
fn test_compress_session() {
    let (storage, index) = setup();
    let engine = CompressionEngine::new(&storage, &index);

    for i in 0..5 {
        insert_old_memory(
            &storage,
            &format!("Discussed topic {} in the meeting", i),
            "slack",
            72,
        );
    }

    let sessions = engine
        .find_compressible_sessions(3, Duration::hours(24))
        .unwrap();
    assert_eq!(sessions.len(), 1);

    let before_count = storage
        .list_by_tier(MemoryTier::Episodic, 1000)
        .unwrap()
        .len();
    assert_eq!(before_count, 5);

    let summary = engine.compress_session(&sessions[0]).unwrap();

    // After compression: 5 originals deleted, 1 summary created
    let after_count = storage
        .list_by_tier(MemoryTier::Episodic, 1000)
        .unwrap()
        .len();
    assert_eq!(after_count, 1);

    // Summary should contain compressed marker
    if let MemContent::Text(ref text) = summary.content {
        assert!(text.contains("[Compressed: 5 messages]"));
    } else {
        panic!("Expected text content");
    }

    // Should have metadata about compression
    assert!(summary.metadata.contains_key("compressed_from"));
}

#[test]
fn test_run_compression() {
    let (storage, index) = setup();
    let engine = CompressionEngine::new(&storage, &index);

    // Two separate channels
    for i in 0..4 {
        insert_old_memory(&storage, &format!("slack msg {}", i), "slack", 72);
    }
    for i in 0..6 {
        insert_old_memory(&storage, &format!("tg msg {}", i), "telegram", 72);
    }

    let report = engine
        .run_compression(3, Duration::hours(24))
        .unwrap();

    assert_eq!(report.sessions_compressed, 2);
    assert_eq!(report.episodes_consumed, 10);
    assert_eq!(report.summaries_created, 2);
}

#[test]
fn test_compression_preserves_max_salience() {
    let (storage, index) = setup();
    let engine = CompressionEngine::new(&storage, &index);

    // Insert with varying salience
    for (i, sal) in [0.3, 0.9, 0.5].iter().enumerate() {
        let mem = MemObjectBuilder::new(
            MemoryTier::Episodic,
            MemContent::Text(format!("message {}", i)),
            MemSource::new("test"),
        )
        .salience(Salience::new(*sal))
        .build();
        storage.store_memory(&mem).unwrap();

        let mut updated = storage.get_memory(mem.id).unwrap().unwrap();
        let old_time = Utc::now() - Duration::hours(48);
        updated.temporal.ingestion_time = old_time;
        updated.temporal.last_accessed = old_time;
        storage.update_memory(&updated).unwrap();
    }

    let sessions = engine
        .find_compressible_sessions(3, Duration::hours(24))
        .unwrap();
    let summary = engine.compress_session(&sessions[0]).unwrap();

    // Summary salience should be at least as high as the max original (0.9)
    assert!(summary.salience.base_score >= 0.9);
}

#[test]
fn test_compression_deduplicates_content() {
    let (storage, index) = setup();
    let engine = CompressionEngine::new(&storage, &index);

    // Insert duplicate messages
    for _ in 0..5 {
        insert_old_memory(&storage, "same message repeated", "test", 72);
    }

    let sessions = engine
        .find_compressible_sessions(3, Duration::hours(24))
        .unwrap();
    let summary = engine.compress_session(&sessions[0]).unwrap();

    // Deduplication should reduce content
    if let MemContent::Text(ref text) = summary.content {
        // Should only contain the message once (after dedup)
        let count = text.matches("same message repeated").count();
        assert_eq!(count, 1, "Duplicate messages should be deduplicated");
    }
}
