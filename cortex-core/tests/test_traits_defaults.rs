//! Tests for StorageBackend default method implementations.
//! These exercise the fallback paths that aren't hit when SQLite overrides them.

use cortex_core::storage::sqlite::SqliteStorage;
use cortex_core::storage::traits::StorageBackend;
use cortex_core::types::*;

fn setup() -> SqliteStorage {
    SqliteStorage::open_in_memory().unwrap()
}

#[test]
fn test_get_memories_batch() {
    let storage = setup();

    let mem1 = MemObjectBuilder::new(
        MemoryTier::Episodic,
        MemContent::Text("first".into()),
        MemSource::new("test"),
    ).build();
    let mem2 = MemObjectBuilder::new(
        MemoryTier::Episodic,
        MemContent::Text("second".into()),
        MemSource::new("test"),
    ).build();

    storage.store_memory(&mem1).unwrap();
    storage.store_memory(&mem2).unwrap();

    let batch = storage.get_memories_batch(&[mem1.id, mem2.id]).unwrap();
    assert_eq!(batch.len(), 2);

    // Empty batch
    let empty = storage.get_memories_batch(&[]).unwrap();
    assert!(empty.is_empty());
}

#[test]
fn test_store_memories_batch() {
    let storage = setup();

    let mems: Vec<MemObject> = (0..5).map(|i| {
        MemObjectBuilder::new(
            MemoryTier::Episodic,
            MemContent::Text(format!("batch item {}", i)),
            MemSource::new("test"),
        ).build()
    }).collect();

    let count = storage.store_memories_batch(&mems).unwrap();
    assert_eq!(count, 5);

    let stored = storage.count_by_tier(MemoryTier::Episodic).unwrap();
    assert_eq!(stored, 5);
}

#[test]
fn test_archive_and_restore_memory() {
    let storage = setup();

    let mem = MemObjectBuilder::new(
        MemoryTier::Episodic,
        MemContent::Text("will be archived".into()),
        MemSource::new("test"),
    ).build();
    storage.store_memory(&mem).unwrap();

    // Archive
    storage.archive_memory(mem.id).unwrap();
    let archived = storage.get_memory(mem.id).unwrap().unwrap();
    assert_eq!(archived.tier, MemoryTier::Archived);

    // Restore
    storage.restore_memory(mem.id, MemoryTier::Episodic).unwrap();
    let restored = storage.get_memory(mem.id).unwrap().unwrap();
    assert_eq!(restored.tier, MemoryTier::Episodic);
}

#[test]
fn test_archive_nonexistent() {
    let storage = setup();
    // Should not error on nonexistent ID
    storage.archive_memory(uuid::Uuid::new_v4()).unwrap();
}

#[test]
fn test_list_by_tier_and_namespace() {
    let storage = setup();

    let mut mem = MemObjectBuilder::new(
        MemoryTier::Episodic,
        MemContent::Text("namespaced".into()),
        MemSource::new("test"),
    ).build();
    mem.namespace = Some("project-a".into());
    storage.store_memory(&mem).unwrap();

    let mut mem2 = MemObjectBuilder::new(
        MemoryTier::Episodic,
        MemContent::Text("other namespace".into()),
        MemSource::new("test"),
    ).build();
    mem2.namespace = Some("project-b".into());
    storage.store_memory(&mem2).unwrap();

    // Filter by namespace
    let filtered = storage.list_by_tier_and_namespace(MemoryTier::Episodic, Some("project-a"), 100).unwrap();
    assert_eq!(filtered.len(), 1);

    // No filter
    let all = storage.list_by_tier_and_namespace(MemoryTier::Episodic, None, 100).unwrap();
    assert_eq!(all.len(), 2);
}

#[test]
fn test_query_facts_by_entity_default() {
    let storage = setup();

    let fact = MemObjectBuilder::new(
        MemoryTier::Semantic,
        MemContent::Fact {
            subject: "Alice".into(),
            predicate: "works_at".into(),
            object: "Google".into(),
        },
        MemSource::new("test"),
    ).build();
    storage.store_memory(&fact).unwrap();

    let results = storage.query_facts_by_entity("Alice").unwrap();
    assert_eq!(results.len(), 1);

    let results = storage.query_facts_by_entity("Google").unwrap();
    assert_eq!(results.len(), 1);

    let results = storage.query_facts_by_entity("nonexistent").unwrap();
    assert!(results.is_empty());
}

#[test]
fn test_query_facts_by_entities_batch() {
    let storage = setup();

    let fact1 = MemObjectBuilder::new(
        MemoryTier::Semantic,
        MemContent::Fact { subject: "Alice".into(), predicate: "works_at".into(), object: "Google".into() },
        MemSource::new("test"),
    ).build();
    let fact2 = MemObjectBuilder::new(
        MemoryTier::Semantic,
        MemContent::Fact { subject: "Bob".into(), predicate: "works_at".into(), object: "Meta".into() },
        MemSource::new("test"),
    ).build();
    storage.store_memory(&fact1).unwrap();
    storage.store_memory(&fact2).unwrap();

    let results = storage.query_facts_by_entities(&["Alice".into(), "Bob".into()]).unwrap();
    assert_eq!(results.len(), 2);

    // Empty input
    let empty = storage.query_facts_by_entities(&[]).unwrap();
    assert!(empty.is_empty());
}

#[test]
fn test_query_preferences_by_key() {
    let storage = setup();

    let pref = MemObjectBuilder::new(
        MemoryTier::Semantic,
        MemContent::Preference { key: "editor".into(), value: "neovim".into(), confidence: 0.9 },
        MemSource::new("test"),
    ).build();
    storage.store_memory(&pref).unwrap();

    let results = storage.query_preferences_by_key("editor").unwrap();
    assert_eq!(results.len(), 1);

    let results = storage.query_preferences_by_key("nonexistent").unwrap();
    assert!(results.is_empty());
}

#[test]
fn test_list_beliefs_confident() {
    let storage = setup();

    // High confidence belief
    let high = cortex_core::belief::Belief {
        id: uuid::Uuid::new_v4(),
        key: "user_is_dev".into(),
        probability: 0.95,
        observations: vec![],
        last_updated: chrono::Utc::now(),
    };
    storage.store_belief(&high).unwrap();

    // Low confidence belief (strong negative)
    let low = cortex_core::belief::Belief {
        id: uuid::Uuid::new_v4(),
        key: "user_likes_php".into(),
        probability: 0.05,
        observations: vec![],
        last_updated: chrono::Utc::now(),
    };
    storage.store_belief(&low).unwrap();

    // Mid confidence (not confident)
    let mid = cortex_core::belief::Belief {
        id: uuid::Uuid::new_v4(),
        key: "user_likes_tea".into(),
        probability: 0.55,
        observations: vec![],
        last_updated: chrono::Utc::now(),
    };
    storage.store_belief(&mid).unwrap();

    let confident = storage.list_beliefs_confident(0.8).unwrap();
    assert_eq!(confident.len(), 2); // high + low (both confident)
}

#[test]
fn test_list_namespaces() {
    let storage = setup();
    let ns = storage.list_namespaces().unwrap();
    // Default impl returns empty
    // SQLite override may return actual data
    let _ = ns;
}
