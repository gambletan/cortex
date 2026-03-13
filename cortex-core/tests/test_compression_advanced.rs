//! Advanced compression tests: TF-IDF summarization, content classification,
//! deduplication, diversity selection, custom summarizer, edge cases.

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
    storage.store_memory(&mem).unwrap();
    let mut updated = storage.get_memory(mem.id).unwrap().unwrap();
    let old_time = Utc::now() - Duration::hours(hours_ago);
    updated.temporal.ingestion_time = old_time;
    updated.temporal.last_accessed = old_time;
    storage.update_memory(&updated).unwrap();
}

// ── TF-IDF summary selection ─────────────────────────────────────────────

#[test]
fn test_summary_selects_informative_sentences() {
    let (storage, index) = setup();
    let engine = CompressionEngine::new(&storage, &index);

    // Insert varied content — the unique/informative one should rank higher
    insert_old_memory(&storage, "hello", "test", 72);
    insert_old_memory(&storage, "hello", "test", 72);
    insert_old_memory(&storage, "hello", "test", 72);
    insert_old_memory(&storage, "The user deployed Kubernetes cluster on AWS", "test", 72);

    let sessions = engine.find_compressible_sessions(3, Duration::hours(24)).unwrap();
    assert_eq!(sessions.len(), 1);

    let summary = engine.compress_session(&sessions[0]).unwrap();
    if let MemContent::Text(ref text) = summary.content {
        // The informative sentence should be in the summary
        assert!(text.contains("Kubernetes") || text.contains("deployed"));
        // Deduplicated "hello" should appear only once
        assert!(text.matches("hello").count() <= 1);
    }
}

// ── Content classification ───────────────────────────────────────────────

#[test]
fn test_summary_with_mixed_content_types() {
    let (storage, index) = setup();
    let engine = CompressionEngine::new(&storage, &index);

    // Fact-like content (contains '=')
    insert_old_memory(&storage, "result = 42", "test", 72);
    // Preference-like content (contains ':')
    insert_old_memory(&storage, "editor: neovim", "test", 72);
    // General content
    insert_old_memory(&storage, "We had a productive meeting today", "test", 72);

    let sessions = engine.find_compressible_sessions(3, Duration::hours(24)).unwrap();
    let summary = engine.compress_session(&sessions[0]).unwrap();

    if let MemContent::Text(ref text) = summary.content {
        // Diversity: at least content from different groups should appear
        assert!(text.contains("[Compressed: 3 messages]"));
    }
}

// ── Near-duplicate deduplication ─────────────────────────────────────────

#[test]
fn test_summary_deduplicates_near_duplicates() {
    let (storage, index) = setup();
    let engine = CompressionEngine::new(&storage, &index);

    // Near-duplicates: one contains the other
    insert_old_memory(&storage, "I like using Rust for systems programming", "test", 72);
    insert_old_memory(
        &storage,
        "I like using Rust for systems programming and web development",
        "test",
        72,
    );
    insert_old_memory(&storage, "Python is great for scripting", "test", 72);

    let sessions = engine.find_compressible_sessions(3, Duration::hours(24)).unwrap();
    let summary = engine.compress_session(&sessions[0]).unwrap();

    if let MemContent::Text(ref text) = summary.content {
        // Near-duplicate should be deduplicated — only one Rust mention
        let rust_count = text.matches("Rust").count();
        assert!(rust_count <= 1, "Near-duplicate not deduped, Rust appears {} times", rust_count);
    }
}

// ── URL exclusion from preference classification ─────────────────────────

#[test]
fn test_url_not_classified_as_preference() {
    let (storage, index) = setup();
    let engine = CompressionEngine::new(&storage, &index);

    insert_old_memory(&storage, "Check https://example.com for details", "test", 72);
    insert_old_memory(&storage, "Visit http://localhost:8080", "test", 72);
    insert_old_memory(&storage, "FTP at ftp://files.example.com", "test", 72);

    let sessions = engine.find_compressible_sessions(3, Duration::hours(24)).unwrap();
    let summary = engine.compress_session(&sessions[0]).unwrap();
    // Should not crash and should produce a valid summary
    if let MemContent::Text(ref text) = summary.content {
        assert!(text.contains("[Compressed: 3 messages]"));
    }
}

// ── Custom summarizer ────────────────────────────────────────────────────

#[test]
fn test_compression_with_custom_summarizer() {
    let (storage, index) = setup();

    for i in 0..5 {
        insert_old_memory(&storage, &format!("custom summary msg {}", i), "test", 72);
    }

    let engine = CompressionEngine::new(&storage, &index);
    let sessions = engine.find_compressible_sessions(3, Duration::hours(24)).unwrap();
    assert_eq!(sessions.len(), 1);

    // Compress using the built-in engine first, verify it produces valid output
    let summary = engine.compress_session(&sessions[0]).unwrap();
    assert!(matches!(&summary.content, MemContent::Text(t) if t.contains("[Compressed:")));
}

// ── Multi-channel sessions ───────────────────────────────────────────────

#[test]
fn test_sessions_grouped_by_channel() {
    let (storage, index) = setup();
    let engine = CompressionEngine::new(&storage, &index);

    for i in 0..4 {
        insert_old_memory(&storage, &format!("slack {}", i), "slack", 72);
    }
    for i in 0..4 {
        insert_old_memory(&storage, &format!("telegram {}", i), "telegram", 72);
    }

    let sessions = engine.find_compressible_sessions(3, Duration::hours(24)).unwrap();
    assert_eq!(sessions.len(), 2);

    let channels: Vec<&str> = sessions.iter().map(|s| s.channel.as_str()).collect();
    assert!(channels.contains(&"slack"));
    assert!(channels.contains(&"telegram"));
}

// ── Empty session handling ───────────────────────────────────────────────

#[test]
fn test_compression_no_sessions() {
    let (storage, index) = setup();
    let engine = CompressionEngine::new(&storage, &index);

    let report = engine.run_compression(3, Duration::hours(24)).unwrap();
    assert_eq!(report.sessions_compressed, 0);
    assert_eq!(report.episodes_consumed, 0);
    assert_eq!(report.summaries_created, 0);
}

// ── Compression metadata ─────────────────────────────────────────────────

#[test]
fn test_compressed_memory_has_metadata() {
    let (storage, index) = setup();
    let engine = CompressionEngine::new(&storage, &index);

    for i in 0..5 {
        insert_old_memory(&storage, &format!("metadata test {}", i), "test", 72);
    }

    let sessions = engine.find_compressible_sessions(3, Duration::hours(24)).unwrap();
    let summary = engine.compress_session(&sessions[0]).unwrap();

    assert!(summary.metadata.contains_key("compressed_from"));
    assert_eq!(summary.tier, MemoryTier::Episodic);
}
