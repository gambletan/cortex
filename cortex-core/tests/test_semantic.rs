use cortex_core::semantic::SemanticStore;
use cortex_core::storage::memory_index::MemoryIndex;
use cortex_core::storage::sqlite::SqliteStorage;
use cortex_core::types::*;

fn setup() -> (SqliteStorage, MemoryIndex) {
    let storage = SqliteStorage::open_in_memory().unwrap();
    let index = MemoryIndex::new();
    (storage, index)
}

#[test]
fn test_add_fact() {
    let (storage, index) = setup();
    let store = SemanticStore::new(&storage, &index);

    let mem = store
        .add_fact("Rust", "is", "a systems language", 0.95, MemSource::new("cli"), None)
        .unwrap();

    assert_eq!(mem.tier, MemoryTier::Semantic);
    match &mem.content {
        MemContent::Fact { subject, predicate, object } => {
            assert_eq!(subject, "Rust");
            assert_eq!(predicate, "is");
            assert_eq!(object, "a systems language");
        }
        _ => panic!("Expected Fact content"),
    }
}

#[test]
fn test_add_preference() {
    let (storage, index) = setup();
    let store = SemanticStore::new(&storage, &index);

    let mem = store.add_preference("editor", "neovim", 0.9).unwrap();

    match &mem.content {
        MemContent::Preference { key, value, confidence } => {
            assert_eq!(key, "editor");
            assert_eq!(value, "neovim");
            assert!((*confidence - 0.9).abs() < 0.01);
        }
        _ => panic!("Expected Preference content"),
    }
}

#[test]
fn test_query_facts() {
    let (storage, index) = setup();
    let store = SemanticStore::new(&storage, &index);

    store.add_fact("Rust", "is", "fast", 0.9, MemSource::new("cli"), None).unwrap();
    store.add_fact("Python", "is", "dynamic", 0.8, MemSource::new("cli"), None).unwrap();
    store.add_fact("Rust", "has", "ownership", 0.95, MemSource::new("cli"), None).unwrap();

    let results = store.query_facts("Rust").unwrap();
    assert_eq!(results.len(), 2);
}

#[test]
fn test_query_preferences() {
    let (storage, index) = setup();
    let store = SemanticStore::new(&storage, &index);

    store.add_preference("editor", "neovim", 0.9).unwrap();
    store.add_preference("editor.theme", "dark", 0.8).unwrap();
    store.add_preference("language", "rust", 0.7).unwrap();

    let results = store.query_preferences("editor").unwrap();
    assert_eq!(results.len(), 2);
}

#[test]
fn test_update_confidence() {
    let (storage, index) = setup();
    let store = SemanticStore::new(&storage, &index);

    let mem = store.add_preference("color", "blue", 0.5).unwrap();
    store.update_confidence(mem.id, 0.95).unwrap();

    use cortex_core::storage::traits::StorageBackend;
    let updated = storage.get_memory(mem.id).unwrap().unwrap();
    assert!((updated.salience.base_score - 0.95).abs() < 0.01);
}

#[test]
fn test_find_contradictions() {
    let (storage, index) = setup();
    let store = SemanticStore::new(&storage, &index);

    store.add_fact("User", "lives_in", "San Francisco", 0.8, MemSource::new("cli"), None).unwrap();
    store.add_fact("User", "lives_in", "New York", 0.6, MemSource::new("cli"), None).unwrap();

    let contradictions = store.find_contradictions("User", "lives_in", "San Francisco").unwrap();
    assert_eq!(contradictions.len(), 1);
    match &contradictions[0].0.content {
        MemContent::Fact { object, .. } => assert_eq!(object, "New York"),
        _ => panic!("Expected Fact"),
    }
}

#[test]
fn test_merge_facts() {
    let (storage, index) = setup();
    let store = SemanticStore::new(&storage, &index);

    let old = store.add_fact("User", "works_at", "Company A", 0.9, MemSource::new("cli"), None).unwrap();
    let new = store.add_fact("User", "works_at", "Company B", 0.85, MemSource::new("cli"), None).unwrap();

    store.merge_facts(old.id, new.id).unwrap();

    use cortex_core::storage::traits::StorageBackend;
    let old_updated = storage.get_memory(old.id).unwrap().unwrap();
    // Old fact should have reduced salience
    assert!(old_updated.salience.base_score < 0.9);

    let links = storage.get_links(new.id).unwrap();
    assert_eq!(links.len(), 1);
}

#[test]
fn test_search_similar() {
    let (storage, index) = setup();
    let store = SemanticStore::new(&storage, &index);

    let emb1 = vec![1.0, 0.0, 0.0];
    let emb2 = vec![0.0, 1.0, 0.0];

    store.add_fact("A", "is", "X", 0.9, MemSource::new("cli"), Some(emb1)).unwrap();
    store.add_fact("B", "is", "Y", 0.8, MemSource::new("cli"), Some(emb2)).unwrap();

    let results = store.search_similar(&[0.9, 0.1, 0.0], 1).unwrap();
    assert_eq!(results.len(), 1);
    match &results[0].content {
        MemContent::Fact { subject, .. } => assert_eq!(subject, "A"),
        _ => panic!("Expected Fact"),
    }
}
