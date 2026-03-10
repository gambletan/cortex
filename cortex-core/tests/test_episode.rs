use cortex_core::storage::memory_index::MemoryIndex;
use cortex_core::storage::sqlite::SqliteStorage;
use cortex_core::episode::EpisodeStore;
use cortex_core::types::*;

fn setup() -> (SqliteStorage, MemoryIndex) {
    let storage = SqliteStorage::open_in_memory().unwrap();
    let index = MemoryIndex::new();
    (storage, index)
}

#[test]
fn test_add_episode() {
    let (storage, index) = setup();
    let store = EpisodeStore::new(&storage, &index);

    let mem = store
        .add(
            MemContent::Text("User asked about the weather".to_string()),
            MemSource::new("telegram"),
            Some(0.6),
            None,
        )
        .unwrap();

    assert_eq!(mem.tier, MemoryTier::Episodic);
    assert!(matches!(mem.content, MemContent::Text(_)));
    assert_eq!(mem.source.channel, "telegram");
    assert!((mem.salience.base_score - 0.6).abs() < 0.01);
}

#[test]
fn test_add_with_embedding() {
    let (storage, index) = setup();
    let store = EpisodeStore::new(&storage, &index);

    let emb = vec![0.1, 0.2, 0.3, 0.4];
    let mem = store
        .add(
            MemContent::Text("Hello world".to_string()),
            MemSource::new("cli"),
            None,
            Some(emb.clone()),
        )
        .unwrap();

    assert!(mem.embedding.is_some());
    assert_eq!(index.len(), 1);
}

#[test]
fn test_query_by_embedding() {
    let (storage, index) = setup();
    let store = EpisodeStore::new(&storage, &index);

    let emb1 = vec![1.0, 0.0, 0.0];
    let emb2 = vec![0.0, 1.0, 0.0];
    let emb3 = vec![0.9, 0.1, 0.0];

    store.add(MemContent::Text("cats".to_string()), MemSource::new("a"), None, Some(emb1)).unwrap();
    store.add(MemContent::Text("dogs".to_string()), MemSource::new("a"), None, Some(emb2)).unwrap();
    store.add(MemContent::Text("kittens".to_string()), MemSource::new("a"), None, Some(emb3)).unwrap();

    let query = vec![1.0, 0.0, 0.0];
    let results = store.query(&query, 2, None).unwrap();
    assert_eq!(results.len(), 2);
    // First result should be "cats" (exact match)
    assert!(matches!(&results[0].content, MemContent::Text(t) if t == "cats"));
}

#[test]
fn test_get_recent() {
    let (storage, index) = setup();
    let store = EpisodeStore::new(&storage, &index);

    for i in 0..5 {
        store
            .add(
                MemContent::Text(format!("Memory {}", i)),
                MemSource::new("telegram"),
                None,
                None,
            )
            .unwrap();
    }

    let recent = store.get_recent(3, None).unwrap();
    assert_eq!(recent.len(), 3);
}

#[test]
fn test_get_recent_with_channel_filter() {
    let (storage, index) = setup();
    let store = EpisodeStore::new(&storage, &index);

    store.add(MemContent::Text("tg1".to_string()), MemSource::new("telegram"), None, None).unwrap();
    store.add(MemContent::Text("em1".to_string()), MemSource::new("email"), None, None).unwrap();
    store.add(MemContent::Text("tg2".to_string()), MemSource::new("telegram"), None, None).unwrap();

    let recent = store.get_recent(10, Some("telegram")).unwrap();
    assert_eq!(recent.len(), 2);
}

#[test]
fn test_get_decayed() {
    let (storage, index) = setup();
    let store = EpisodeStore::new(&storage, &index);

    // Add a memory with very low salience
    let mem = MemObjectBuilder::new(
        MemoryTier::Episodic,
        MemContent::Text("old stuff".to_string()),
        MemSource::new("cli"),
    )
    .salience(Salience {
        base_score: 0.01,
        emotional_weight: 1.0,
        access_boost: 1.0,
        decay_factor: 0.01,
        effective_score: 0.0001,
    })
    .build();

    use cortex_core::storage::traits::StorageBackend;
    storage.store_memory(&mem).unwrap();

    let decayed = store.get_decayed(0.1).unwrap();
    assert_eq!(decayed.len(), 1);
}

#[test]
fn test_touch_boosts_salience() {
    let (storage, index) = setup();
    let store = EpisodeStore::new(&storage, &index);

    let mem = store
        .add(MemContent::Text("important".to_string()), MemSource::new("cli"), Some(0.5), None)
        .unwrap();

    store.touch(mem.id).unwrap();

    use cortex_core::storage::traits::StorageBackend;
    let updated = storage.get_memory(mem.id).unwrap().unwrap();
    assert!(updated.temporal.access_count == 1);
}

#[test]
fn test_default_salience() {
    let (storage, index) = setup();
    let store = EpisodeStore::new(&storage, &index);

    let mem = store
        .add(MemContent::Text("test".to_string()), MemSource::new("cli"), None, None)
        .unwrap();

    assert!((mem.salience.base_score - 0.5).abs() < 0.01);
}
